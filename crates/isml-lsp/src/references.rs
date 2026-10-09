//! Find All References for a function a cartridge script exports: every `require` and
//! `module.superModule` that reaches it, along the cartridge path of each storefront.
//!
//! Read from the text, not from a JavaScript parser: a module bound to a variable, required
//! inline or destructured is followed; one passed around or reassigned is not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::reference::Reference;
use crate::resolve::{self, Override};
use crate::script::*;
use crate::workspace::{Cartridge, Workspace};

/// One identifier, in UTF-16 columns as LSP counts them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    /// Zero-based.
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

pub struct Query<'a> {
    pub file: &'a Path,
    pub text: &'a str,
    /// Character offset of the cursor into `text`.
    pub offset: usize,
    pub include_declaration: bool,
}

/// Where the function under the cursor is used, its definition first when asked for.
/// `open` holds the editor's unsaved text, which wins over the file on disk.
pub fn find(query: &Query, workspace: &Workspace, open: &HashMap<PathBuf, &str>) -> Vec<Found> {
    let chars = code(query.file, query.text);
    let Some((name, files)) = targets_at(&chars, query, workspace) else {
        return Vec::new();
    };
    let targets: Vec<Target> = files
        .iter()
        .filter_map(|file| Target::new(file, &name, workspace, open))
        .collect();

    let mut found = Vec::new();
    if query.include_declaration {
        for target in &targets {
            if let Some(text) = read(&target.path, open) {
                let chars = code(&target.path, &text);
                let definitions = exports(&chars, &name).definitions;
                found.extend(located(&target.path, &chars, &definitions, &name));
            }
        }
    }
    // Opening thousands of files is most of the cost, so the scan is split across threads.
    let sources = workspace.sources();
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
    let chunk = sources.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = sources
            .chunks(chunk)
            .map(|files| {
                let (name, targets) = (&name, &targets);
                scope.spawn(move || {
                    files
                        .iter()
                        .flat_map(|source| uses_in(source, name, targets, workspace, open))
                        .collect::<Vec<Found>>()
                })
            })
            .collect();
        for worker in workers {
            found.extend(worker.join().unwrap_or_default());
        }
    });
    let mut unique = Vec::new();
    for hit in found {
        if !unique.contains(&hit) {
            unique.push(hit);
        }
    }
    unique
}

fn uses_in(
    source: &Path,
    name: &str,
    targets: &[Target],
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Found> {
    let Some(text) = read(source, open) else {
        return Vec::new();
    };
    if !text.contains(name) {
        return Vec::new();
    }
    let chars = code(source, &text);
    let referrer = workspace.cartridge_of(source);
    let mut positions = uses(&chars, name, |origin| {
        targets
            .iter()
            .any(|target| target.reached_by(origin, source, referrer, workspace))
    });
    if targets.iter().any(|target| target.is_file(source)) {
        positions.extend(local_uses(&chars, name));
    }
    located(source, &chars, &positions, name)
}

/// The name under the cursor and the files whose export of it is meant: the file itself
/// when the cursor is on the definition, what the receiver resolves to when it is on a use.
fn targets_at(
    chars: &[char],
    query: &Query,
    workspace: &Workspace,
) -> Option<(String, Vec<PathBuf>)> {
    let at = member_at(chars, query.offset)?;
    let files = match &at.member {
        Member::Here => vec![query.file.to_path_buf()],
        Member::Of(origin) => origin_files(origin, query.file, workspace),
    };
    Some((at.name, files))
}

fn origin_files(origin: &Origin, file: &Path, workspace: &Workspace) -> Vec<PathBuf> {
    match origin {
        Origin::Require(spec) => {
            resolve::resolve(&Reference::Module(spec.clone()), file, workspace)
                .into_iter()
                .map(|hit| hit.path)
                .collect()
        }
        Origin::Super => parents(file, workspace),
    }
}

/// What `module.superModule` is in `file`: the next copy to its right in each path that
/// holds its cartridge, or every other copy when no path does.
fn parents(file: &Path, workspace: &Workspace) -> Vec<PathBuf> {
    let Some(cartridge) = workspace.cartridge_of(file) else {
        return Vec::new();
    };
    let Ok(relative) = file.strip_prefix(&cartridge.root) else {
        return Vec::new();
    };
    let others: Vec<Override> = resolve::path_modules(&relative.to_string_lossy(), file, workspace)
        .into_iter()
        .filter(|copy| copy.cartridge != cartridge.name)
        .collect();
    let holding: Vec<_> = workspace
        .paths
        .iter()
        .filter_map(|path| path.rank(&cartridge.name).map(|rank| (path, rank)))
        .collect();
    if holding.is_empty() {
        return others.into_iter().map(|copy| copy.hit.path).collect();
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for (path, own) in holding {
        let next = others
            .iter()
            .filter_map(|copy| path.rank(&copy.cartridge).map(|rank| (rank, copy)))
            .filter(|(rank, _)| *rank > own)
            .min_by_key(|(rank, _)| *rank);
        if let Some((_, copy)) = next {
            if !files.contains(&copy.hit.path) {
                files.push(copy.hit.path.clone());
            }
        }
    }
    files
}

/// One file whose export is being looked for, with every cartridge's copy of it: a copy
/// left of it that defines the same name hides it from whatever reaches that copy first.
struct Target {
    path: PathBuf,
    cartridge: Cartridge,
    relative: PathBuf,
    /// Each copy, and whether it defines the name itself rather than passing the parent's on.
    copies: Vec<(Override, bool)>,
}

impl Target {
    fn new(
        path: &Path,
        name: &str,
        workspace: &Workspace,
        open: &HashMap<PathBuf, &str>,
    ) -> Option<Target> {
        let cartridge = workspace.cartridge_of(path)?.clone();
        let relative = path.strip_prefix(&cartridge.root).ok()?.to_path_buf();
        let copies = resolve::path_modules(&relative.to_string_lossy(), path, workspace)
            .into_iter()
            .map(|copy| {
                let defines = read(&copy.hit.path, open)
                    .is_some_and(|text| exports(&code(&copy.hit.path, &text), name).exported);
                (copy, defines)
            })
            .collect();
        Some(Target {
            path: path.to_path_buf(),
            cartridge,
            relative,
            copies,
        })
    }

    fn reached_by(
        &self,
        origin: &Origin,
        source: &Path,
        referrer: Option<&Cartridge>,
        workspace: &Workspace,
    ) -> bool {
        match origin {
            Origin::Super => referrer.is_some_and(|referrer| {
                referrer.name != self.cartridge.name
                    && source.strip_prefix(&referrer.root).ok() == Some(self.relative.as_path())
                    && self.along_paths(&referrer.name, true, workspace)
            }),
            Origin::Require(spec) => {
                if spec.starts_with("./") || spec.starts_with("../") {
                    let base = source.parent().unwrap_or(source);
                    return self.is(&base.join(spec));
                }
                if let Some(rest) = spec.strip_prefix("*/") {
                    let referrer = referrer.map_or("", |cartridge| cartridge.name.as_str());
                    return self.is(&self.cartridge.root.join(rest))
                        && self.along_paths(referrer, false, workspace);
                }
                if let Some(rest) = spec.strip_prefix("~/") {
                    return referrer.is_some_and(|referrer| referrer.name == self.cartridge.name)
                        && self.is(&self.cartridge.root.join(rest));
                }
                match spec.split_once('/') {
                    Some((cartridge, rest)) => {
                        cartridge == self.cartridge.name && self.is(&self.cartridge.root.join(rest))
                    }
                    None => false,
                }
            }
        }
    }

    fn is(&self, candidate: &Path) -> bool {
        resolve::existing_module(candidate).is_some_and(|module| self.is_file(&module))
    }

    fn is_file(&self, path: &Path) -> bool {
        same_file(path, &self.path)
    }

    /// Whether some path running `referrer` reaches this copy: walking it from the start —
    /// or, for `module.superModule`, from just right of the referrer — no copy before this
    /// one defines the name. With no order recorded for either cartridge, it might.
    fn along_paths(&self, referrer: &str, after_referrer: bool, workspace: &Workspace) -> bool {
        if workspace
            .paths
            .iter()
            .all(|path| path.rank(&self.cartridge.name).is_none())
        {
            return true;
        }
        let running: Vec<_> = workspace
            .paths
            .iter()
            .filter(|path| path.rank(referrer).is_some())
            .collect();
        if running.is_empty() {
            return true;
        }
        running.into_iter().any(|path| {
            let Some(own) = path.rank(&self.cartridge.name) else {
                return false;
            };
            let start = match (after_referrer, path.rank(referrer)) {
                (true, Some(rank)) => rank + 1,
                _ => 0,
            };
            own >= start
                && !self.copies.iter().any(|(copy, defines)| {
                    *defines
                        && path
                            .rank(&copy.cartridge)
                            .is_some_and(|rank| rank >= start && rank < own)
                })
        })
    }
}

/// Uses of `name` through every require or superModule in the file that `reaches` accepts.
fn uses(chars: &[char], name: &str, reaches: impl Fn(&Origin) -> bool) -> Vec<usize> {
    let mut positions = Vec::new();
    let mut verdicts: HashMap<Origin, bool> = HashMap::new();
    for span in spans(chars) {
        let reached = *verdicts
            .entry(span.origin.clone())
            .or_insert_with(|| reaches(&span.origin));
        if !reached {
            continue;
        }
        if let Some(at) = member_after(chars, span.end, name) {
            positions.push(at);
        }
        match &span.bound {
            Bound::Alias(alias) => {
                for at in words(chars, alias) {
                    if at > 0 && chars[at - 1] == '.' {
                        continue;
                    }
                    let after = at + alias.chars().count();
                    if chars.get(after) != Some(&'.') {
                        continue;
                    }
                    if let Some(member) = member_after(chars, after, name) {
                        positions.push(member);
                    }
                }
            }
            Bound::Destructured(entries) => {
                for entry in entries.iter().filter(|entry| entry.key == name) {
                    positions.push(entry.key_at);
                    positions.extend(
                        words(chars, &entry.local)
                            .into_iter()
                            .filter(|at| *at >= span.end && is_bare(chars, *at, &entry.local)),
                    );
                }
            }
            Bound::Nothing => {}
        }
    }
    positions
}

/// `.name` right after `end`, unless it is assigned: that is a definition, not a use.
fn member_after(chars: &[char], end: usize, name: &str) -> Option<usize> {
    let dot = skip_space(chars, end);
    if chars.get(dot) != Some(&'.') {
        return None;
    }
    let at = skip_space(chars, dot + 1);
    let member = ident_at(chars, at)?;
    (member == name && !is_assignment(chars, at + member.chars().count())).then_some(at)
}

/// A plain call or mention of `name` inside the file that defines it.
fn local_uses(chars: &[char], name: &str) -> Vec<usize> {
    let exported = exports(chars, name);
    let assigned: Vec<usize> = exported
        .definitions
        .iter()
        .filter_map(|at| assigned_value(chars, at + name.chars().count()))
        .collect();
    words(chars, name)
        .into_iter()
        .filter(|at| !exported.definitions.contains(at) && !assigned.contains(at))
        .filter(|at| {
            !exported
                .literal
                .is_some_and(|(open, close)| *at > open && *at < close)
        })
        .filter(|at| is_bare(chars, *at, name))
        .filter(|at| {
            let before = skip_space_back(chars, *at);
            !matches!(
                ident_ending_at(chars, before)
                    .map(|(_, word)| word)
                    .as_deref(),
                Some("function" | "var" | "let" | "const")
            )
        })
        .collect()
}

/// Where the value starts in `exports.name = value`.
fn assigned_value(chars: &[char], after: usize) -> Option<usize> {
    let equals = skip_space(chars, after);
    is_assignment(chars, equals).then(|| skip_space(chars, equals + 1))
}

/// Neither a member of something else nor an object key.
fn is_bare(chars: &[char], at: usize, name: &str) -> bool {
    let before = skip_space_back(chars, at);
    if before > 0 && chars[before - 1] == '.' {
        return false;
    }
    let after = skip_space(chars, at + name.chars().count());
    chars.get(after) != Some(&':')
}

fn located(path: &Path, chars: &[char], positions: &[usize], name: &str) -> Vec<Found> {
    let mut sorted = positions.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let width: u32 = name.chars().map(|c| c.len_utf16() as u32).sum();
    let mut found = Vec::new();
    let (mut line, mut column, mut index) = (0u32, 0u32, 0usize);
    for at in sorted {
        while index < at && index < chars.len() {
            if chars[index] == '\n' {
                line += 1;
                column = 0;
            } else {
                column += chars[index].len_utf16() as u32;
            }
            index += 1;
        }
        found.push(Found {
            path: path.to_path_buf(),
            line,
            start: column,
            end: column + width,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const HELPER: &str = "cartridge/scripts/helpers/cartHelpers.js";
    const CART: &str = "cartridge/controllers/Cart.js";
    const REQUIRE: &str = "require('*/cartridge/scripts/helpers/cartHelpers')";

    const DEFINES: &str = "'use strict';\nfunction total(basket) {\n    return basket;\n}\nfunction other() {\n    return total(null);\n}\nmodule.exports = {\n    total: total,\n    other: other\n};\n";
    const REDEFINES: &str = "var base = module.superModule;\nbase.total = function (basket) {\n    return base.total(basket);\n};\nmodule.exports = base;\n";
    const PASSES_ON: &str =
        "var base = module.superModule;\nmodule.exports = {\n    total: base.total\n};\n";

    /// `site_a` runs `app_na:app_brand:app_storefront_base`, `site_b` leaves `app_na` out.
    fn checkout(name: &str, files: &[(&str, &str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&root);
        for (cartridge, file, text) in files {
            let path = root.join("cartridges").join(cartridge).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
        }
        root
    }

    fn workspace(root: &Path) -> Workspace {
        let settings = serde_json::json!({
            "cartridge_path": {
                "site_a": "app_na:app_brand:app_storefront_base",
                "site_b": "app_brand:app_storefront_base",
            }
        });
        Workspace::scan(&[root.to_path_buf()], &settings)
    }

    /// References with the cursor on the first `at` in the file, as `cartridge line`.
    fn references(workspace: &Workspace, root: &Path, file: (&str, &str), at: &str) -> Vec<String> {
        let path = root.join("cartridges").join(file.0).join(file.1);
        let text = fs::read_to_string(&path).unwrap();
        let offset = text[..text.find(at).unwrap()].chars().count();
        let query = Query {
            file: &path,
            text: &text,
            offset,
            include_declaration: false,
        };
        let mut found: Vec<String> = find(&query, workspace, &HashMap::new())
            .into_iter()
            .map(|hit| {
                let relative = hit.path.strip_prefix(root.join("cartridges")).unwrap();
                let cartridge = relative.iter().next().unwrap().to_string_lossy();
                format!("{cartridge} {}", hit.line)
            })
            .collect();
        found.sort();
        found
    }

    fn uses() -> String {
        format!(
            "var helpers = {REQUIRE};\nhelpers.total(1);\n{REQUIRE}.total(2);\nvar {{ total }} = {REQUIRE};\ntotal(3);\n// helpers.total(4);\nhelpers.totalPrice(5);\n"
        )
    }

    #[test]
    fn finds_every_way_of_importing_the_function() {
        let cart = uses();
        let template = format!("<isprint value=\"${{{REQUIRE}.total(pdict.basket)}}\"/>\n");
        let root = checkout(
            "isml-lsp-references-imports",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", CART, &cart),
                (
                    "app_brand",
                    "cartridge/templates/default/cart.isml",
                    &template,
                ),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found,
            [
                "app_brand 0",
                "app_brand 1",
                "app_brand 2",
                "app_brand 3",
                "app_brand 4",
                "app_storefront_base 5"
            ]
        );
    }

    #[test]
    fn answers_the_same_from_a_use_as_from_the_definition() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-from-use",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", CART, &cart),
            ],
        );
        let workspace = workspace(&root);
        let from_definition = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total: total",
        );
        let from_use = references(&workspace, &root, ("app_brand", CART), "total(1)");
        assert_eq!(from_use, from_definition);
    }

    #[test]
    fn an_override_that_redefines_the_function_hides_the_parent() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-hidden",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", CART, &cart),
            ],
        );
        let workspace = workspace(&root);
        // Only the override's own call to its parent, and the base's internal call.
        let found = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(found, ["app_brand 2", "app_storefront_base 5"]);
        // The callers reach the override instead.
        let found = references(&workspace, &root, ("app_brand", HELPER), "total = function");
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            4
        );
    }

    #[test]
    fn an_override_that_passes_the_function_on_does_not_hide_it() {
        let cart = uses();
        let root = checkout(
            "isml-lsp-references-passes-on",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_na", HELPER, PASSES_ON),
                ("app_brand", CART, &cart),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            4
        );
        // `base.total` in the override hands the parent's on: a use of it.
        assert!(found.contains(&"app_na 2".to_string()));
    }

    #[test]
    fn counts_a_caller_only_in_the_storefronts_that_run_it() {
        let na_caller = format!("{REQUIRE}.total(1);\n");
        let root = checkout(
            "isml-lsp-references-storefront",
            &[
                ("app_brand", HELPER, DEFINES),
                ("app_na", HELPER, REDEFINES),
                ("app_na", "cartridge/scripts/caller.js", &na_caller),
                ("app_brand", CART, &uses()),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_brand", HELPER),
            "total(basket)",
        );
        // The brand cart reaches it through site_b; app_na runs only in site_a, where its
        // own copy answers, so its caller does not — but its call to the parent does.
        assert!(!found.contains(&"app_na 0".to_string()));
        assert!(found.contains(&"app_na 2".to_string()));
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            5
        );
    }

    #[test]
    fn a_tilde_require_names_its_own_cartridge() {
        let caller = "require('~/cartridge/scripts/helpers/cartHelpers').total(1);\n";
        let root = checkout(
            "isml-lsp-references-tilde",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", "cartridge/scripts/caller.js", caller),
                ("app_storefront_base", "cartridge/scripts/caller.js", caller),
            ],
        );
        let found = references(
            &workspace(&root),
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert!(found.contains(&"app_storefront_base 0".to_string()));
        assert!(!found.contains(&"app_brand 0".to_string()));
    }

    #[test]
    fn counts_every_copy_when_no_cartridge_path_is_recorded() {
        let root = checkout(
            "isml-lsp-references-no-path",
            &[
                ("app_storefront_base", HELPER, DEFINES),
                ("app_brand", HELPER, REDEFINES),
                ("app_brand", CART, &uses()),
            ],
        );
        let workspace = Workspace::scan(std::slice::from_ref(&root), &serde_json::Value::Null);
        let found = references(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total(basket)",
        );
        assert_eq!(
            found
                .iter()
                .filter(|hit| hit.starts_with("app_brand"))
                .count(),
            5
        );
    }

    #[test]
    fn includes_the_definition_when_asked() {
        let root = checkout(
            "isml-lsp-references-declaration",
            &[("app_storefront_base", HELPER, DEFINES)],
        );
        let path = root.join("cartridges/app_storefront_base").join(HELPER);
        let query = Query {
            file: &path,
            text: DEFINES,
            offset: DEFINES.find("total: total").unwrap(),
            include_declaration: true,
        };
        let lines: Vec<(u32, u32)> = find(&query, &workspace(&root), &HashMap::new())
            .into_iter()
            .map(|hit| (hit.line, hit.start))
            .collect();
        assert_eq!(lines, [(1, 9), (8, 4), (5, 11)]);
    }

    #[test]
    fn the_value_an_export_is_assigned_is_not_a_use() {
        let helper = "var parent = module.superModule;\nfunction total() {}\nmodule.exports = parent;\nmodule.exports.total = total;\n";
        let root = checkout(
            "isml-lsp-references-assigned",
            &[("app_brand", HELPER, helper)],
        );
        let found = references(&workspace(&root), &root, ("app_brand", HELPER), "total()");
        assert!(found.is_empty(), "{found:?}");
    }
}
