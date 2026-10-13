//! The function behind `helpers.total`: Go to Definition and hover on a member of a
//! cartridge module, along the cartridge path. A copy that passes its parent's function on
//! (`module.exports = base`) sends the search on to the next copy to its right.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::Url;

use tree_sitter::Node;

use crate::js::{Export, Origin, Parsed, Pending, Script, Value};
use crate::reference::Reference;
use crate::references::{located, read};
use crate::resolve;
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
    /// Each parameter of that function as written, and its name when it has a plain one.
    pub parameters: Vec<(String, Option<String>)>,
    /// The `/** ... */` block above the definition, without its stars.
    pub doc: Option<String>,
}

/// What the member at `offset` (characters into `text`) resolves to, one definition per
/// distinct file: the chosen storefront's only, when `storefront` is set and that
/// storefront has one.
pub fn at(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Definition> {
    let Some(parsed) = Parsed::new(file, text) else {
        return Vec::new();
    };
    let script = parsed.script();
    match script.name_at(byte_offset(text, offset)) {
        Some(node) => of(&script, node, file, workspace, open),
        None => Vec::new(),
    }
}

/// What the member `node` names resolves to, as `at`.
pub fn of<'a>(
    script: &Script<'a>,
    node: Node<'a>,
    file: &Path,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Vec<Definition> {
    let Some((origin, name)) = script.export_at(node) else {
        return Vec::new();
    };
    let chains = chains(&origin, file, workspace);
    let copies = Copies::load(&chains, open);
    resolved(&chains, &name, workspace, &copies.scripts())
}

/// A member a module offers after `helpers.`, with where it is defined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub name: String,
    pub definitions: Vec<Definition>,
}

/// What the module before the dot at `offset` exports, as the cartridge path builds it:
/// each copy's own names, then its parent's while it inherits them. With the number of
/// characters of the member already typed. `None` unless the module is a cartridge one, so
/// `dw.*` and `.custom.` completion still answer.
pub fn offered(
    file: &Path,
    text: &str,
    offset: usize,
    workspace: &Workspace,
    open: &HashMap<PathBuf, &str>,
) -> Option<(Vec<Offered>, usize)> {
    let pending = Pending::at(text, offset)?;
    let parsed = Parsed::new(file, &pending.text)?;
    let script = parsed.script();
    let Value::Module(origin) = script.value_of(pending.receiver(&script)?)? else {
        return None;
    };

    let mut chains = chains(&origin, file, workspace);
    if let Some(chosen) = &workspace.storefront {
        if chains
            .iter()
            .any(|(label, _)| label.as_ref() == Some(chosen))
        {
            chains.retain(|(label, _)| label.as_ref() == Some(chosen));
        }
    }
    let copies = Copies::load(&chains, open);
    let scripts = copies.scripts();
    let mut names: Vec<String> = Vec::new();
    for (_, chain) in &chains {
        for path in chain {
            let Some((_, copy)) = scripts.get(path) else {
                break;
            };
            let (exported, inherits) = copy.exported_names();
            for name in exported {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            if !inherits {
                break;
            }
        }
    }
    let offered: Vec<Offered> = names
        .into_iter()
        .map(|name| Offered {
            definitions: resolved(&chains, &name, workspace, &scripts),
            name,
        })
        .filter(|offered| !offered.definitions.is_empty())
        .collect();
    (!offered.is_empty()).then_some((offered, pending.typed))
}

/// Every copy a request may ask about, read and parsed once however many names it is
/// asked about.
struct Copies {
    loaded: HashMap<PathBuf, (String, Parsed)>,
}

impl Copies {
    fn load(chains: &[(Option<String>, Vec<PathBuf>)], open: &HashMap<PathBuf, &str>) -> Copies {
        let mut loaded = HashMap::new();
        for path in chains.iter().flat_map(|(_, chain)| chain) {
            if loaded.contains_key(path) {
                continue;
            }
            let Some(text) = read(path, open) else {
                continue;
            };
            if let Some(parsed) = Parsed::new(path, &text) {
                loaded.insert(path.clone(), (text, parsed));
            }
        }
        Copies { loaded }
    }

    fn scripts(&self) -> HashMap<PathBuf, (&str, Script<'_>)> {
        self.loaded
            .iter()
            .map(|(path, (text, parsed))| (path.clone(), (text.as_str(), parsed.script())))
            .collect()
    }
}

/// The definition each chain reaches, one per distinct file, labelled with the storefronts
/// that reach it: the chosen storefront's only, when it has one.
fn resolved(
    chains: &[(Option<String>, Vec<PathBuf>)],
    name: &str,
    workspace: &Workspace,
    scripts: &HashMap<PathBuf, (&str, Script)>,
) -> Vec<Definition> {
    let mut found: Vec<Definition> = Vec::new();
    for (storefront, chain) in chains {
        let Some(mut definition) = defined_along(chain, name, workspace, scripts) else {
            continue;
        };
        if let (Some(chosen), Some(label)) = (&workspace.storefront, storefront) {
            if chosen == label {
                definition.storefronts = vec![label.clone()];
                return vec![definition];
            }
        }
        match found
            .iter_mut()
            .find(|seen| seen.path == definition.path && seen.line == definition.line)
        {
            Some(seen) => seen.storefronts.extend(storefront.clone()),
            None => {
                definition.storefronts = storefront.iter().cloned().collect();
                found.push(definition);
            }
        }
    }
    found
}

fn byte_offset(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(at, _)| at)
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
/// passes its parent's on; a copy that leaves it out, or exports something the text cannot
/// read, ends the search.
fn defined_along(
    chain: &[PathBuf],
    name: &str,
    workspace: &Workspace,
    scripts: &HashMap<PathBuf, (&str, Script)>,
) -> Option<Definition> {
    for path in chain {
        let (text, script) = scripts.get(path)?;
        match script.export(name) {
            Export::Defines(definition) => {
                // The declaration carries the documentation; `total: total` only names it.
                let site = definition.sites.first()?;
                let description = script.describe(site, name);
                let location = located(path, text, &[site.name]).into_iter().next()?;
                return Some(Definition {
                    storefronts: Vec::new(),
                    cartridge: workspace
                        .cartridge_of(path)
                        .map(|cartridge| cartridge.name.clone())
                        .unwrap_or_default(),
                    path: path.clone(),
                    line: location.line,
                    start: location.start,
                    end: location.end,
                    signature: description.signature,
                    parameters: description.parameters,
                    doc: description.doc,
                });
            }
            Export::PassesOn => continue,
            Export::Absent | Export::Unknown => return None,
        }
    }
    None
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
    /// The SFRA way: the parent's function is saved before the override replaces it.
    const REDEFINES: &str = "var base = module.superModule;\nvar superTotal = base.total;\n/** The brand's total. */\nbase.total = function (basket) {\n    return superTotal(basket);\n};\nmodule.exports = base;\n";
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

    fn definitions_of(
        workspace: &Workspace,
        root: &Path,
        file: (&str, &str),
        at: &str,
    ) -> Vec<Definition> {
        let path = root.join("cartridges").join(file.0).join(file.1);
        let text = fs::read_to_string(&path).unwrap();
        let offset = text[..text.find(at).unwrap()].chars().count();
        self::at(&path, &text, offset, workspace, &HashMap::new())
    }

    /// The definitions of the member at the first `at` in the file, as `cartridge line storefronts`.
    fn definitions(
        workspace: &Workspace,
        root: &Path,
        file: (&str, &str),
        at: &str,
    ) -> Vec<String> {
        definitions_of(workspace, root, file, at)
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
        assert_eq!(found, ["app_na 3 site_a", "app_storefront_base 7 site_b"]);
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
        assert_eq!(found, ["app_na 3 site_a"]);
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
    fn stops_at_a_copy_that_leaves_the_function_out() {
        let root = checkout(
            "isml-lsp-members-left-out",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, "module.exports = { other: 1 };\n"),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = definitions(
            &workspace(&root, None),
            &root,
            ("app_brand", CART),
            "total(1)",
        );
        // site_a reaches app_na first, where `total` is undefined.
        assert_eq!(found, ["app_storefront_base 7 site_b"]);
    }

    #[test]
    fn resolves_a_destructured_name_and_a_saved_parent_function() {
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
        assert_eq!(found, ["app_na 3 site_a", "app_storefront_base 7 site_b"]);
        // `superTotal(basket)` inside the override calls what it saved: the parent's.
        let found = definitions(&workspace, &root, ("app_na", HELPER), "superTotal(basket)");
        assert_eq!(found, ["app_storefront_base 7 site_a"]);
    }

    #[test]
    fn says_nothing_about_a_definition_in_the_file_itself() {
        let root = checkout(
            "isml-lsp-members-here",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, REDEFINES),
            ],
        );
        let workspace = workspace(&root, None);
        let found = definitions(
            &workspace,
            &root,
            ("app_storefront_base", HELPER),
            "total: total",
        );
        assert!(found.is_empty());
        // Once replaced, `base.total` is the override's own: nothing to jump to elsewhere.
        let overriding = REDEFINES.replace("superTotal(basket)", "base.total.length");
        let root = checkout(
            "isml-lsp-members-own",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, &overriding),
            ],
        );
        let found = definitions(
            &self::tests::workspace(&root, None),
            &root,
            ("app_na", HELPER),
            "total.length",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn reads_the_signature_and_the_documentation() {
        let root = checkout(
            "isml-lsp-members-described",
            &[
                ("app_storefront_base", HELPER, DOCUMENTED),
                ("app_na", HELPER, REDEFINES),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = definitions_of(
            &workspace(&root, None),
            &root,
            ("app_brand", CART),
            "total(1)",
        );
        let base = &found[1];
        assert_eq!(base.signature.as_deref(), Some("total(basket, options)"));
        let doc = base.doc.as_deref().unwrap();
        assert_eq!(
            doc,
            "Adds up the basket.\n@param {dw.order.Basket} basket - the basket\n@returns {number} the total"
        );
        assert_eq!(
            doc_markdown(doc),
            "Adds up the basket.\n\n*@param* {dw.order.Basket} basket - the basket  \n*@returns* {number} the total"
        );
        let brand = &found[0];
        assert_eq!(brand.signature.as_deref(), Some("total(basket)"));
        assert_eq!(brand.doc.as_deref(), Some("The brand's total."));
    }

    #[test]
    fn documentation_belongs_to_the_definition_right_below_it() {
        let helper = "/** About other. */\nfunction other() {}\n/* not documentation */\nfunction total() {}\n/** About something else. */\nvar x = 1;\nfunction count() {}\nmodule.exports = { other: other, total: total, count: count };\n";
        let root = checkout(
            "isml-lsp-members-docs",
            &[
                ("app_storefront_base", HELPER, helper),
                ("app_brand", CART, "var h = require('*/cartridge/scripts/helpers/cartHelpers');\nh.other(); h.total(); h.count();\n"),
            ],
        );
        let workspace = workspace(&root, None);
        let doc = |at: &str| {
            definitions_of(&workspace, &root, ("app_brand", CART), at)[0]
                .doc
                .clone()
        };
        assert_eq!(doc("other()").as_deref(), Some("About other."));
        assert_eq!(doc("total()"), None);
        assert_eq!(doc("count()"), None);
    }

    #[test]
    fn jumps_to_the_function_the_export_names_not_a_local_of_the_same_name() {
        let helper = "function sum() {\n    var total = 0;\n    return total;\n}\nfunction computeTotal(basket) {}\nmodule.exports = { sum: sum, total: computeTotal };\n";
        let root = checkout(
            "isml-lsp-members-named",
            &[
                ("app_storefront_base", HELPER, helper),
                ("app_brand", CART, CALLER),
            ],
        );
        let found = definitions_of(
            &workspace(&root, None),
            &root,
            ("app_brand", CART),
            "total(1)",
        );
        assert_eq!((found[0].line, found[0].start), (4, 9));
        assert_eq!(found[0].signature.as_deref(), Some("total(basket)"));
    }

    fn offered_names(workspace: &Workspace, root: &Path, typed: &str) -> Vec<String> {
        let path = root.join("cartridges/app_brand").join(CART);
        let text = format!("{CALLER}{typed}");
        let Some((offered, _)) = offered(
            &path,
            &text,
            text.chars().count(),
            workspace,
            &HashMap::new(),
        ) else {
            return Vec::new();
        };
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

    #[test]
    fn offers_nothing_for_a_platform_module_or_a_plain_object() {
        let root = checkout(
            "isml-lsp-members-offered-none",
            &[("app_storefront_base", HELPER, DOCUMENTED)],
        );
        let workspace = workspace(&root, None);
        for typed in [
            "var Site = require('dw/system/Site');\nSite.",
            "var o = {};\no.",
            "helpers.total(1).",
        ] {
            assert!(
                offered_names(&workspace, &root, typed).is_empty(),
                "{typed}"
            );
        }
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
            parameters: vec![("basket".into(), Some("basket".into()))],
            doc: Some("Adds up.".into()),
        };
        let text = markdown(&[definition]).unwrap();
        assert!(text.starts_with(
            "```js\ntotal(basket)\n```\n\nAdds up.\n\nDefined in [`app_brand`](file:"
        ));
        assert!(text.ends_with(", run by `site_a`, `site_b`"));
    }
}
