//! On a developer's machine: read the sandbox log since the last look, and
//! report what neither the team nor this developer has seen before.
//!
//! `check` is one pass and `watch` is the same pass on a timer, so a hook that
//! runs while a watcher is going finds the ledger the watcher left and does
//! not report the same thing twice.

use crate::finding::{Finding, findings};
use crate::ledger::{Known, Ledger, load_shared, local_dir};
use crate::notify::{self, headline};
use crate::output::{self, Badge, Card, Tone, status};
use anyhow::{Result, bail};
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
}

/// What one pass found.
pub struct Outcome {
    /// Signatures seen for the first time in this pass.
    pub new: Vec<Finding>,
    /// Everything reported and not acknowledged, new or not.
    pub pending: Vec<(String, Known)>,
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
        let mut new = Vec::new();
        for finding in findings(&read.entries) {
            if team.knows(&finding.signature.id) {
                continue;
            }
            if mine.observe(&finding, None, !baseline) && !baseline {
                new.push(finding);
            }
        }
        mine.advance(host, read.next);
        mine.save(&self.state)?;

        let pending = mine
            .pending(team)
            .map(|(id, known)| (id.clone(), known.clone()))
            .collect();
        Ok(Outcome { new, pending })
    }

    /// Tell whoever is looking: the terminal always, the desktop when asked.
    pub fn report(&self, outcome: &Outcome) {
        let host = &self.config.hostname;
        let (new, pending) = (outcome.new.len(), outcome.pending.len());

        if output::problems() {
            status(Tone::Info, &format!("checking {host}"));
            for (id, known) in &outcome.pending {
                println!("{}", self.problem(id, known));
            }
            status(
                Tone::Info,
                &match (new, pending) {
                    (0, 0) => "up to date".to_string(),
                    (0, pending) => format!("{pending} pending, nothing new"),
                    (new, pending) => format!("{new} new, {pending} pending"),
                },
            );
        } else {
            match (new, pending) {
                (0, 0) => status(Tone::Ok, &format!("{host} · nothing new")),
                (0, pending) => status(
                    Tone::Pending,
                    &format!("{host} · nothing new, {pending} still pending"),
                ),
                (new, pending) => status(
                    Tone::New,
                    &format!(
                        "{new} new error{} on {host}{}",
                        if new == 1 { "" } else { "s" },
                        match pending > new {
                            true => format!(" · {pending} pending in all"),
                            false => String::new(),
                        }
                    ),
                ),
            }
            // What turned up in this pass first, then what is still waiting.
            let is_new = |id: &str| outcome.new.iter().any(|finding| finding.signature.id == id);
            let mut shown: Vec<&(String, Known)> = outcome.pending.iter().collect();
            shown.sort_by_key(|(id, _)| !is_new(id));
            for (id, known) in shown {
                let badge = match is_new(id) {
                    true => Badge::New,
                    false => Badge::Pending,
                };
                println!(
                    "
{}",
                    output::card(&card_of(id, known, badge))
                );
            }
            if pending > 0 {
                println!();
                status(
                    Tone::Info,
                    "`log-diff ack <id>` or `log-diff ack --all` once dealt with",
                );
            }
        }

        if self.desktop {
            let headlines: Vec<String> = outcome
                .new
                .iter()
                .map(|finding| {
                    let signature = &finding.signature;
                    headline(
                        &signature.label,
                        signature.exception_class.as_deref(),
                        signature.location.as_deref(),
                    )
                })
                .collect();
            notify::desktop(&self.config.hostname, &headlines);
        }
    }

    /// `path:line: error: [level] message (id)` - the shape a compiler prints, which
    /// every editor's problem matcher already reads. The path is the local
    /// file when the frame names one this checkout has.
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
                            if shown.as_ref() != Some(&ids) || !outcome.new.is_empty() {
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

/// Mark pending signatures as dealt with. No ids lists what is pending.
pub fn acknowledge(state: &Path, ids: &[String], all: bool) -> Result<()> {
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
            println!(
                "{}
",
                output::card(&card_of(id, known, Badge::None))
            );
        }
        match pending.len() {
            0 => status(Tone::Ok, "nothing pending"),
            count => status(
                Tone::Pending,
                &format!("{count} pending - `log-diff ack <id>` or `log-diff ack --all`"),
            ),
        }
        return Ok(());
    }

    let mut acknowledged = 0;
    for id in &pending {
        if (all || ids.iter().any(|wanted| id.starts_with(wanted.as_str())))
            && let Some(known) = mine.known_signatures.get_mut(id)
        {
            known.pending = false;
            acknowledged += 1;
        }
    }
    mine.save(state)?;
    status(Tone::Ok, &format!("{acknowledged} acknowledged"));
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
    }
}
