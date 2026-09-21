//! Discovery of the SFCC cartridges in the open folder.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 6;
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
    pub name: String,
    /// The directory holding `cartridge/`, e.g. `.../cartridges/app_common_eu_guess`.
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
}

impl Workspace {
    pub fn scan(roots: &[PathBuf]) -> Self {
        let mut workspace = Workspace::default();
        for root in roots {
            workspace.visit(root, 0);
        }
        workspace.cartridges.sort_by(|a, b| a.name.cmp(&b.name));
        workspace.cartridges.dedup_by(|a, b| a.root == b.root);
        workspace
    }

    /// The cartridge a file belongs to, if any.
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

fn cartridge_at(cartridge_dir: &Path) -> Option<Cartridge> {
    let root = cartridge_dir.parent()?.to_path_buf();
    let name = root.file_name()?.to_str()?.to_string();
    Some(Cartridge { name, root })
}
