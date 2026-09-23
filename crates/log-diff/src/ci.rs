//! On CI, against the shared development instance: read the log since the
//! last run, lay each new signature at the deploy that was live when it was
//! first logged, and write the team's ledger.
//!
//! Several merges can land between two deploys, and a deploy's errors only
//! show up once someone uses what it shipped - often after the next deploy has
//! been dispatched. So a signature is not blamed on the deploy that triggered
//! the run, but on the one whose window its first timestamp falls in, and the
//! suspects are the commits between that deploy and the one before it.

use crate::finding::findings;
use crate::ledger::Ledger;
use crate::notify::{Report, ReportDeploy, ReportItem};
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
    /// How many days before today the first run learns from. Ignored once
    /// the ledger has a cursor.
    pub baseline_days: u32,
}

/// What a run did.
pub struct Outcome {
    /// What it found new.
    pub report: Report,
    /// Whether this was the first run, which learns instead of reporting.
    pub baseline: bool,
    /// The day the first run started learning from, `YYYYMMDD`.
    pub baseline_from: String,
    /// Signatures in the ledger after the run.
    pub known: usize,
}

/// One run.
pub async fn run(config: &Config, dav: &Dav, options: &RunOptions) -> Result<Outcome> {
    let host = &config.hostname;
    let mut ledger = Ledger::load(&options.state)?;

    if let Some(sha) = &options.sha {
        let at = match &options.at {
            Some(at) => DateTime::parse_from_rfc3339(at)
                .with_context(|| format!("--at {at:?} is not an RFC 3339 timestamp"))?
                .with_timezone(&Utc),
            None => Utc::now(),
        };
        // A dispatch retried is the same deploy, not a second one.
        if ledger.deploy_log.last().map(|last| &last.sha) != Some(sha) {
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
    let read = logs::since(dav, &from, &options.levels).await?;

    let mut report = Report {
        instance: host.clone(),
        generated: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        new: Vec::new(),
    };
    for finding in findings(&read.entries) {
        let live = match baseline {
            true => None,
            false => ledger.deploy_at(&finding.first),
        };
        let sha = live.map(|index| ledger.deploy_log[index].sha.clone());
        if !ledger.observe(&finding, sha.as_deref(), false) || baseline {
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
            deploy,
        });
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
        known: ledger.known_signatures.len(),
    })
}

fn compare_url(template: Option<&str>, from: Option<&str>, to: &str) -> Option<String> {
    Some(template?.replace("{from}", from?).replace("{to}", to))
}

#[cfg(test)]
mod tests {
    use super::compare_url;

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
}
