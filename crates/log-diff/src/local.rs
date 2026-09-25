//! On a developer's machine: read the sandbox log since the last look, and
//! report what neither the team nor this developer has seen before.
//!
//! `check` is one pass and `watch` is the same pass on a timer, so a hook that
//! runs while a watcher is going finds the ledger the watcher left and does
//! not report the same thing twice.
//!
//! A signature reported is pending until it is dealt with: acknowledged, or
//! not logged again for as long as `--expire` says, after which it resolves on
//! its own. Only pending signatures expire. A resolved one that is logged
//! again comes back - pending once more, and able to expire again - so being
//! wrong about a fix costs one more notification, never a missed one. A muted
//! one, or one taken in with a baseline, is never reported again.

use crate::finding::{Finding, findings};
use crate::ledger::{Known, Ledger, Seen, Standing, by_importance, load_shared, local_dir};
use crate::notify::{self, headline};
use crate::output::{self, Badge, Card, Tone, status};
use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use sfcc_core::config::Config;
use sfcc_core::logs::{self, Mark};
use sfcc_core::webdav::{Availability, Dav};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `watch` trusts its copy of the team ledger.
const TEAM_REFRESH: Duration = Duration::from_secs(300);
/// Longest message on a problem line.
const PROBLEM_CHARS: usize = 200;

/// Where to read, and where to remember.
pub struct Local {
    /// The sandbox.
    pub config: Config,
    /// Log levels to read.
    pub levels: Vec<String>,
    /// This developer's ledger.
    pub state: PathBuf,
    /// The team's ledger, a path or a URL.
    pub shared: Option<String>,
    /// Whether to pop a desktop notification for what is new.
    pub desktop: bool,
    /// How long a pending signature stays pending without being logged again.
    /// `None` keeps it until it is acknowledged.
    pub expire: Option<Duration>,
}

/// What one pass found.
pub struct Outcome {
    /// Signatures seen for the first time in this pass.
    pub new: Vec<Finding>,
    /// Signatures resolved before, and logged again in this pass.
    pub back: Vec<Finding>,
    /// Everything reported and not acknowledged, new, back or older.
    pub pending: Vec<(String, Known)>,
    /// Pending signatures this pass resolved for not having been logged in time.
    pub expired: usize,
}

impl Outcome {
    fn badge(&self, id: &str) -> Badge {
        let has = |found: &[Finding]| found.iter().any(|finding| finding.signature.id == id);
        match (has(&self.new), has(&self.back)) {
            (true, _) => Badge::New,
            (_, true) => Badge::Back,
            _ => Badge::Pending,
        }
    }
}

/// Whether the sandbox can be read right now. `None` when it cannot, which
/// is not worth failing over: there is simply nothing to check.
pub async fn reachable(dav: &Dav) -> Result<Option<String>> {
    match dav.availability().await {
        Availability::Ready | Availability::MissingCodeVersion => Ok(None),
        Availability::Unauthorized => bail!("the instance rejected the credentials in dw.json"),
        Availability::Unavailable(reason) => Ok(Some(reason)),
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Where the status of a sandbox's checks is written: `status/<identity>.json`
/// in log-diff's own folder, the one layout the language server has to share.
fn status_path(config: &Config) -> PathBuf {
    local_dir()
        .join("status")
        .join(format!("{}.json", config.identity()))
}

impl Local {
    /// The team's ledger, and a warning when it had to make do.
    pub async fn team(&self) -> Result<Ledger> {
        let Some(source) = &self.shared else {
            return Ok(Ledger::default());
        };
        let (ledger, warning) = load_shared(source, &local_dir().join("shared-cache.json")).await?;
        if let Some(warning) = warning {
            status(Tone::Warn, &warning);
        }
        Ok(ledger)
    }

    /// Read what was logged since the last pass and record it.
    ///
    /// With `baseline`, everything found is taken as already known: the way to
    /// start from a sandbox that has been failing for reasons of its own.
    pub async fn pass(&self, dav: &Dav, team: &Ledger, baseline: bool) -> Result<Outcome> {
        let host = &self.config.hostname;
        let from = Ledger::load(&self.state)?
            .cursor_for(host)
            .cloned()
            .unwrap_or_else(Mark::start_of_today);
        let read = logs::since(dav, &from, &self.levels).await?;

        // Loaded again after the read, which is where the time goes: a hook
        // that ran meanwhile has already recorded what it found.
        let mut mine = Ledger::load(&self.state)?;
        let (mut new, mut back) = (Vec::new(), Vec::new());
        for finding in findings(&read.entries) {
            if team.knows(&finding.signature.id) {
                continue;
            }
            match mine.observe_local(&finding, baseline) {
                Seen::New => new.push(finding),
                Seen::Back => back.push(finding),
                Seen::Known => {}
            }
        }

        let expired = match self.expire {
            Some(expire) => {
                let cutoff = Utc::now() - chrono::Duration::from_std(expire).unwrap_or_default();
                // What this pass reports is shown at least once, however old.
                let spared: Vec<&str> = new
                    .iter()
                    .chain(&back)
                    .map(|finding: &Finding| finding.signature.id.as_str())
                    .collect();
                mine.expire(
                    &cutoff.to_rfc3339_opts(SecondsFormat::Secs, true),
                    &now(),
                    &spared,
                )
            }
            None => 0,
        };
        mine.advance(host, read.next);
        mine.save(&self.state)?;

        let pending: Vec<(String, Known)> = mine
            .pending(team)
            .map(|(id, known)| (id.clone(), known.clone()))
            .collect();
        self.write_status(pending.len(), new.len() + back.len());
        Ok(Outcome {
            new,
            back,
            pending,
            expired,
        })
    }

    /// What an editor shows: how many signatures are pending for this
    /// checkout, in a file next to the ledger, one per sandbox. The ISML
    /// language server reads it for Zed's status bar. Never fails a pass.
    fn write_status(&self, pending: usize, fresh: usize) {
        let path = status_path(&self.config);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let status = serde_json::json!({
            "cartridges": self.config.cartridges_dir.to_string_lossy(),
            "hostname": self.config.hostname,
            "pending": pending,
            "new": fresh,
            "at": Utc::now().timestamp(),
        });
        let _ = std::fs::write(path, status.to_string());
    }

    /// Tell whoever is looking: the terminal always, the desktop when asked.
    pub fn report(&self, outcome: &Outcome) {
        let host = &self.config.hostname;
        let (new, back, pending) = (outcome.new.len(), outcome.back.len(), outcome.pending.len());
        let fresh = new + back;

        if output::problems() {
            // The begin and end lines a background problem matcher waits for.
            status(Tone::Info, &format!("checking {host}"));
            for (id, known) in &outcome.pending {
                println!("{}", self.problem(id, known));
            }
            status(
                Tone::Info,
                &match (fresh, pending) {
                    (0, 0) => "up to date".to_string(),
                    (0, pending) => format!("{pending} pending, nothing new"),
                    (fresh, pending) => format!("{fresh} new, {pending} pending"),
                },
            );
        } else {
            let mut parts = Vec::new();
            if new > 0 {
                parts.push(format!(
                    "{new} new error{}",
                    if new == 1 { "" } else { "s" }
                ));
            }
            if back > 0 {
                parts.push(format!("{back} back"));
            }
            match (fresh, pending) {
                (0, 0) => status(Tone::Ok, &format!("{host} · nothing new")),
                (0, pending) => status(
                    Tone::Pending,
                    &format!("{host} · nothing new, {pending} still pending"),
                ),
                (fresh, pending) => status(
                    Tone::New,
                    &format!(
                        "{} on {host}{}",
                        parts.join(", "),
                        match pending > fresh {
                            true => format!(" · {pending} pending in all"),
                            false => String::new(),
                        }
                    ),
                ),
            }

            // What turned up in this pass first, then what is still waiting;
            // within each, the most important first.
            let mut shown: Vec<&(String, Known)> = outcome.pending.iter().collect();
            shown.sort_by(|(left_id, left), (right_id, right)| {
                let waiting = |id: &str| outcome.badge(id) == Badge::Pending;
                waiting(left_id)
                    .cmp(&waiting(right_id))
                    .then(by_importance(left, right))
            });
            for (id, known) in shown {
                println!();
                println!("{}", output::card(&card_of(id, known, outcome.badge(id))));
            }
            if pending > 0 {
                println!();
                status(
                    Tone::Info,
                    "`log-diff ack <id>` once fixed, `log-diff ack --mute <id>` if it does not matter",
                );
            }
        }
        if outcome.expired > 0 {
            status(
                Tone::Ok,
                &format!(
                    "{} pending signature{} not logged for {} resolved on {}",
                    outcome.expired,
                    if outcome.expired == 1 { "" } else { "s" },
                    self.expire.map(worded).unwrap_or_default(),
                    if outcome.expired == 1 {
                        "its own"
                    } else {
                        "their own"
                    },
                ),
            );
        }

        if self.desktop {
            let headline_of = |finding: &Finding| {
                let signature = &finding.signature;
                headline(
                    &signature.label,
                    signature.exception_class.as_deref(),
                    signature.location.as_deref(),
                )
            };
            let headlines: Vec<String> = outcome
                .new
                .iter()
                .map(headline_of)
                .chain(
                    outcome
                        .back
                        .iter()
                        .map(|finding| format!("Back: {}", headline_of(finding))),
                )
                .collect();
            notify::desktop(&self.config.hostname, &headlines);
        }
    }

    /// `path:line: error: [level] message (id)` - the shape a compiler prints,
    /// which every editor's problem matcher already reads. The path is the
    /// local file when the frame names one this checkout has.
    fn problem(&self, id: &str, known: &Known) -> String {
        let (file, line) = self.local_position(known.location.as_deref());
        let head = known.example.lines().next().unwrap_or_default();
        // The thread and the category are for the ledger; a problem list wants the message.
        let message: String = head
            .split_once("[] ")
            .map_or(head, |(_, message)| message)
            .chars()
            .take(PROBLEM_CHARS)
            .collect();
        format!(
            "{}:{line}: error: [{}] {message} ({id})",
            file.display(),
            known.label
        )
    }

    fn local_position(&self, location: Option<&str>) -> (PathBuf, u32) {
        let fallback = (self.config.dw_json.clone(), 1);
        let Some((path, line)) = location.and_then(|location| location.rsplit_once(':')) else {
            return fallback;
        };
        let local = self
            .config
            .cartridges_dir
            .join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
        match local.is_file() {
            true => (local, line.parse().unwrap_or(1)),
            false => fallback,
        }
    }

    /// `pass` and `report` on a timer, until interrupted. Only prints when
    /// something changed, so the terminal stays quiet while nothing does.
    pub async fn watch(&self, dav: &Dav, interval: Duration) -> Result<()> {
        status(
            Tone::Info,
            &format!(
                "watching {} every {}s",
                self.config.hostname,
                interval.as_secs()
            ),
        );
        let mut team: Option<(Instant, Ledger)> = None;
        let mut shown: Option<Vec<String>> = None;
        let mut offline = false;

        loop {
            match reachable(dav).await? {
                Some(reason) => {
                    if !offline {
                        status(
                            Tone::Warn,
                            &format!("instance unreachable ({reason}) - waiting"),
                        );
                        offline = true;
                    }
                }
                None => {
                    if offline {
                        status(Tone::Ok, "instance is back");
                        offline = false;
                    }
                    let stale = team
                        .as_ref()
                        .is_none_or(|(fetched, _)| fetched.elapsed() >= TEAM_REFRESH);
                    if stale {
                        team = Some((Instant::now(), self.team().await?));
                    }
                    let ledger = &team.as_ref().expect("fetched just above").1;

                    match self.pass(dav, ledger, false).await {
                        Ok(outcome) => {
                            let ids: Vec<String> =
                                outcome.pending.iter().map(|(id, _)| id.clone()).collect();
                            let fresh = !outcome.new.is_empty() || !outcome.back.is_empty();
                            if shown.as_ref() != Some(&ids) || fresh {
                                self.report(&outcome);
                                shown = Some(ids);
                            }
                        }
                        Err(error) => output::error(&format!("{error:#}")),
                    }
                }
            }
            tokio::time::sleep(interval).await;
        }
    }
}

/// `3d`, `36h`, `90m`: an expiry the way it was most likely written.
pub fn worded(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3_600 == 0 => format!("{}h", s / 3_600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// Deal with pending signatures. No ids lists them instead.
///
/// Acknowledged means fixed: logged again later, it comes back. Muted means it
/// does not matter: it is never reported again.
pub fn acknowledge(state: &Path, ids: &[String], all: bool, mute: bool) -> Result<()> {
    let mut mine = Ledger::load(state)?;
    let pending: Vec<String> = mine
        .known_signatures
        .iter()
        .filter(|(_, known)| known.pending)
        .map(|(id, _)| id.clone())
        .collect();

    if ids.is_empty() && !all {
        for id in &pending {
            let known = &mine.known_signatures[id];
            let badge = match known.back {
                true => Badge::Back,
                false => Badge::None,
            };
            println!("{}", output::card(&card_of(id, known, badge)));
            println!();
        }
        match pending.len() {
            0 => status(Tone::Ok, "nothing pending"),
            count => status(
                Tone::Pending,
                &format!(
                    "{count} pending - `log-diff ack <id>` once fixed, `--mute` if it does not matter"
                ),
            ),
        }
        return Ok(());
    }

    let resolved_at = now();
    let mut dealt = 0;
    for id in &pending {
        if (all || ids.iter().any(|wanted| id.starts_with(wanted.as_str())))
            && let Some(known) = mine.known_signatures.get_mut(id)
        {
            known.pending = false;
            known.back = false;
            known.muted = mute;
            known.resolved_at = match mute {
                true => None,
                false => Some(resolved_at.clone()),
            };
            dealt += 1;
        }
    }
    mine.save(state)?;
    match mute {
        true => status(Tone::Ok, &format!("{dealt} muted - never reported again")),
        false => status(
            Tone::Ok,
            &format!("{dealt} acknowledged - reported again if logged again"),
        ),
    }
    Ok(())
}

/// List signatures by standing, the most important first: what shows as a
/// 500, then what happened most. No standings lists them all.
pub fn list(state: &Path, wanted: &[Standing], limit: usize) -> Result<()> {
    let mine = Ledger::load(state)?;
    let standings = match wanted.is_empty() {
        true => vec![
            Standing::Pending,
            Standing::Resolved,
            Standing::Muted,
            Standing::Baseline,
        ],
        false => wanted.to_vec(),
    };

    let mut listed = 0;
    for standing in standings {
        let mut group: Vec<(&String, &Known)> = mine
            .known_signatures
            .iter()
            .filter(|(_, known)| known.standing() == standing)
            .collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by(|(_, left), (_, right)| by_importance(left, right));

        let (title, hint) = match standing {
            Standing::Pending => ("pending", "`log-diff ack <id>` once fixed"),
            Standing::Resolved => ("resolved", "reported again if logged again"),
            Standing::Muted => ("muted", "`log-diff unmute <id>` to hear of one again"),
            Standing::Baseline => (
                "baseline",
                "known from the start; `log-diff unmute <id>` to watch one",
            ),
        };
        if listed > 0 {
            println!();
        }
        status(Tone::Info, &format!("{} {title} · {hint}", group.len()));
        let shown = match limit {
            0 => group.len(),
            limit => limit.min(group.len()),
        };
        for (id, known) in &group[..shown] {
            let badge = match known.back {
                true => Badge::Back,
                false => Badge::None,
            };
            println!();
            println!("{}", output::card(&card_of(id, known, badge)));
        }
        if shown < group.len() {
            println!();
            status(
                Tone::Info,
                &format!(
                    "{} more {title} - `--limit 0` to see them all",
                    group.len() - shown
                ),
            );
        }
        listed += group.len();
    }
    if listed == 0 {
        status(Tone::Ok, "nothing to list");
    }
    Ok(())
}

/// Hear of muted signatures again: they become resolved, so the next time one
/// is logged it comes back. A baseline one can be named too, to start
/// watching it; `all` only takes the muted ones.
pub fn unmute(state: &Path, ids: &[String], all: bool) -> Result<()> {
    let mut mine = Ledger::load(state)?;
    let resolved_at = now();
    let mut unmuted = 0;
    for (id, known) in mine.known_signatures.iter_mut() {
        let named = ids.iter().any(|wanted| id.starts_with(wanted.as_str()));
        let eligible = match known.standing() {
            Standing::Muted => all || named,
            Standing::Baseline => named,
            Standing::Pending | Standing::Resolved => false,
        };
        if eligible {
            known.muted = false;
            known.resolved_at = Some(resolved_at.clone());
            unmuted += 1;
        }
    }
    mine.save(state)?;
    match unmuted {
        0 => status(
            Tone::Warn,
            "nothing unmuted - `log-diff list --muted --baseline` shows what can be",
        ),
        unmuted => status(
            Tone::Ok,
            &format!("{unmuted} unmuted - reported again the next time they are logged"),
        ),
    }
    Ok(())
}

/// A known signature, as a card.
fn card_of<'a>(id: &'a str, known: &'a Known, badge: Badge) -> Card<'a> {
    Card {
        id,
        label: &known.label,
        exception: known.exception_class.as_deref(),
        location: known.location.as_deref(),
        example: &known.example,
        count: known.count,
        first_seen: &known.first_seen,
        last_seen: Some(&known.last_seen),
        badge,
        deploy: None,
        serious: known.serious(),
    }
}
