use std::fs;
use std::path::{Path, PathBuf};

use crate::hover;
use crate::reference::Reference;
use crate::workspace::Workspace;

const MODULE_EXTENSIONS: [&str; 3] = ["js", "json", "ds"];
/// `default` holds the fallback template, so it is tried first.
const TEMPLATE_DIRS: [&str; 1] = ["default"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// Zero-based.
    pub line: u32,
}

impl From<PathBuf> for Hit {
    fn from(path: PathBuf) -> Self {
        Hit { path, line: 0 }
    }
}

/// One cartridge's copy of something several cartridges can define; the cartridge path
/// decides which copy the platform uses.
#[derive(Debug, Clone)]
pub struct Override {
    pub cartridge: String,
    pub hit: Hit,
}

/// Most relevant first; a template or script usually exists in several overriding cartridges.
pub fn resolve(reference: &Reference, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    match reference {
        Reference::Template(path) => what_runs(templates(path, from, workspace), workspace),
        Reference::Module(path) => modules(path, from, workspace),
        Reference::Resource { key, bundle } => resources(key, bundle, from, workspace),
        Reference::Route { .. } => routes(reference, from, workspace),
    }
}

/// The copies the cartridge path chooses between, for the hover that lists them:
/// a `*/cartridge/...` module or a template. Empty for anything else.
pub fn overrides(reference: &Reference, from: &Path, workspace: &Workspace) -> Vec<Override> {
    match reference {
        Reference::Template(path) => templates(path, from, workspace),
        Reference::Module(path) => match path.strip_prefix("*/") {
            Some(rest) => path_modules(rest, from, workspace),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The copy each recorded cartridge path uses: the leftmost cartridge that has one.
/// Labelled by path, in the order the paths are recorded; a path holding none is left out.
pub fn winners<'a>(
    overrides: &'a [Override],
    workspace: &'a Workspace,
) -> Vec<(&'a str, &'a Override)> {
    workspace
        .paths
        .iter()
        .filter_map(|path| {
            overrides
                .iter()
                .filter_map(|candidate| {
                    path.rank(&candidate.cartridge)
                        .map(|rank| (rank, candidate))
                })
                .min_by_key(|(rank, _)| *rank)
                .map(|(_, candidate)| (path.label.as_str(), candidate))
        })
        .collect()
}

/// With a cartridge path recorded, only what it runs: the chosen storefront's copy, or one
/// location when every storefront runs the same file, or one per storefront when they
/// differ. With none recorded the order is not knowable, so every copy is offered.
fn what_runs(overrides: Vec<Override>, workspace: &Workspace) -> Vec<Hit> {
    let winners = winners(&overrides, workspace);
    if winners.is_empty() {
        return overrides
            .into_iter()
            .map(|candidate| candidate.hit)
            .collect();
    }
    if let Some(chosen) = &workspace.storefront {
        if let Some((_, candidate)) = winners.iter().find(|(label, _)| label == chosen) {
            return vec![candidate.hit.clone()];
        }
    }
    let mut hits: Vec<Hit> = Vec::new();
    for (_, candidate) in winners {
        if !hits.contains(&candidate.hit) {
            hits.push(candidate.hit.clone());
        }
    }
    hits
}

/// Every cartridge declaring the route, the one that runs first.
fn routes(reference: &Reference, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    let Some(route) = hover::route_of(reference, from) else {
        return Vec::new();
    };
    hover::targets(&hover::chains(&route, workspace))
        .into_iter()
        .map(|(path, line)| Hit { path, line })
        .collect()
}

fn templates(template: &str, from: &Path, workspace: &Workspace) -> Vec<Override> {
    let relative = format!("{}.isml", template.trim_start_matches('/'));
    let mut found = Vec::new();
    for cartridge in workspace.cartridges_from(from) {
        let templates_dir = cartridge.cartridge_dir().join("templates");
        for locale in template_dirs(&templates_dir) {
            let candidate = templates_dir.join(locale).join(&relative);
            if candidate.is_file() {
                found.push(Override {
                    cartridge: cartridge.name.clone(),
                    hit: candidate.into(),
                });
            }
        }
    }
    found
}

/// `default` first, then any other locale folder that exists.
fn template_dirs(templates_dir: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = TEMPLATE_DIRS.iter().map(|dir| dir.to_string()).collect();
    let Ok(entries) = fs::read_dir(templates_dir) else {
        return dirs;
    };
    let mut others: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| !dirs.contains(name) && name != "resources")
        .collect();
    others.sort();
    dirs.extend(others);
    dirs
}

fn modules(module: &str, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    if module.starts_with("dw/") || module.starts_with("dw.") {
        return Vec::new();
    }

    if module.starts_with("./") || module.starts_with("../") {
        let base = from.parent().unwrap_or(from);
        return existing_module(&base.join(module.replace('/', std::path::MAIN_SEPARATOR_STR)))
            .into_iter()
            .map(Hit::from)
            .collect();
    }

    let mut hits = Vec::new();

    if let Some(rest) = module.strip_prefix("*/") {
        // `*/` is relative to the start of the cartridge path, wherever the require is.
        hits = what_runs(path_modules(rest, from, workspace), workspace);
    } else if let Some(rest) = module.strip_prefix("~/") {
        if let Some(cartridge) = workspace.cartridge_of(from) {
            push_module(&mut hits, &cartridge.root.join(rest));
        }
    } else if let Some((name, rest)) = module.split_once('/') {
        // `app_brand/cartridge/scripts/x` — an explicit cartridge.
        for cartridge in workspace.cartridges_from(from) {
            if cartridge.name == name {
                push_module(&mut hits, &cartridge.root.join(rest));
            }
        }
    }

    // Bare specifiers (`server`, `int_gtm`, `dwapi/...`) come from a
    // `cartridges/modules` folder.
    if hits.is_empty() {
        for module_root in &workspace.module_roots {
            push_module(&mut hits, &module_root.join(module));
        }
    }

    hits
}

/// Every cartridge's copy of a `*/` module.
pub(crate) fn path_modules(rest: &str, from: &Path, workspace: &Workspace) -> Vec<Override> {
    workspace
        .cartridges_from(from)
        .into_iter()
        .filter_map(|cartridge| {
            existing_module(&cartridge.root.join(rest)).map(|path| Override {
                cartridge: cartridge.name.clone(),
                hit: path.into(),
            })
        })
        .collect()
}

fn push_module(hits: &mut Vec<Hit>, candidate: &Path) {
    if let Some(path) = existing_module(candidate) {
        hits.push(path.into());
    }
}

/// `x` resolves to `x`, `x.js`, `x.json`, `x.ds` or `x/index.js`, as SFCC does.
pub(crate) fn existing_module(candidate: &Path) -> Option<PathBuf> {
    if candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    for extension in MODULE_EXTENSIONS {
        let with_extension = PathBuf::from(format!("{}.{extension}", candidate.display()));
        if with_extension.is_file() {
            return Some(with_extension);
        }
    }
    let index = candidate.join("index.js");
    index.is_file().then_some(index)
}

fn resources(key: &str, bundle: &str, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    let file_name = format!("{bundle}.properties");
    let bundles: Vec<Override> = workspace
        .cartridges_from(from)
        .into_iter()
        .map(|cartridge| Override {
            cartridge: cartridge.name.clone(),
            hit: cartridge
                .cartridge_dir()
                .join("templates")
                .join("resources")
                .join(&file_name)
                .into(),
        })
        .filter(|candidate| candidate.hit.path.is_file())
        .collect();

    let defining: Vec<Override> = bundles
        .iter()
        .filter_map(|candidate| {
            line_of_key(&candidate.hit.path, key).map(|line| Override {
                cartridge: candidate.cartridge.clone(),
                hit: Hit {
                    path: candidate.hit.path.clone(),
                    line,
                },
            })
        })
        .collect();

    // A key nobody defines is usually a typo; still offer the bundles it would
    // belong to rather than nothing at all.
    if defining.is_empty() {
        return bundles.into_iter().map(|candidate| candidate.hit).collect();
    }
    // A key is looked up bundle by bundle along the path, so the leftmost definition wins.
    what_runs(defining, workspace)
}

fn line_of_key(path: &Path, key: &str) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;
    contents
        .lines()
        .position(|line| defines_key(line, key))
        .map(|index| index as u32)
}

fn defines_key(line: &str, key: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_a_whole_key() {
        assert!(defines_key("label.a.b=Value", "label.a.b"));
        assert!(defines_key("  label.a.b = Value", "label.a.b"));
        assert!(!defines_key("label.a.bc=Value", "label.a.b"));
        assert!(!defines_key("#label.a.b=Value", "label.a.b"));
    }

    const HELPER: &str = "cartridge/scripts/helpers/productHelpers.js";
    const TEMPLATE: &str = "cartridge/templates/default/product/tile.isml";

    /// `app_na` overrides `app_brand`, which overrides base; `site_b` leaves `app_na` out.
    fn checkout(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&root);
        for (cartridge, file) in files {
            let path = root.join("cartridges").join(cartridge).join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "label.title=Title\n").unwrap();
        }
        let controller = root.join("cartridges/app_brand/cartridge/controllers/Cart.js");
        fs::create_dir_all(controller.parent().unwrap()).unwrap();
        fs::write(controller, "").unwrap();
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

    fn cart(root: &Path) -> PathBuf {
        root.join("cartridges/app_brand/cartridge/controllers/Cart.js")
    }

    fn cartridges_of(hits: &[Hit], root: &Path) -> Vec<String> {
        hits.iter()
            .map(|hit| {
                let relative = hit.path.strip_prefix(root.join("cartridges")).unwrap();
                relative
                    .iter()
                    .next()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn every_override(name: &str) -> PathBuf {
        checkout(
            name,
            &[
                ("app_na", HELPER),
                ("app_brand", HELPER),
                ("app_storefront_base", HELPER),
            ],
        )
    }

    fn helper() -> Reference {
        Reference::Module("*/cartridge/scripts/helpers/productHelpers".into())
    }

    #[test]
    fn jumps_to_what_the_chosen_storefront_runs() {
        let root = every_override("isml-lsp-resolve-chosen");
        let hits = resolve(&helper(), &cart(&root), &workspace(&root, Some("site_a")));
        assert_eq!(cartridges_of(&hits, &root), ["app_na"]);

        let hits = resolve(&helper(), &cart(&root), &workspace(&root, Some("site_b")));
        assert_eq!(cartridges_of(&hits, &root), ["app_brand"]);
    }

    #[test]
    fn offers_one_location_per_storefront_when_they_run_different_files() {
        let root = every_override("isml-lsp-resolve-disagree");
        let hits = resolve(&helper(), &cart(&root), &workspace(&root, None));
        assert_eq!(cartridges_of(&hits, &root), ["app_na", "app_brand"]);
    }

    #[test]
    fn jumps_directly_when_every_storefront_runs_the_same_file() {
        let root = checkout(
            "isml-lsp-resolve-agree",
            &[("app_brand", HELPER), ("app_storefront_base", HELPER)],
        );
        let hits = resolve(&helper(), &cart(&root), &workspace(&root, None));
        assert_eq!(cartridges_of(&hits, &root), ["app_brand"]);
    }

    #[test]
    fn falls_back_to_every_storefront_when_the_chosen_one_has_no_copy() {
        let root = every_override("isml-lsp-resolve-unknown-storefront");
        let hits = resolve(&helper(), &cart(&root), &workspace(&root, Some("site_c")));
        assert_eq!(cartridges_of(&hits, &root), ["app_na", "app_brand"]);
    }

    #[test]
    fn offers_every_copy_when_no_cartridge_path_is_recorded() {
        let root = every_override("isml-lsp-resolve-no-path");
        let workspace = Workspace::scan(std::slice::from_ref(&root), &serde_json::Value::Null);
        let hits = resolve(&helper(), &cart(&root), &workspace);
        assert_eq!(
            cartridges_of(&hits, &root),
            ["app_brand", "app_na", "app_storefront_base"]
        );
    }

    #[test]
    fn leaves_the_current_cartridge_alone() {
        let root = every_override("isml-lsp-resolve-tilde");
        let reference = Reference::Module("~/cartridge/scripts/helpers/productHelpers".into());
        let hits = resolve(&reference, &cart(&root), &workspace(&root, Some("site_a")));
        assert_eq!(cartridges_of(&hits, &root), ["app_brand"]);
    }

    #[test]
    fn resolves_a_template_along_the_path() {
        let root = checkout(
            "isml-lsp-resolve-template",
            &[("app_na", TEMPLATE), ("app_brand", TEMPLATE)],
        );
        let reference = Reference::Template("product/tile".into());
        let hits = resolve(&reference, &cart(&root), &workspace(&root, Some("site_b")));
        assert_eq!(cartridges_of(&hits, &root), ["app_brand"]);
    }

    #[test]
    fn resolves_a_resource_key_along_the_path() {
        let bundle = "cartridge/templates/resources/product.properties";
        let root = checkout(
            "isml-lsp-resolve-resource",
            &[("app_na", bundle), ("app_brand", bundle)],
        );
        let reference = Reference::Resource {
            key: "label.title".into(),
            bundle: "product".into(),
        };
        let hits = resolve(&reference, &cart(&root), &workspace(&root, Some("site_a")));
        assert_eq!(cartridges_of(&hits, &root), ["app_na"]);
    }

    #[test]
    fn names_the_winner_of_each_path() {
        let root = every_override("isml-lsp-resolve-winners");
        let workspace = workspace(&root, None);
        let overrides = overrides(&helper(), &cart(&root), &workspace);
        let winners: Vec<(&str, &str)> = winners(&overrides, &workspace)
            .into_iter()
            .map(|(label, candidate)| (label, candidate.cartridge.as_str()))
            .collect();
        assert_eq!(winners, [("site_a", "app_na"), ("site_b", "app_brand")]);
    }
}
