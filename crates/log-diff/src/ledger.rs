//! What is already known: every signature seen, when, how often, and which
//! deploy brought it.
//!
//! The same shape serves twice. The team's ledger lives in its own repository
//! and only CI writes to it. Each developer has a local one next to it, which
//! only remembers what they were already told - read from the team's, written
//! to their own, never the other way round.

use crate::finding::Finding;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sfcc_core::logs::Mark;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The format this build reads and writes. A newer one is refused rather
/// than rewritten without the fields this build does not know.
pub const VERSION: u32 = 1;
/// Deploys remembered. Old ones only matter through the signatures they
/// introduced, which keep their sha.
const DEPLOYS_KEPT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A ledger file.
pub struct Ledger {
    /// The format version.
    #[serde(default = "version")]
    pub version: u32,
    /// The instance the cursor belongs to. A cursor from another instance
    /// means nothing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// Where the last read of the log ended.
    #[serde(default)]
    pub cursor: Option<Mark>,
    /// Signature id -> what is known about it.
    #[serde(default)]
    pub known_signatures: BTreeMap<String, Known>,
    /// Deploys seen, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy_log: Vec<Deploy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// One signature, as far as the ledger knows it.
pub struct Known {
    /// The level it was logged at.
    pub label: String,
    /// The innermost exception named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_class: Option<String>,
    /// The top script frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// One occurrence, scrubbed.
    pub example: String,
    /// When it was first logged.
    pub first_seen: String,
    /// When it was last logged.
    pub last_seen: String,
    /// How many times, while the ledger was watching.
    pub count: u64,
    /// The deploy that was live when it first showed up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_deploy_sha: Option<String>,
    /// Local only: reported, and not acknowledged yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
    /// Local only: pending again, after it had been resolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub back: bool,
    /// Local only: when it was acknowledged or expired. Logged again after
    /// that, it is back. A signature neither pending nor resolved - muted, or
    /// taken in with a baseline - is never reported again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// Local only: muted on purpose, as opposed to taken in with a baseline.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
}

/// Where a signature stands in a developer's own ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// Reported, and not dealt with yet.
    Pending,
    /// Acknowledged or expired; reported again if it is logged again.
    Resolved,
    /// Muted on purpose: never reported again.
    Muted,
    /// Taken in with a baseline, or dealt with before log-diff kept track of
    /// how: never reported again.
    Baseline,
}

impl Known {
    /// Where it stands.
    pub fn standing(&self) -> Standing {
        match (self.pending, self.muted, self.resolved_at.is_some()) {
            (true, _, _) => Standing::Pending,
            (_, true, _) => Standing::Muted,
            (_, _, true) => Standing::Resolved,
            _ => Standing::Baseline,
        }
    }

    /// Whether it is the kind of failure a shopper sees as an error page: an
    /// uncaught `error` or a `fatal` - which SFCC answers with a 500 - or a
    /// record that says 500 itself.
    pub fn serious(&self) -> bool {
        serious(&self.label, &self.example)
    }
}

/// See [`Known::serious`].
pub fn serious(label: &str, example: &str) -> bool {
    let head = example.lines().next().unwrap_or_default();
    matches!(label, "error" | "fatal")
        || head.contains(" 500")
        || head.contains("Internal Server Error")
}

/// Most important first: what shows as a 500, then what happened most, then
/// what happened last.
pub fn by_importance(left: &Known, right: &Known) -> std::cmp::Ordering {
    right
        .serious()
        .cmp(&left.serious())
        .then(right.count.cmp(&left.count))
        .then(right.last_seen.cmp(&left.last_seen))
}

/// What recording a finding in a developer's own ledger made of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seen {
    /// Never seen before.
    New,
    /// Resolved, and logged again since.
    Back,
    /// Already pending, muted, or taken in with a baseline.
    Known,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A deploy to the instance.
pub struct Deploy {
    /// The commit deployed.
    pub sha: String,
    /// The CI build that deployed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<u64>,
    /// When it went live, RFC 3339.
    pub timestamp: String,
    /// Signatures first seen while it was live.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_signatures: Vec<String>,
}

fn version() -> u32 {
    VERSION
}

impl Default for Ledger {
    fn default() -> Ledger {
        Ledger {
            version: VERSION,
            instance: None,
            cursor: None,
            known_signatures: BTreeMap::new(),
            deploy_log: Vec::new(),
        }
    }
}

impl Ledger {
    /// The ledger at `path`; an empty one when there is no file yet.
    pub fn load(path: &Path) -> Result<Ledger> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                Ledger::parse(&raw).with_context(|| format!("cannot read {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    /// A ledger from its JSON.
    pub fn parse(raw: &str) -> Result<Ledger> {
        let ledger: Ledger = serde_json::from_str(raw).context("not a ledger")?;
        if ledger.version > VERSION {
            bail!(
                "the ledger is format {}, and this log-diff only knows up to {VERSION} - update it",
                ledger.version
            );
        }
        Ok(ledger)
    }

    /// Write the ledger whole or not at all: a watcher and a hook may be at
    /// it at the same time, and half a file is worse than a lost update.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        let partial = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&partial, json)
            .with_context(|| format!("cannot write {}", partial.display()))?;
        std::fs::rename(&partial, path)
            .with_context(|| format!("cannot replace {}", path.display()))
    }

    /// Where the last read ended, if it was a read of this instance.
    pub fn cursor_for(&self, instance: &str) -> Option<&Mark> {
        match self.instance.as_deref() == Some(instance) {
            true => self.cursor.as_ref(),
            false => None,
        }
    }

    /// Move the cursor, and claim it for `instance`.
    pub fn advance(&mut self, instance: &str, cursor: Mark) {
        self.instance = Some(instance.to_string());
        self.cursor = Some(cursor);
    }

    /// Whether the signature is known.
    pub fn knows(&self, id: &str) -> bool {
        self.known_signatures.contains_key(id)
    }

    /// Count a finding. Returns whether it was new to this ledger.
    pub fn observe(&mut self, finding: &Finding, deploy: Option<&str>, pending: bool) -> bool {
        if let Some(known) = self.known_signatures.get_mut(&finding.signature.id) {
            known.count += finding.count;
            if finding.last > known.last_seen {
                known.last_seen = finding.last.clone();
            }
            return false;
        }

        let signature = &finding.signature;
        self.known_signatures.insert(
            signature.id.clone(),
            Known {
                label: signature.label.clone(),
                exception_class: signature.exception_class.clone(),
                location: signature.location.clone(),
                example: signature.example(),
                first_seen: finding.first.clone(),
                last_seen: finding.last.clone(),
                count: finding.count,
                first_deploy_sha: deploy.map(str::to_string),
                pending,
                back: false,
                resolved_at: None,
                muted: false,
            },
        );
        true
    }

    /// Count a finding in a developer's own ledger, where a resolved signature
    /// logged again after it was resolved is back. With `baseline`, a new one
    /// is taken in as known and never reported.
    pub fn observe_local(&mut self, finding: &Finding, baseline: bool) -> Seen {
        let Some(known) = self.known_signatures.get_mut(&finding.signature.id) else {
            self.observe(finding, None, !baseline);
            return match baseline {
                true => Seen::Known,
                false => Seen::New,
            };
        };

        known.count += finding.count;
        if finding.last > known.last_seen {
            known.last_seen = finding.last.clone();
        }
        // Both moments are whole seconds, so the same second counts as after:
        // a return missed is worse than one reported a second early.
        let returned = !known.pending
            && known
                .resolved_at
                .as_ref()
                .is_some_and(|resolved| finding.last >= *resolved);
        if !returned {
            return Seen::Known;
        }
        known.pending = true;
        known.back = true;
        known.resolved_at = None;
        Seen::Back
    }

    /// Resolve every pending signature last logged before `cutoff`, except
    /// the ones in `spared`, just reported and not seen by anyone yet. Returns
    /// how many. Only pending ones expire: what is resolved or muted stays so.
    pub fn expire(&mut self, cutoff: &str, now: &str, spared: &[&str]) -> usize {
        let mut expired = 0;
        for (id, known) in self.known_signatures.iter_mut() {
            if known.pending && known.last_seen.as_str() < cutoff && !spared.contains(&id.as_str())
            {
                known.pending = false;
                known.back = false;
                known.resolved_at = Some(now.to_string());
                expired += 1;
            }
        }
        expired
    }

    /// Record a deploy going live at `at`.
    pub fn record_deploy(&mut self, sha: &str, build: Option<u64>, at: DateTime<Utc>) {
        self.deploy_log.push(Deploy {
            sha: sha.to_string(),
            build,
            timestamp: at.to_rfc3339_opts(SecondsFormat::Secs, true),
            new_signatures: Vec::new(),
        });
        self.deploy_log
            .sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
        let excess = self.deploy_log.len().saturating_sub(DEPLOYS_KEPT);
        self.deploy_log.drain(..excess);
    }

    /// The deploy that was live at `moment`: the last one to go live before it.
    pub fn deploy_at(&self, moment: &str) -> Option<usize> {
        self.deploy_log
            .iter()
            .rposition(|deploy| deploy.timestamp.as_str() <= moment)
    }

    /// Signatures reported to this developer and not acknowledged, leaving
    /// out any the team has learned about since.
    pub fn pending<'a>(
        &'a self,
        team: &'a Ledger,
    ) -> impl Iterator<Item = (&'a String, &'a Known)> {
        self.known_signatures
            .iter()
            .filter(move |(id, known)| known.pending && !team.knows(id))
    }
}

/// The team's ledger, from a path or a URL. A URL is fetched with
/// `LOG_DIFF_TOKEN` or `GITHUB_TOKEN` as a bearer token when one is set, and
/// kept in `cache`: when it cannot be reached, the last copy is better than
/// treating everything the team already knows as new.
pub async fn load_shared(source: &str, cache: &Path) -> Result<(Ledger, Option<String>)> {
    if !source.starts_with("http://") && !source.starts_with("https://") {
        return Ok((Ledger::load(Path::new(source))?, None));
    }

    match fetch(source).await {
        Ok(raw) => {
            let ledger = Ledger::parse(&raw).with_context(|| format!("cannot read {source}"))?;
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(cache, raw);
            Ok((ledger, None))
        }
        Err(error) => {
            let warning = format!("cannot fetch the team ledger ({error:#})");
            match cache.is_file() {
                true => Ok((
                    Ledger::load(cache)?,
                    Some(format!("{warning}; using the copy from the last fetch")),
                )),
                false => Ok((Ledger::default(), Some(warning))),
            }
        }
    }
}

async fn fetch(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut request = client.get(url).header("User-Agent", "log-diff");
    let token = std::env::var("LOG_DIFF_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN"));
    if let Ok(token) = token.as_deref().map(str::trim)
        && !token.is_empty()
    {
        request = request
            .bearer_auth(token)
            .header("Accept", "application/vnd.github.raw");
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        bail!("HTTP {}", response.status());
    }
    Ok(response.text().await?)
}

/// Where a developer's own ledger lives unless told otherwise.
pub fn local_dir() -> PathBuf {
    if cfg!(windows)
        && let Ok(appdata) = std::env::var("APPDATA")
    {
        return PathBuf::from(appdata).join("log-diff");
    }
    if let Ok(config) = std::env::var("XDG_CONFIG_HOME")
        && !config.is_empty()
    {
        return PathBuf::from(config).join("log-diff");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("log-diff")
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
