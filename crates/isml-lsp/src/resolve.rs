//! Turning a [`Reference`] into the files it points at.

use std::fs;
use std::path::{Path, PathBuf};

use crate::reference::Reference;
use crate::workspace::Workspace;

const MODULE_EXTENSIONS: [&str; 3] = ["js", "json", "ds"];
/// Locale variants live next to `default`; the fallback template is the one in
/// `default`, so it is tried first.
const TEMPLATE_DIRS: [&str; 1] = ["default"];

pub struct Hit {
    pub path: PathBuf,
    /// Zero-based line to put the cursor on.
    pub line: u32,
}

impl From<PathBuf> for Hit {
    fn from(path: PathBuf) -> Self {
        Hit { path, line: 0 }
    }
}

pub fn resolve(reference: &Reference, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    match reference {
        Reference::Template(path) => templates(path, from, workspace),
        Reference::Module(path) => modules(path, from, workspace),
        Reference::Resource { key, bundle } => resources(key, bundle, from, workspace),
    }
}

fn templates(template: &str, from: &Path, workspace: &Workspace) -> Vec<Hit> {
    let relative = format!("{}.isml", template.trim_start_matches('/'));
    let mut hits = Vec::new();
    for cartridge in workspace.cartridges_from(from) {
        let templates_dir = cartridge.cartridge_dir().join("templates");
        for locale in template_dirs(&templates_dir) {
            let candidate = templates_dir.join(locale).join(&relative);
            if candidate.is_file() {
                hits.push(candidate.into());
            }
        }
    }
    hits
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
        for cartridge in workspace.cartridges_from(from) {
            push_module(&mut hits, &cartridge.root.join(rest));
        }
    } else if let Some(rest) = module.strip_prefix("~/") {
        if let Some(cartridge) = workspace.cartridge_of(from) {
            push_module(&mut hits, &cartridge.root.join(rest));
        }
    } else if let Some((name, rest)) = module.split_once('/') {
        // `app_common_eu_guess/cartridge/scripts/x` — an explicit cartridge.
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

fn push_module(hits: &mut Vec<Hit>, candidate: &Path) {
    if let Some(path) = existing_module(candidate) {
        hits.push(path.into());
    }
}

/// `x` resolves to `x`, `x.js`, `x.json`, `x.ds` or `x/index.js`, as SFCC does.
fn existing_module(candidate: &Path) -> Option<PathBuf> {
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
    let bundles: Vec<PathBuf> = workspace
        .cartridges_from(from)
        .into_iter()
        .map(|cartridge| {
            cartridge
                .cartridge_dir()
                .join("templates")
                .join("resources")
                .join(&file_name)
        })
        .filter(|path| path.is_file())
        .collect();

    let defining: Vec<Hit> = bundles
        .iter()
        .filter_map(|path| {
            line_of_key(path, key).map(|line| Hit {
                path: path.clone(),
                line,
            })
        })
        .collect();

    // A key nobody defines is usually a typo; still offer the bundles it would
    // belong to rather than nothing at all.
    if defining.is_empty() {
        return bundles.into_iter().map(Hit::from).collect();
    }
    defining
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
}
