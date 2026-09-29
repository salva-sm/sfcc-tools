use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sfcc_core::config::Config;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use xxhash_rust::xxh3::Xxh3;

const HASH_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Entry {
    pub hash: u64,
    pub size: u64,
    pub modified_millis: i64,
    /// The sandbox's `getetag` right after this checkout wrote the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Epoch seconds, for files sent in bulk, whose etag is never read back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
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
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
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

    pub fn record(&mut self, relative: String, mut entry: Entry) {
        entry
            .sent_at
            .get_or_insert_with(|| chrono::Utc::now().timestamp());
        self.files.insert(relative, entry);
    }

    /// The file itself, or every file under it when it is a folder.
    pub fn under<'a>(&'a self, path: &str) -> impl Iterator<Item = (&'a String, &'a Entry)> {
        let folder = format!("{path}/");
        let path = path.to_string();
        self.files
            .iter()
            .filter(move |(key, _)| **key == path || key.starts_with(&folder))
    }

    pub fn forget(&mut self, relative: &str) {
        self.files.remove(relative);
    }

    pub fn forget_prefix(&mut self, prefix: &str) {
        let owned = format!("{prefix}/");
        self.files
            .retain(|key, _| key != prefix && !key.starts_with(&owned));
    }
}

pub fn manifest_path(config: &Config) -> PathBuf {
    sfcc_core::state::upload::manifest_path(&config.identity())
}

pub fn hash_file(path: &Path) -> Result<u64> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Xxh3::new();
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.digest())
}
