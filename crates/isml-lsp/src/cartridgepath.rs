//! Cartridge path order, which is not in the source: from editor settings, site archives or `dw.json`.
//! Settings win: `dw.json` is personal and usually ignored, and site archives are rarely checked out.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const MAX_DEPTH: usize = 8;
const SKIPPED: [&str; 6] = ["node_modules", ".git", "static", "build", "dist", "target"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CartridgePath {
    /// The site id, or `dw.json` when that is where it came from.
    pub label: String,
    /// Cartridge names, leftmost — highest priority — first.
    pub order: Vec<String>,
}

impl CartridgePath {
    pub fn rank(&self, cartridge: &str) -> Option<usize> {
        self.order.iter().position(|name| name == cartridge)
    }
}

/// Settings first, then one per site archive and `dw.json`.
pub fn load(roots: &[PathBuf], settings: &Value) -> Vec<CartridgePath> {
    let mut found = from_settings(settings);
    for root in roots {
        visit(root, 0, &mut found);
    }
    // A stable sort keeps the settings ahead of a site of the same name, and
    // dedup keeps the first — so naming a site in the settings corrects it.
    found.sort_by(|a, b| a.label.cmp(&b.label));
    found.dedup_by(|a, b| a.label == b.label);
    found
}

/// `cartridge_path` init option: one path or an object per storefront; each a colon-joined string or an array.
fn from_settings(settings: &Value) -> Vec<CartridgePath> {
    let Some(declared) = settings.get("cartridge_path") else {
        return Vec::new();
    };
    if let Some(sites) = declared.as_object() {
        return sites
            .iter()
            .filter_map(|(label, value)| named(label, value))
            .collect();
    }
    named("settings", declared).into_iter().collect()
}

fn named(label: &str, value: &Value) -> Option<CartridgePath> {
    let order = match value {
        Value::String(text) => split_path(text),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .map(leaf_name)
            .filter(|name| !name.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    (!order.is_empty()).then(|| CartridgePath {
        label: label.to_string(),
        order,
    })
}

fn visit(dir: &Path, depth: usize, into: &mut Vec<CartridgePath>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if !SKIPPED.contains(&name) && !name.starts_with('.') {
                visit(&path, depth + 1, into);
            }
            continue;
        }
        match name {
            "site.xml" => into.extend(from_site_archive(&path)),
            "dw.json" => into.extend(from_dw_json(&path)),
            _ => {}
        }
    }
}

/// `<custom-cartridges>a:b:c</custom-cartridges>`, and the `site-id` naming it.
fn from_site_archive(file: &Path) -> Option<CartridgePath> {
    let text = fs::read_to_string(file).ok()?;
    let document = roxmltree::Document::parse(&text).ok()?;
    let root = document.root_element();
    let label = root
        .attribute("site-id")
        .map(str::to_string)
        .or_else(|| directory_name(file))?;
    let node = root
        .descendants()
        .find(|node| node.tag_name().name() == "custom-cartridges")?;
    let order = split_path(node.text()?);
    (!order.is_empty()).then_some(CartridgePath { label, order })
}

/// Only the `cartridge` key is read. Everything else in `dw.json` is a
/// credential, and serde drops an unknown field rather than holding it.
fn from_dw_json(file: &Path) -> Option<CartridgePath> {
    #[derive(serde::Deserialize)]
    struct Relevant {
        cartridge: Option<Vec<String>>,
    }

    let text = fs::read_to_string(file).ok()?;
    let relevant: Relevant = serde_json::from_str(&text).ok()?;
    let order: Vec<String> = relevant
        .cartridge?
        .iter()
        .map(|name| leaf_name(name))
        .filter(|name| !name.is_empty())
        .collect();
    (!order.is_empty()).then_some(CartridgePath {
        label: "dw.json".to_string(),
        order,
    })
}

fn split_path(value: &str) -> Vec<String> {
    value
        .split(':')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Prophet allows a path in the `cartridge` array; only the name matters.
fn leaf_name(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn directory_name(file: &Path) -> Option<String> {
    Some(file.parent()?.file_name()?.to_str()?.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<site xmlns="http://www.demandware.com/xml/impex/site/2007-4-31" site-id="storefront">
    <currency>EUR</currency>
    <custom-cartridges>int_search:app_brand:app_storefront_base</custom-cartridges>
</site>
"#;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn reads_a_path_per_storefront_from_the_settings() {
        let settings = serde_json::json!({
            "cartridge_path": {
                "storefront_a": "app_brand:app_storefront_base",
                "storefront_b": ["int_search", "app_storefront_base"],
            }
        });
        let found = load(&[], &settings);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].label, "storefront_a");
        assert_eq!(found[0].order, ["app_brand", "app_storefront_base"]);
        assert_eq!(found[1].order, ["int_search", "app_storefront_base"]);
    }

    #[test]
    fn reads_a_single_unnamed_path_from_the_settings() {
        let settings = serde_json::json!({ "cartridge_path": "app_brand:app_storefront_base" });
        let found = load(&[], &settings);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "settings");
    }

    #[test]
    fn lets_the_settings_correct_a_site_of_the_same_name() {
        let directory = scratch("isml-lsp-settings-wins");
        fs::write(directory.join("site.xml"), SITE).unwrap();
        let settings = serde_json::json!({ "cartridge_path": { "storefront": ["app_brand"] } });
        let found = load(&[directory], &settings);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].order, ["app_brand"]);
    }

    #[test]
    fn ignores_settings_that_declare_nothing() {
        assert!(load(&[], &Value::Null).is_empty());
        assert!(load(&[], &serde_json::json!({ "cartridge_path": [] })).is_empty());
    }

    #[test]
    fn reads_the_ordered_path_of_each_site() {
        let directory = scratch("isml-lsp-site-path");
        fs::write(directory.join("site.xml"), SITE).unwrap();
        let found = load(&[directory], &Value::Null);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "storefront");
        assert_eq!(
            found[0].order,
            ["int_search", "app_brand", "app_storefront_base"]
        );
    }

    #[test]
    fn ranks_by_position_and_ignores_a_cartridge_outside_the_path() {
        let path = CartridgePath {
            label: "storefront".into(),
            order: vec!["a".into(), "b".into()],
        };
        assert_eq!(path.rank("a"), Some(0));
        assert_eq!(path.rank("b"), Some(1));
        assert_eq!(path.rank("c"), None);
    }

    #[test]
    fn reads_the_cartridge_array_of_dw_json_and_nothing_else() {
        let directory = scratch("isml-lsp-dw-path");
        fs::write(
            directory.join("dw.json"),
            r#"{"hostname":"x","username":"u","password":"p",
                "cartridge":["app_brand","app_storefront_base"]}"#,
        )
        .unwrap();
        let found = load(&[directory], &Value::Null);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "dw.json");
        assert_eq!(found[0].order, ["app_brand", "app_storefront_base"]);
    }

    #[test]
    fn says_nothing_when_dw_json_carries_no_cartridge_array() {
        let directory = scratch("isml-lsp-dw-bare");
        fs::write(
            directory.join("dw.json"),
            r#"{"hostname":"x","password":"p"}"#,
        )
        .unwrap();
        assert!(load(&[directory], &Value::Null).is_empty());
    }
}
