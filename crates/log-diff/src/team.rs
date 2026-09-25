//! What the team decided about signatures: which do not matter, and which
//! already have a ticket.
//!
//! It lives next to the ledgers in the ledger repository, as `team.json`, and
//! is edited by people - by pull request, or by `log-diff ticket` - never by
//! the scheduled run, which only reads it. Keeping it out of the ledgers means
//! a person's edit and a run's commit never touch the same file.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// The team file.
pub struct Team {
    /// Signature id -> why it does not matter. A muted signature is recorded
    /// like any other, but never reported: not as new, not as a spike.
    #[serde(default)]
    pub muted: BTreeMap<String, Mute>,
    /// Signature id -> the ticket that tracks it.
    #[serde(default)]
    pub tickets: BTreeMap<String, Ticket>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Why a signature is muted for everyone.
pub struct Mute {
    /// Why it does not matter.
    #[serde(default)]
    pub reason: String,
    /// Who said so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A ticket tracking a signature.
pub struct Ticket {
    /// Its key, `ABC-123`.
    pub key: String,
    /// Where it can be read.
    pub url: String,
}

impl Team {
    /// The team file at `path`; an empty one when there is none.
    pub fn load(path: &Path) -> Result<Team> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .with_context(|| format!("cannot read {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Team::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// The team file at `path`, or an empty one when no path is given.
    pub fn load_optional(path: Option<&Path>) -> Result<Team> {
        match path {
            Some(path) => Team::load(path),
            None => Ok(Team::default()),
        }
    }

    /// Write it back, pretty, the way a person would want to review it.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(path, json).with_context(|| format!("cannot write {}", path.display()))
    }

    /// Whether the signature is muted for everyone.
    pub fn mutes(&self, id: &str) -> bool {
        self.muted.contains_key(id)
    }

    /// The muted ids, for the calls that take a list.
    pub fn muted_ids(&self) -> Vec<&str> {
        self.muted.keys().map(String::as_str).collect()
    }
}
