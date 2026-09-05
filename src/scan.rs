use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const DEFAULT_IGNORED_NAMES: [&str; 12] = [
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    ".idea",
    ".vscode",
    ".sass-cache",
    ".cache",
    "coverage",
    ".DS_Store",
    "Thumbs.db",
    "desktop.ini",
];

const DEFAULT_IGNORED_SUFFIXES: [&str; 5] = [".swp", ".swo", ".orig", ".rej", "~"];

#[derive(Debug, Clone)]
pub struct LocalFile {
    pub relative: String,
    pub absolute: PathBuf,
    pub size: u64,
    pub modified_millis: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Ignore {
    names: HashSet<String>,
    suffixes: Vec<String>,
    prefixes: Vec<String>,
}

impl Ignore {
    pub fn load(config: &Config) -> Ignore {
        let mut ignore = Ignore {
            names: DEFAULT_IGNORED_NAMES.iter().map(|name| name.to_string()).collect(),
            suffixes: DEFAULT_IGNORED_SUFFIXES.iter().map(|suffix| suffix.to_string()).collect(),
            prefixes: Vec::new(),
        };

        let candidates = [
            config.cartridges_dir.join(".sfccignore"),
            config.dw_json.with_file_name(".sfccignore"),
        ];
        for candidate in candidates {
            let Ok(contents) = std::fs::read_to_string(&candidate) else {
                continue;
            };
            ignore.extend(&contents);
            break;
        }
        ignore
    }

    fn extend(&mut self, contents: &str) {
        for line in contents.lines() {
            let pattern = line.trim();
            if pattern.is_empty() || pattern.starts_with('#') {
                continue;
            }
            if let Some(suffix) = pattern.strip_prefix('*') {
                self.suffixes.push(suffix.to_string());
            } else if pattern.contains('/') {
                self.prefixes.push(pattern.trim_matches('/').to_string());
            } else {
                self.names.insert(pattern.to_string());
            }
        }
    }

    pub fn skips_name(&self, name: &str) -> bool {
        self.names.contains(name) || self.suffixes.iter().any(|suffix| name.ends_with(suffix.as_str()))
    }

    pub fn skips(&self, relative: &str) -> bool {
        if relative.split('/').any(|segment| self.skips_name(segment)) {
            return true;
        }
        self.prefixes.iter().any(|prefix| relative == prefix || relative.starts_with(&format!("{prefix}/")))
    }
}

pub fn cartridge_directories(config: &Config) -> Result<Vec<PathBuf>> {
    let entries = std::fs::read_dir(&config.cartridges_dir)
        .with_context(|| format!("cannot read {}", config.cartridges_dir.display()))?;

    let mut directories: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.join("cartridge").is_dir())
        .filter(|path| match &config.cartridge_filter {
            Some(names) => names.iter().any(|name| Some(name.as_str()) == file_name(path)),
            None => true,
        })
        .collect();

    if directories.is_empty() {
        bail!("no cartridges found in {}", config.cartridges_dir.display());
    }
    directories.sort();
    Ok(directories)
}

pub fn scan(config: &Config, ignore: &Ignore) -> Result<Vec<LocalFile>> {
    let mut files = Vec::new();
    for directory in cartridge_directories(config)? {
        files.extend(collect_files(&directory, &config.cartridges_dir, ignore));
    }
    Ok(files)
}

pub fn collect_files(root: &Path, base: &Path, ignore: &Ignore) -> Vec<LocalFile> {
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !ignore.skips_name(&entry.file_name().to_string_lossy()));

    let mut files = Vec::new();
    for entry in walker.filter_map(|entry| entry.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(relative) = remote_path(entry.path(), base) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        files.push(LocalFile {
            relative,
            absolute: entry.path().to_path_buf(),
            size: metadata.len(),
            modified_millis: modified_millis(&metadata),
        });
    }
    files
}

pub fn describe(path: &Path, cartridges_dir: &Path) -> Option<LocalFile> {
    let relative = remote_path(path, cartridges_dir)?;
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(LocalFile {
        relative,
        absolute: path.to_path_buf(),
        size: metadata.len(),
        modified_millis: modified_millis(&metadata),
    })
}

pub fn remote_path(path: &Path, cartridges_dir: &Path) -> Option<String> {
    let relative = path.strip_prefix(cartridges_dir).ok()?;
    let joined = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() { None } else { Some(joined) }
}

fn modified_millis(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore_with(patterns: &str) -> Ignore {
        let mut ignore = Ignore {
            names: DEFAULT_IGNORED_NAMES.iter().map(|name| name.to_string()).collect(),
            suffixes: DEFAULT_IGNORED_SUFFIXES.iter().map(|suffix| suffix.to_string()).collect(),
            prefixes: Vec::new(),
        };
        ignore.extend(patterns);
        ignore
    }

    #[test]
    fn skips_default_noise_anywhere_in_the_path() {
        let ignore = ignore_with("");
        assert!(ignore.skips("app_common_ui/node_modules/lit/index.js"));
        assert!(ignore.skips("app_common_ui/cartridge/.DS_Store"));
        assert!(!ignore.skips("app_common_ui/cartridge/client/default/js/utils.js"));
    }

    #[test]
    fn honours_names_suffixes_and_prefixes_from_sfccignore() {
        let ignore = ignore_with("# comment\n*.test.js\nfixtures\nint_analytics/cartridge/static\n");
        assert!(ignore.skips("app/cartridge/scripts/rules.test.js"));
        assert!(ignore.skips("app/cartridge/fixtures/data.json"));
        assert!(ignore.skips("int_analytics/cartridge/static/default/js/gtm.js"));
        assert!(!ignore.skips("int_analytics/cartridge/scripts/gtm.js"));
    }
}
