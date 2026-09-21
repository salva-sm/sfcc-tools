use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::Xxh3;

const HASH_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub hash: u64,
    pub size: u64,
    pub modified_millis: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub files: HashMap<String, Entry>,
}

impl Manifest {
    pub fn load(path: &Path) -> Manifest {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let serialized = serde_json::to_string(self).context("cannot serialize the manifest")?;
        std::fs::write(path, serialized).with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn is_unchanged(&self, relative: &str, size: u64, modified_millis: i64) -> bool {
        match self.files.get(relative) {
            Some(entry) => entry.size == size && entry.modified_millis == modified_millis,
            None => false,
        }
    }

    pub fn matches_hash(&self, relative: &str, hash: u64) -> bool {
        self.files.get(relative).map(|entry| entry.hash) == Some(hash)
    }

    pub fn record(&mut self, relative: String, entry: Entry) {
        self.files.insert(relative, entry);
    }

    pub fn forget(&mut self, relative: &str) {
        self.files.remove(relative);
    }

    pub fn forget_prefix(&mut self, prefix: &str) {
        let owned = format!("{prefix}/");
        self.files.retain(|key, _| key != prefix && !key.starts_with(&owned));
    }
}

pub fn state_dir() -> PathBuf {
    if cfg!(windows) {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(local).join("prost");
        }
    }
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(state).join("prost");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local").join("state").join("prost")
}

pub fn manifest_path(config: &Config) -> PathBuf {
    state_dir().join("manifests").join(format!("{}.json", config.identity()))
}

pub fn hash_file(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.digest())
}
