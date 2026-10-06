//! The function behind `helpers.total`: Go to Definition and hover on a member of a
//! cartridge module, along the cartridge path. A copy that passes its parent's function on
//! (`module.exports = base`) sends the search on to the next copy to its right.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::Url;

use crate::reference::Reference;
use crate::resolve;
use crate::script::*;
use crate::workspace::Workspace;

/// Where a member is defined, and what the definition says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The storefronts that run this definition; empty when no path is recorded.
    pub storefronts: Vec<String>,
    pub cartridge: String,
    pub path: PathBuf,
    /// Zero-based, of the name.
    pub line: u32,
    /// UTF-16 columns of the name.
    pub start: u32,
    pub end: u32,
    /// `total(basket, options)`, when the definition is a function written out there.
    pub signature: Option<String>,
    /// The `/** ... */` block above the definition, without its stars.
    pub doc: Option<String>,
}

/// What the member at `offset` resolves to, one definition per distinct file: the chosen
/// storefront's only, when `storefront` is set and that storefront has one.
pub fn at(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Definition> {
    let chars = code(file, text);
    let Some(MemberAt {
        name,
        member: Member::Of(origin),
    }) = member_at(&chars, offset)
    else {
        return Vec::new();
    };
    let chains = chains(&origin, file, workspace);
    resolved(&chains, &name, workspace, open)
}

/// A member a module offers after `helpers.`, with where it is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub name: String,
    pub definitions: Vec<Definition>,
}

/// What the module before the dot at `offset` exports, as the cartridge path builds it:
/// each copy's own names, then its parent's while it inherits them. With the number of
/// characters of the member already typed.
pub fn offered(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<(Vec<Offered>, usize)> {
    let chars = code(file, text);
    let (origin, typed) = pending_member(&chars, offset)?;
    let mut chains = chains(&origin, file, workspace);
    if let Some(chosen) = &workspace.storefront {
        if chains
            .iter()
            .any(|(label, _)| label.as_ref() == Some(chosen))
        {
            chains.retain(|(label, _)| label.as_ref() == Some(chosen));
        }
    }

    // Every copy is read once, however many names it is asked about.
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    for path in chains.iter().flat_map(|(_, chain)| chain) {
        if !texts.contains_key(path) {
            if let Some(text) = read(path, open) {
                texts.insert(path.clone(), text);
            }
        }
    }
    let loaded: HashMap<PathBuf, &str> = texts
        .iter()
        .map(|(path, text)| (path.clone(), text.as_str()))
        .collect();

    let mut names: Vec<String> = Vec::new();
    for (_, chain) in &chains {
        for path in chain {
            let Some(text) = loaded.get(path) else {
                break;
            };
            let chars = code(path, text);
            for name in exported_names(&chars) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            if !inherits_all(&chars) {
                break;
            }
        }
    }
    let offered = names
        .into_iter()
        .map(|name| Offered {
            definitions: resolved(&chains, &name, workspace, &loaded),
            name,
        })
        .filter(|offered| !offered.definitions.is_empty())
        .collect();
    Some((offered, typed))
}

/// The definition each chain reaches, one per distinct file, labelled with the storefronts
/// that reach it: the chosen storefront's only, when it has one.
fn resolved(
    chains: &[(Option<String>, Vec<PathBuf>)],
    name: &str,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Definition> {
    let mut found: Vec<Definition> = Vec::new();
    for (storefront, chain) in chains {
        let storefront = storefront.clone();
        let Some(mut definition) = defined_along(chain, name, workspace, open) else {
            continue;
        };
        if let (Some(chosen), Some(label)) = (&workspace.storefront, &storefront) {
            if chosen == label {
                definition.storefronts = vec![label.clone()];
                return vec![definition];
            }
        }
        match found
            .iter_mut()
            .find(|seen| seen.path == definition.path && seen.line == definition.line)
        {
            Some(seen) => seen.storefronts.extend(storefront),
            None => {
                definition.storefronts = storefront.into_iter().collect();
                found.push(definition);
            }
        }
    }
    found
}

/// The hover for a member: signature and documentation of each definition, and where it is.
pub fn markdown(definitions: &[Definition]) -> Option<String> {
    let sections: Vec<String> = definitions
        .iter()
        .map(|definition| {
            let mut section = String::new();
            if let Some(signature) = &definition.signature {
                section.push_str(&format!("```js\n{signature}\n```\n\n"));
            }
            if let Some(doc) = &definition.doc {
                section.push_str(&doc_markdown(doc));
                section.push_str("\n\n");
            }
            let place = match Url::from_file_path(&definition.path) {
                Ok(url) => format!("[`{}`]({url})", definition.cartridge),
                Err(()) => format!("`{}`", definition.cartridge),
            };
            section.push_str(&format!("Defined in {place}"));
            if !definition.storefronts.is_empty() {
                let storefronts: Vec<String> = definition
                    .storefronts
                    .iter()
                    .map(|label| format!("`{label}`"))
                    .collect();
                section.push_str(&format!(", run by {}", storefronts.join(", ")));
            }
            section
        })
        .collect();
    (!sections.is_empty()).then(|| sections.join("\n\n---\n\n"))
}

/// The copies a member could come from, in the order they are asked, per storefront.
fn chains(
    origin: &Origin,
    file: &Path,
    workspace: &Workspace,
) -> Vec<(Option<String>, Vec<PathBuf>)> {
    match origin {
        Origin::Require(spec) => match spec.strip_prefix("*/") {
            Some(rest) => from_path_start(rest, file, workspace),
            None => resolve::resolve(&Reference::Module(spec.clone()), file, workspace)
                .into_iter()
                .take(1)
                .flat_map(|hit| {
                    after(&hit.path, workspace)
                        .into_iter()
                        .map(move |(label, rest)| {
                            let mut chain = vec![hit.path.clone()];
                            chain.extend(rest);
                            (label, chain)
                        })
                })
                .collect(),
        },
        Origin::Super => after(file, workspace),
    }
}

/// A `*/` module: every copy, leftmost first, in each path that runs the requiring file.
/// With no path recorded, each copy on its own: the order is not knowable.
fn from_path_start(
    rest: &str,
    file: &Path,
    workspace: &Workspace,
) -> Vec<(Option<String>, Vec<PathBuf>)> {
    let copies = resolve::path_modules(rest, file, workspace);
    if workspace.paths.is_empty() {
        return copies
            .into_iter()
            .map(|copy| (None, vec![copy.hit.path]))
            .collect();
    }
    let referrer = workspace
        .cartridge_of(file)
        .map(|cartridge| cartridge.name.as_str());
    let mut running: Vec<_> = workspace
        .paths
        .iter()
        .filter(|path| referrer.is_some_and(|name| path.rank(name).is_some()))
        .collect();
    if running.is_empty() {
        running = workspace.paths.iter().collect();
    }
    running
        .into_iter()
        .map(|path| {
            let mut ranked: Vec<_> = copies
                .iter()
                .filter_map(|copy| path.rank(&copy.cartridge).map(|rank| (rank, copy)))
                .collect();
            ranked.sort_by_key(|(rank, _)| *rank);
            let chain = ranked
                .into_iter()
                .map(|(_, copy)| copy.hit.path.clone())
                .collect();
            (Some(path.label.clone()), chain)
        })
        .collect()
}

/// The copies to the right of `file`'s cartridge, nearest first, in each path holding it:
/// where `module.superModule` leads. With none holding it, every other copy, unordered.
fn after(file: &Path, workspace: &Workspace) -> Vec<(Option<String>, Vec<PathBuf>)> {
    let Some(cartridge) = workspace.cartridge_of(file) else {
        return vec![(None, Vec::new())];
    };
    let Ok(relative) = file.strip_prefix(&cartridge.root) else {
        return vec![(None, Vec::new())];
    };
    let others: Vec<_> = resolve::path_modules(&relative.to_string_lossy(), file, workspace)
        .into_iter()
        .filter(|copy| copy.cartridge != cartridge.name)
        .collect();
    let holding: Vec<_> = workspace
        .paths
        .iter()
        .filter_map(|path| path.rank(&cartridge.name).map(|rank| (path, rank)))
        .collect();
    if holding.is_empty() {
        let chain = others.into_iter().map(|copy| copy.hit.path).collect();
        return vec![(None, chain)];
    }
    holding
        .into_iter()
        .map(|(path, own)| {
            let mut ranked: Vec<_> = others
                .iter()
                .filter_map(|copy| path.rank(&copy.cartridge).map(|rank| (rank, copy)))
                .filter(|(rank, _)| *rank > own)
                .collect();
            ranked.sort_by_key(|(rank, _)| *rank);
            let chain = ranked
                .into_iter()
                .map(|(_, copy)| copy.hit.path.clone())
                .collect();
            (Some(path.label.clone()), chain)
        })
        .collect()
}

/// The first copy along the chain that defines `name`, as long as every copy before it
/// passes its parent's on; a copy that does neither ends the search.
fn defined_along(
    chain: &[PathBuf],
    name: &str,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<Definition> {
    for path in chain {
        let text = read(path, open)?;
        let chars = code(path, &text);
        let exported = exports(&chars, name);
        if exported.exported {
            let original: Vec<char> = text.chars().collect();
            return Some(definition(
                path,
                &chars,
                &original,
                &exported.definitions,
                name,
                workspace,
            ));
        }
        if !inherits(&chars, name) {
            return None;
        }
    }
    None
}

fn definition(
    path: &Path,
    chars: &[char],
    original: &[char],
    definitions: &[usize],
    name: &str,
    workspace: &Workspace,
) -> Definition {
    // The declaration carries the documentation; `total: total` only names it.
    let at = declaration(chars, name)
        .or_else(|| definitions.iter().min().copied())
        .unwrap_or(0);
    let (line, start) = line_column(chars, at);
    let width: u32 = name.chars().map(|c| c.len_utf16() as u32).sum();
    Definition {
        storefronts: Vec::new(),
        cartridge: workspace
            .cartridge_of(path)
            .map(|cartridge| cartridge.name.clone())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        line,
        start,
        end: start + width,
        signature: signature(chars, at, name),
        doc: doc_above(original, at),
    }
}

/// `function name`, or `var name =` / `let` / `const`.
fn declaration(chars: &[char], name: &str) -> Option<usize> {
    words(chars, name).into_iter().find(|at| {
        let before = skip_space_back(chars, *at);
        let keyword = ident_ending_at(chars, before).map(|(_, word)| word);
        match keyword.as_deref() {
            Some("function") => true,
            Some("var" | "let" | "const") => {
                let value = skip_space(chars, at + name.chars().count());
                is_assignment(chars, value)
            }
            _ => false,
        }
    })
}

/// `name(params)` from the function the definition at `at` names, when it is written there:
/// `function name(`, `name = function (`, `name: function (`, `name(` or an arrow.
fn signature(chars: &[char], at: usize, name: &str) -> Option<String> {
    let mut index = skip_space(chars, at + name.chars().count());
    if matches!(chars.get(index), Some('=' | ':')) {
        index = skip_space(chars, index + 1);
        for keyword in ["async", "function"] {
            if ident_at(chars, index).as_deref() == Some(keyword) {
                index = skip_space(chars, index + keyword.len());
            }
        }
        if let Some(named) = ident_at(chars, index) {
            index = skip_space(chars, index + named.chars().count());
        }
    }
    if chars.get(index) != Some(&'(') {
        return None;
    }
    let close = matching_brace(chars, index)?;
    let params: String = chars[index + 1..close].iter().collect();
    let params = params.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(format!("{name}({params})"))
}

/// The `/** */` block that ends just before the line holding `at`, stars and indentation
/// stripped. A plain `/* */` or `//` comment is not documentation.
fn doc_above(chars: &[char], at: usize) -> Option<String> {
    let line_start = chars[..at.min(chars.len())]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |index| index + 1);
    let end = skip_space_back(chars, line_start);
    if end < 2 || chars[end - 2..end] != ['*', '/'] {
        return None;
    }
    let opening: Vec<char> = "/**".chars().collect();
    let start = (0..end.saturating_sub(2))
        .rev()
        .find(|index| chars[*index..].starts_with(&opening))?;
    let body: String = chars[start + 3..end - 2].iter().collect();
    let lines: Vec<String> = body
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let stripped = trimmed.strip_prefix('*').unwrap_or(trimmed);
            stripped.strip_prefix(' ').unwrap_or(stripped).to_string()
        })
        .collect();
    let text = lines.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The description as it is; `@param`, `@returns` and the other tags one per line.
fn doc_markdown(doc: &str) -> String {
    let mut description: Vec<&str> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    for line in doc.lines() {
        match line.strip_prefix('@') {
            Some(tag) => {
                let (name, rest) = tag.split_once(' ').unwrap_or((tag, ""));
                tags.push(format!("*@{name}* {}", rest.trim()).trim_end().to_string());
            }
            None if tags.is_empty() => description.push(line),
            // A tag's description continuing on the next line.
            None => {
                if let Some(last) = tags.last_mut() {
                    last.push(' ');
                    last.push_str(line.trim());
                }
            }
        }
    }
    let mut out = description.join("\n").trim().to_string();
    if !tags.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&tags.join("  \n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const HELPER: &str = "cartridge/scripts/helpers/cartHelpers.js";
    const CART: &str = "cartridge/controllers/Cart.js";
    const CALLER: &str = "var helpers = require('*/cartridge/scripts/helpers/cartHelpers');\nhelpers.total(1);\nvar { total: sum } = require('*/cartridge/scripts/helpers/cartHelpers');\nsum(2);\n";

    const DOCUMENTED: &str = "'use strict';\n\n/**\n * Adds up the basket.\n * @param {dw.order.Basket} basket - the basket\n * @returns {number} the total\n */\nfunction total(basket, options) {\n    return 0;\n}\n\nmodule.exports = {\n    total: total\n};\n";
    const REDEFINES: &str = "var base = module.superModule;\n/** The brand's total. */\nbase.total = function (basket) {\n    return base.total(basket);\n};\nmodule.exports = base;\n";
    const PASSES_ON: &str =
        "var base = module.superModule;\nbase.other = function () {};\nmodule.exports = base;\n";

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

    fn workspace(root: &Path, storefront: Option<&str>) -> Workspace {
        let mut settings = serde_json::json!({
            "cartridge_path": {
                "site_a": "app_na:app_brand:app_storefront_base",
                "site_b": "app_brand:app_storefront_base",
            }
        });
        if let Some(storefront) = storefront {
            settings["storefront"] = storefront.into();
        }
        Workspace::scan(&[root.to_path_buf()], &settings)
    }

    /// The definitions of the member at the first `at` in the file, as `cartridge line storefronts`.
    fn definitions(
        workspace: &Workspace,
        root: &Path,
        file: (&str, &str),
        at: &str,
    ) -> Vec<String> {
        let path = root.join("cartridges").join(file.0).join(file.1);
        let text = fs::read_to_string(&path).unwrap();
        let offset = text[..text.find(at).unwrap()].chars().count();
        self::at(&path, &text, offset, workspace, &HashMap::new())
            .into_iter()
            .map(|definition| {
                format!(
                    "{} {} {}",
                    definition.cartridge,
                    definition.line,
                    definition.storefronts.join(",")
                )
            })
            .collect()
    }

    #[test]
    fn finds_the_definition_each_storefront_runs() {
        let root = checkout(
            "isml-lsp-members-per-storefront",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, REDEFINES),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = definitions(
            &workspace(&root, None),
            &root,
            ("app_brand", CART),
            "total(1)",
        );
        assert_eq!(found, ["app_na 2 site_a", "app_storefront_base 7 site_b"]);
    }

    #[test]
    fn answers_only_for_the_chosen_storefront() {
        let root = checkout(
            "isml-lsp-members-chosen",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, REDEFINES),
                ("app_brand", CART, CALLER),
            ],
        );
        let workspace = workspace(&root, Some("site_a"));
        let found = definitions(&workspace, &root, ("app_brand", CART), "total(1)");
        assert_eq!(found, ["app_na 2 site_a"]);
    }

    #[test]
    fn follows_a_copy_that_passes_its_parent_on() {
        let root = checkout(
            "isml-lsp-members-passes-on",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, PASSES_ON),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = definitions(
            &workspace(&root, None),
            &root,
            ("app_brand", CART),
            "total(1)",
        );
        assert_eq!(found, ["app_storefront_base 7 site_a,site_b"]);
    }

    #[test]
    fn resolves_a_destructured_name_and_a_parent_call() {
        let root = checkout(
            "isml-lsp-members-destructured",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, REDEFINES),
                ("app_brand", CART, CALLER),
            ],
        );
        let workspace = workspace(&root, None);
        let found = definitions(&workspace, &root, ("app_brand", CART), "sum(2)");
        assert_eq!(found, ["app_na 2 site_a", "app_storefront_base 7 site_b"]);
        // `base.total(basket)` inside the override is the parent's.
        let found = definitions(&workspace, &root, ("app_na", HELPER), "total(basket)");
        assert_eq!(found, ["app_storefront_base 7 site_a"]);
    }

    #[test]
    fn says_nothing_about_a_definition_in_the_file_itself() {
        let root = checkout(
            "isml-lsp-members-here",
            &[("app_storefront_base", HELPER, DOCUMENTED)],
        );
        let found = definitions(
            &workspace(&root, None),
            &root,
            ("app_storefront_base", HELPER),
            "total: total",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn reads_the_signature_and_the_documentation() {
        let chars: Vec<char> = DOCUMENTED.chars().collect();
        let at = declaration(&chars, "total").unwrap();
        assert_eq!(
            signature(&chars, at, "total").as_deref(),
            Some("total(basket, options)")
        );
        let doc = doc_above(&chars, at).unwrap();
        assert_eq!(
            doc,
            "Adds up the basket.\n@param {dw.order.Basket} basket - the basket\n@returns {number} the total"
        );
        assert_eq!(
            doc_markdown(&doc),
            "Adds up the basket.\n\n*@param* {dw.order.Basket} basket - the basket  \n*@returns* {number} the total"
        );

        let chars: Vec<char> = REDEFINES.chars().collect();
        let at = REDEFINES.find("total =").unwrap();
        assert_eq!(
            signature(&chars, at, "total").as_deref(),
            Some("total(basket)")
        );
        assert_eq!(doc_above(&chars, at).as_deref(), Some("The brand's total."));
    }

    #[test]
    fn a_plain_comment_is_not_documentation() {
        let text = "/* not this */\nfunction total() {}\n";
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(doc_above(&chars, text.find("total").unwrap()), None);
    }

    #[test]
    fn the_hover_says_where_and_for_whom() {
        let definition = Definition {
            storefronts: vec!["site_a".into(), "site_b".into()],
            cartridge: "app_brand".into(),
            path: std::env::temp_dir().join("cartHelpers.js"),
            line: 0,
            start: 0,
            end: 5,
            signature: Some("total(basket)".into()),
            doc: Some("Adds up.".into()),
        };
        let text = markdown(&[definition]).unwrap();
        assert!(text.starts_with(
            "```js\ntotal(basket)\n```\n\nAdds up.\n\nDefined in [`app_brand`](file:"
        ));
        assert!(text.ends_with(", run by `site_a`, `site_b`"));
    }

    fn offered_names(workspace: &Workspace, root: &Path, typed: &str) -> Vec<String> {
        let path = root.join("cartridges/app_brand").join(CART);
        let text = format!("{CALLER}{typed}");
        let (offered, _) = offered(
            &path,
            &text,
            text.chars().count(),
            workspace,
            &HashMap::new(),
        )
        .unwrap();
        offered
            .into_iter()
            .map(|offered| {
                let storefronts: Vec<String> = offered
                    .definitions
                    .iter()
                    .map(|definition| {
                        format!(
                            "{}:{}",
                            definition.cartridge,
                            definition.storefronts.join(",")
                        )
                    })
                    .collect();
                format!("{} {}", offered.name, storefronts.join(" "))
            })
            .collect()
    }

    #[test]
    fn offers_what_each_copy_exports_and_inherits() {
        let root = checkout(
            "isml-lsp-members-offered",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, PASSES_ON),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = offered_names(&workspace(&root, None), &root, "helpers.");
        assert_eq!(
            found,
            [
                "other app_na:site_a",
                "total app_storefront_base:site_a,site_b"
            ]
        );
    }

    #[test]
    fn offers_only_the_chosen_storefront() {
        let root = checkout(
            "isml-lsp-members-offered-chosen",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, PASSES_ON),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = offered_names(&workspace(&root, Some("site_b")), &root, "helpers.to");
        assert_eq!(found, ["total app_storefront_base:site_b"]);
    }

    #[test]
    fn a_copy_that_does_not_inherit_hides_its_parent_names() {
        let own = "module.exports = {\n    mine: function (a) {}\n};\n";
        let root = checkout(
            "isml-lsp-members-offered-own",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_brand", HELPER, own),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = offered_names(&workspace(&root, None), &root, "helpers.");
        assert_eq!(found, ["mine app_brand:site_a,site_b"]);
    }
}
