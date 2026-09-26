use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cartridgepath::{self, CartridgePath};
use crate::routes::Controllers;

const MAX_DEPTH: usize = 6;
const TEMPLATE_DEPTH: usize = 10;
const SKIPPED: [&str; 8] = [
    "node_modules",
    ".git",
    "static",
    "build",
    "dist",
    "coverage",
    ".zed",
    ".vscode",
];

#[derive(Debug, Clone)]
pub struct Cartridge {
    /// The directory name, which is how the cartridge path refers to it.
    pub name: String,
    /// The directory holding `cartridge/`, e.g. `.../cartridges/app_brand`.
    pub root: PathBuf,
}

impl Cartridge {
    pub fn cartridge_dir(&self) -> PathBuf {
        self.root.join("cartridge")
    }
}

#[derive(Debug, Default)]
pub struct Workspace {
    pub cartridges: Vec<Cartridge>,
    /// `cartridges/modules` folders, which hold plain CommonJS modules such as
    /// `server` that are required without a `cartridge/` segment.
    pub module_roots: Vec<PathBuf>,
    pub paths: Vec<CartridgePath>,
    /// Built on the first `template="..."` completion, not at startup: most
    /// sessions never ask, and walking every cartridge costs a second.
    templates: OnceLock<Vec<String>>,
    controllers: OnceLock<Controllers>,
    resource_keys: OnceLock<HashSet<String>>,
}

impl Workspace {
    /// The heavier indexes are left until something asks for them.
    pub fn scan(roots: &[PathBuf], settings: &serde_json::Value) -> Self {
        let mut workspace = Workspace::default();
        for root in roots {
            workspace.visit(root, 0);
        }
        workspace.cartridges.sort_by(|a, b| a.name.cmp(&b.name));
        workspace.cartridges.dedup_by(|a, b| a.root == b.root);
        workspace.paths = cartridgepath::load(roots, settings);
        workspace
    }

    pub fn cartridge_of(&self, file: &Path) -> Option<&Cartridge> {
        self.cartridges
            .iter()
            .filter(|cartridge| file.starts_with(&cartridge.root))
            .max_by_key(|cartridge| cartridge.root.as_os_str().len())
    }

    /// Every cartridge, with the one owning `file` first: `*/cartridge/...`
    /// most often means "this cartridge, then the rest of the path".
    pub fn cartridges_from(&self, file: &Path) -> Vec<&Cartridge> {
        let current = self.cartridge_of(file);
        let mut ordered: Vec<&Cartridge> = current.into_iter().collect();
        ordered.extend(self.cartridges.iter().filter(|cartridge| {
            Some(cartridge.root.as_path()) != current.map(|c| c.root.as_path())
        }));
        ordered
    }

    /// Template paths as SFCC spells them: no locale folder, no `.isml`, forward slashes.
    /// Deduplicated, since the same path exists in every overriding cartridge.
    pub fn templates(&self) -> &[String] {
        self.templates.get_or_init(|| {
            let mut paths: Vec<String> = Vec::new();
            for cartridge in &self.cartridges {
                let root = cartridge.cartridge_dir().join("templates").join("default");
                collect_templates(&root, &root, 0, &mut paths);
            }
            paths.sort();
            paths.dedup();
            paths
        })
    }

    /// Keys of every default bundle; which bundle is not recorded, since a form resolves labels against several.
    pub fn resource_keys(&self) -> &HashSet<String> {
        self.resource_keys.get_or_init(|| {
            let mut keys = HashSet::new();
            for cartridge in &self.cartridges {
                let directory = cartridge
                    .cartridge_dir()
                    .join("templates")
                    .join("resources");
                collect_keys(&directory, &mut keys);
            }
            keys
        })
    }

    pub fn controllers(&self) -> &Controllers {
        self.controllers
            .get_or_init(|| Controllers::scan(&self.cartridges))
    }

    fn visit(&mut self, dir: &Path, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if SKIPPED.contains(&name) || name.starts_with('.') {
                continue;
            }
            if name == "cartridge" {
                if let Some(cartridge) = cartridge_at(&path) {
                    self.cartridges.push(cartridge);
                }
                continue;
            }
            if name == "modules" && dir.file_name().is_some_and(|parent| parent == "cartridges") {
                self.module_roots.push(path.clone());
                continue;
            }
            self.visit(&path, depth + 1);
        }
    }
}

/// Only the suffix-free bundles: a locale file defines no key of its own.
fn collect_keys(directory: &Path, into: &mut HashSet<String>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "properties")
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.contains('_') {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') || trimmed.starts_with('!') {
                continue;
            }
            if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if !key.is_empty() {
                    into.insert(key.to_string());
                }
            }
        }
    }
}

fn collect_templates(dir: &Path, root: &Path, depth: usize, into: &mut Vec<String>) {
    if depth > TEMPLATE_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_templates(&path, root, depth + 1, into);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "isml") {
            continue;
        }
        if let Some(name) = template_name(&path, root) {
            into.push(name);
        }
    }
}

fn template_name(file: &Path, root: &Path) -> Option<String> {
    let relative = file.strip_prefix(root).ok()?.with_extension("");
    let joined = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    (!joined.is_empty()).then_some(joined)
}

fn cartridge_at(cartridge_dir: &Path) -> Option<Cartridge> {
    let root = cartridge_dir.parent()?.to_path_buf();
    let name = root.file_name()?.to_str()?.to_string();
    Some(Cartridge { name, root })
}
