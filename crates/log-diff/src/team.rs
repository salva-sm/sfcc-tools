//! `team.json` is edited by people, never by the scheduled run: kept out of the ledgers so a
//! person's edit and a run's commit never touch the same file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Team {
    /// Recorded like any other, but never reported: not as new, not as a spike.
    #[serde(default)]
    pub muted: BTreeMap<String, Mute>,
    #[serde(default)]
    pub tickets: BTreeMap<String, Ticket>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mute {
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub key: String,
    pub url: String,
}

impl Team {
    pub fn load(path: &Path) -> Result<Team> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .with_context(|| format!("cannot read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Team::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn load_optional(path: Option<&Path>) -> Result<Team> {
        match path {
            Some(path) => Team::load(path),
            None => Ok(Team::default()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(path, json).with_context(|| format!("cannot write {}", path.display()))
    }

    pub fn mutes(&self, id: &str) -> bool {
        self.muted.contains_key(id)
    }

    pub fn muted_ids(&self) -> Vec<&str> {
        self.muted.keys().map(String::as_str).collect()
    }
}
