//! One shape, two uses: the team's ledger, written only by CI, and each developer's local one,
//! which remembers what they were told. The team's is read, never written, locally.

use crate::finding::Finding;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sfcc_core::logs::Mark;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A newer format is refused rather than rewritten without the fields this build does not know.
pub const VERSION: u32 = 1;
/// Old deploys only matter through the signatures they introduced, which keep their sha.
const DEPLOYS_KEPT: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default = "version")]
    pub version: u32,
    /// [`crate::normalize::SIGNATURES`] when written; a ledger older than the field is the first.
    #[serde(default = "first_signatures")]
    pub signatures: u32,
    /// The instance the cursor belongs to; a cursor from another one means nothing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(default)]
    pub cursor: Option<Mark>,
    #[serde(default)]
    pub known_signatures: BTreeMap<String, Known>,
    /// Oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploy_log: Vec<Deploy>,
    /// Team only: day (`YYYY-MM-DD`, UTC) -> signature -> records; what spikes are measured
    /// against.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub daily: BTreeMap<String, BTreeMap<String, u64>>,
    /// Team only: when the first read, which learns instead of reporting, ended. What was
    /// first seen before it was already there, not new.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

pub const DAYS_KEPT: usize = 90;
const SPIKE_WINDOW: i64 = 7;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Known {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Scrubbed.
    pub example: String,
    pub first_seen: String,
    pub last_seen: String,
    /// Only while the ledger was watching.
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_deploy_sha: Option<String>,
    /// Local only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
    /// Local only: pending again after it had been resolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub back: bool,
    /// Local only: acknowledged or expired. Neither pending nor resolved means never reported
    /// again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    /// Local only: muted on purpose, as opposed to taken in with a baseline.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Team only: so that a day's spike is reported once, not on every run of the day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spiked_on: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spike {
    pub id: String,
    pub today: u64,
    /// Records per day over the week before.
    pub usual: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    Pending,
    Resolved,
    Muted,
    /// Taken in with a baseline, or dealt with before log-diff kept track of how.
    Baseline,
}

impl Known {
    pub fn standing(&self) -> Standing {
        match (self.pending, self.muted, self.resolved_at.is_some()) {
            (true, _, _) => Standing::Pending,
            (_, true, _) => Standing::Muted,
            (_, _, true) => Standing::Resolved,
            _ => Standing::Baseline,
        }
    }

    /// Shows as an error page: SFCC answers an uncaught `error` or a `fatal` with a 500.
    pub fn serious(&self) -> bool {
        serious(&self.label, &self.example)
    }
}

pub fn serious(label: &str, example: &str) -> bool {
    let head = example.lines().next().unwrap_or_default();
    // A failed `${}` renders empty and the page goes on; the cause is logged apart, in customerror.
    if head.contains("Error in template script") {
        return false;
    }
    // A shopper's error page comes from a storefront request, not from Business Manager or a
    // background thread such as the OIDC token refresh.
    let storefront = head.contains("PipelineCallServlet") && !head.contains("BUSINESSMGR");
    (matches!(label, "error" | "fatal") && storefront)
        || head.contains(" 500")
        || head.contains("Internal Server Error")
}

pub fn by_importance(left: &Known, right: &Known) -> std::cmp::Ordering {
    right
        .serious()
        .cmp(&left.serious())
        .then(right.count.cmp(&left.count))
        .then(right.last_seen.cmp(&left.last_seen))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seen {
    New,
    Back,
    Known,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deploy {
    pub sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<u64>,
    /// RFC 3339.
    pub timestamp: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_signatures: Vec<String>,
}

fn version() -> u32 {
    VERSION
}

fn first_signatures() -> u32 {
    1
}

impl Default for Ledger {
    fn default() -> Ledger {
        Ledger {
            version: VERSION,
            signatures: crate::normalize::SIGNATURES,
            instance: None,
            cursor: None,
            known_signatures: BTreeMap::new(),
            deploy_log: Vec::new(),
            daily: BTreeMap::new(),
            baseline: None,
        }
    }
}

impl Ledger {
    pub fn load(path: &Path) -> Result<Ledger> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                Ledger::parse(&raw).with_context(|| format!("cannot read {}", path.display()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }

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

    /// Whole or not at all: a watcher and a hook may write at once, and half a file is worse
    /// than a lost update.
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

    pub fn cursor_for(&self, instance: &str) -> Option<&Mark> {
        match self.instance.as_deref() == Some(instance) {
            true => self.cursor.as_ref(),
            false => None,
        }
    }

    /// What it knows from now on is signed the way this log-diff signs.
    pub fn advance(&mut self, instance: &str, cursor: Mark) {
        self.instance = Some(instance.to_string());
        self.cursor = Some(cursor);
        self.signatures = crate::normalize::SIGNATURES;
    }

    /// Signed another way than this log-diff signs, so all it knows would look new.
    pub fn resigned(&self) -> bool {
        self.signatures != crate::normalize::SIGNATURES && !self.known_signatures.is_empty()
    }

    pub fn knows(&self, id: &str) -> bool {
        self.known_signatures.contains_key(id)
    }

    /// Returns whether it was new to this ledger.
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
                spiked_on: None,
            },
        );
        true
    }

    pub fn record_daily(&mut self, finding: &Finding) {
        for (day, count) in &finding.per_day {
            *self
                .daily
                .entry(day.clone())
                .or_default()
                .entry(finding.signature.id.clone())
                .or_default() += count;
        }
        let excess = self.daily.len().saturating_sub(DAYS_KEPT);
        let old: Vec<String> = self.daily.keys().take(excess).cloned().collect();
        for day in old {
            self.daily.remove(&day);
        }
    }

    /// Each reported once a day. What first showed up today is new, not a spike.
    pub fn spikes(&mut self, today: &str, min: u64, factor: f64, quiet: &[&str]) -> Vec<Spike> {
        let Some(counts) = self.daily.get(today).cloned() else {
            return Vec::new();
        };
        let Ok(date) = chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
            return Vec::new();
        };
        let before: Vec<String> = (1..=SPIKE_WINDOW)
            .map(|back| {
                (date - chrono::Duration::days(back))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .collect();

        let mut spikes = Vec::new();
        for (id, today_count) in counts {
            let Some(known) = self.known_signatures.get_mut(&id) else {
                continue;
            };
            if today_count < min
                || known.first_seen.starts_with(today)
                || known.spiked_on.as_deref() == Some(today)
                || quiet.contains(&id.as_str())
            {
                continue;
            }
            let total: u64 = before
                .iter()
                .filter_map(|day| self.daily.get(day)?.get(&id))
                .sum();
            let usual = total as f64 / SPIKE_WINDOW as f64;
            if today_count as f64 >= factor * usual.max(1.0) {
                known.spiked_on = Some(today.to_string());
                spikes.push(Spike {
                    id,
                    today: today_count,
                    usual,
                });
            }
        }
        spikes.sort_by_key(|spike| std::cmp::Reverse(spike.today));
        spikes
    }

    /// With `baseline`, a new one is taken in as known and never reported.
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
        // Whole seconds, so the same second counts as after: a missed return is worse than an
        // early one.
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

    /// `spared`: just reported and not seen by anyone yet.
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

    /// Short and full shas of the same commit are the same deploy.
    pub fn has_deploy(&self, sha: &str) -> bool {
        self.deploy_log
            .iter()
            .any(|deploy| deploy.sha.starts_with(sha) || sha.starts_with(&deploy.sha))
    }

    pub fn has_build(&self, build: u64) -> bool {
        self.deploy_log
            .iter()
            .any(|deploy| deploy.build == Some(build))
    }

    pub fn deploy_at(&self, moment: &str) -> Option<usize> {
        self.deploy_log
            .iter()
            .rposition(|deploy| deploy.timestamp.as_str() <= moment)
    }

    /// Leaves out any the team has learned about since.
    pub fn pending<'a>(
        &'a self,
        team: &'a Ledger,
    ) -> impl Iterator<Item = (&'a String, &'a Known)> {
        self.known_signatures
            .iter()
            .filter(move |(id, known)| known.pending && !team.knows(id))
    }
}

/// A fetched URL is kept in `cache`: when unreachable, the last copy beats treating everything
/// the team knows as new.
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

pub async fn fetch(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut request = client.get(url).header("User-Agent", "log-diff");
    // A GitHub token only ever goes to GitHub, whatever URL is configured.
    let host = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split(['/', ':']).next())
        .unwrap_or_default();
    let github = matches!(
        host,
        "github.com" | "api.github.com" | "raw.githubusercontent.com"
    );
    if let Some(token) = github.then(crate::envs::github_token).flatten() {
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
