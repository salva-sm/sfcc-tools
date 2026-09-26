//! On CI, against a shared instance: read the log since the last run, lay
//! each new signature at the deploy that was live when it was first logged,
//! count every signature per day, and write the team's ledger.
//!
//! Several merges can land between two deploys, and a deploy's errors only
//! show up once someone uses what it shipped - often after the next deploy has
//! been dispatched. So a signature is not blamed on the deploy that triggered
//! the run, but on the one whose window its first timestamp falls in, and the
//! suspects are the commits between that deploy and the one before it.
//!
//! A known signature is news again when it spikes: logged far more today
//! than on an average day of the week before, the way a regression of an old
//! failure looks.

use crate::finding::findings;
use crate::ledger::{Ledger, serious};
use crate::notify::{Report, ReportDeploy, ReportItem, ReportSpike};
use crate::team::Team;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use sfcc_core::config::Config;
use sfcc_core::logs::{self, Mark};
use sfcc_core::webdav::Dav;
use std::path::PathBuf;

/// What `run` was told.
pub struct RunOptions {
    /// Log levels to read.
    pub levels: Vec<String>,
    /// The team's ledger.
    pub state: PathBuf,
    /// The commit just deployed, when this run is for a deploy.
    pub sha: Option<String>,
    /// Its CI build number.
    pub build: Option<u64>,
    /// When it went live, when that was not just now.
    pub at: Option<String>,
    /// Where to write the report for `notify`.
    pub report: Option<PathBuf>,
    /// A compare link, with `{from}` and `{to}` for the two shas.
    pub compare_url: Option<String>,
    /// A link to a line of code, with `{sha}`, `{path}` and `{line}`.
    pub code_url: Option<String>,
    /// How many days before today the first run learns from. Ignored once
    /// the ledger has a cursor.
    pub baseline_days: u32,
    /// The team file: what is muted for everyone.
    pub team: Option<PathBuf>,
    /// Fewest records in a day that can make a spike.
    pub spike_min: u64,
    /// How many times its usual day a signature must be logged to spike.
    pub spike_factor: f64,
    /// Name the report after this environment rather than the host.
    pub environment: Option<String>,
}

/// What a run did.
pub struct Outcome {
    /// What it found new, and what spiked.
    pub report: Report,
    /// Whether this was the first run, which learns instead of reporting.
    pub baseline: bool,
    /// The day the first run started learning from, `YYYYMMDD`.
    pub baseline_from: String,
    /// Whether the ledger was signed another way, and this run learned the
    /// new signatures instead of reporting them.
    pub resigned: bool,
    /// Signatures in the ledger after the run.
    pub known: usize,
}

/// One run.
pub async fn run(config: &Config, dav: &Dav, options: &RunOptions) -> Result<Outcome> {
    let host = &config.hostname;
    let mut ledger = Ledger::load(&options.state)?;
    let team = Team::load_optional(options.team.as_deref())?;

    if let Some(sha) = &options.sha {
        let at = match &options.at {
            Some(at) => DateTime::parse_from_rfc3339(at)
                .with_context(|| format!("--at {at:?} is not an RFC 3339 timestamp"))?
                .with_timezone(&Utc),
            None => Utc::now(),
        };
        // A dispatch retried, or a deploy already recorded by `log-diff
        // deploy`, is the same deploy, not a second one.
        if !ledger.has_deploy(sha) && !options.build.is_some_and(|build| ledger.has_build(build)) {
            ledger.record_deploy(sha, options.build, at);
        }
    }

    // With nothing to compare against, the first run learns what the
    // instance already logs instead of reporting all of it as new - today's
    // log, or as many days back as asked. The more history, the fewer of
    // the failures that only turn up once a week get blamed on a deploy.
    let (from, baseline) = match ledger.cursor_for(host) {
        Some(cursor) => (cursor.clone(), false),
        None => (Mark::days_back(options.baseline_days), true),
    };
    let baseline_from = from.day.clone();
    // Signatures computed another way are the same failures under new ids:
    // learned, like a baseline, but from where the cursor is.
    let resigned = !baseline && ledger.resigned();
    let mut entries = Vec::new();
    if baseline && options.baseline_days > 0 {
        // The days the instance has already archived are only in log_archive.
        entries.extend(logs::archived(dav, &from.day, &options.levels).await?);
    }
    let read = logs::since(dav, &from, &options.levels).await?;
    entries.extend(read.entries);
    logs::order(&mut entries);

    let mut report = Report {
        instance: options.environment.clone().unwrap_or_else(|| host.clone()),
        generated: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        new: Vec::new(),
        spikes: Vec::new(),
    };
    for finding in findings(&entries) {
        ledger.record_daily(&finding);
        let live = match baseline {
            true => None,
            false => ledger.deploy_at(&finding.first),
        };
        let sha = live.map(|index| ledger.deploy_log[index].sha.clone());
        let new = ledger.observe(&finding, sha.as_deref(), false);
        if !new || baseline || resigned || team.mutes(&finding.signature.id) {
            continue;
        }

        let deploy = live.map(|index| {
            ledger.deploy_log[index]
                .new_signatures
                .push(finding.signature.id.clone());
            let deploy = &ledger.deploy_log[index];
            let previous = index
                .checked_sub(1)
                .map(|before| ledger.deploy_log[before].sha.clone());
            ReportDeploy {
                sha: deploy.sha.clone(),
                build: deploy.build,
                compare_url: compare_url(
                    options.compare_url.as_deref(),
                    previous.as_deref(),
                    &deploy.sha,
                ),
                previous_sha: previous,
            }
        });
        let signature = &finding.signature;
        report.new.push(ReportItem {
            id: signature.id.clone(),
            label: signature.label.clone(),
            exception_class: signature.exception_class.clone(),
            location: signature.location.clone(),
            example: signature.example(),
            count: finding.count,
            first_seen: finding.first.clone(),
            code_url: code_url(
                options.code_url.as_deref(),
                deploy.as_ref().map(|deploy| deploy.sha.as_str()),
                signature.location.as_deref(),
            ),
            deploy,
        });
    }

    if !baseline && !resigned {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let quiet = team.muted_ids();
        for spike in ledger.spikes(&today, options.spike_min, options.spike_factor, &quiet) {
            let known = &ledger.known_signatures[&spike.id];
            report.spikes.push(ReportSpike {
                id: spike.id.clone(),
                label: known.label.clone(),
                exception_class: known.exception_class.clone(),
                location: known.location.clone(),
                example: known.example.clone(),
                today: spike.today,
                usual: spike.usual,
                serious: serious(&known.label, &known.example),
                code_url: code_url(options.code_url.as_deref(), None, known.location.as_deref()),
            });
        }
    }

    ledger.advance(host, read.next);
    ledger.save(&options.state)?;
    if let Some(path) = &options.report {
        report.save(path)?;
    }
    Ok(Outcome {
        report,
        baseline,
        baseline_from,
        resigned,
        known: ledger.known_signatures.len(),
    })
}

/// The code versions on the instance, as `(written, name)`, oldest first:
/// each build is deployed to one of its own, so this is the deploy history.
pub async fn code_versions(dav: &Dav) -> Result<Vec<(String, String)>> {
    let mut versions: Vec<(String, String)> = dav
        .list(dav.root_url())
        .await?
        .into_iter()
        .filter(|entry| entry.is_dir)
        .filter_map(|entry| {
            let at = DateTime::parse_from_rfc2822(&entry.modified).ok()?;
            let at = at
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            Some((at, entry.name))
        })
        .collect();
    versions.sort();
    Ok(versions)
}

fn compare_url(template: Option<&str>, from: Option<&str>, to: &str) -> Option<String> {
    Some(template?.replace("{from}", from?).replace("{to}", to))
}

/// A link to the line a signature points at, at the deploy that brought it,
/// or at the head of the branch when there is none.
pub fn code_url(
    template: Option<&str>,
    sha: Option<&str>,
    location: Option<&str>,
) -> Option<String> {
    let (path, line) = location?.rsplit_once(':')?;
    Some(
        template?
            .replace("{sha}", sha.unwrap_or("HEAD"))
            .replace("{path}", path)
            .replace("{line}", line),
    )
}

#[cfg(test)]
mod tests {
    use super::{code_url, compare_url};

    #[test]
    fn a_compare_link_needs_both_ends() {
        let template = "https://github.com/acme/site/compare/{from}...{to}";
        assert_eq!(
            compare_url(Some(template), Some("aaa"), "bbb").as_deref(),
            Some("https://github.com/acme/site/compare/aaa...bbb")
        );
        assert_eq!(compare_url(Some(template), None, "bbb"), None);
        assert_eq!(compare_url(None, Some("aaa"), "bbb"), None);
    }

    #[test]
    fn a_code_link_points_at_the_line_at_the_deploy() {
        let template = "https://github.com/acme/site/blob/{sha}/cartridges/{path}#L{line}";
        assert_eq!(
            code_url(
                Some(template),
                Some("9451cff"),
                Some("app_x/cartridge/a.js:214")
            )
            .as_deref(),
            Some("https://github.com/acme/site/blob/9451cff/cartridges/app_x/cartridge/a.js#L214")
        );
        assert_eq!(
            code_url(Some(template), None, Some("app_x/cartridge/a.js:1")).as_deref(),
            Some("https://github.com/acme/site/blob/HEAD/cartridges/app_x/cartridge/a.js#L1")
        );
        assert_eq!(code_url(Some(template), None, None), None);
    }
}

#[cfg(test)]
#[path = "ci_tests.rs"]
mod run_tests;
