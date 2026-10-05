//! A deploy's errors show up once someone uses what it shipped, often after the next deploy,
//! so a signature is blamed on the deploy live at its first timestamp, not the one that ran.

use crate::finding::Findings;
use crate::ledger::{Ledger, serious};
use crate::notify::{Report, ReportDeploy, ReportItem, ReportSpike};
use crate::team::Team;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use sfcc_core::config::Config;
use sfcc_core::logs::{self, Mark};
use sfcc_core::webdav::Dav;
use std::path::PathBuf;

pub struct RunOptions {
    pub levels: Vec<String>,
    pub notify_levels: Vec<String>,
    pub state: PathBuf,
    pub sha: Option<String>,
    pub build: Option<u64>,
    pub at: Option<String>,
    pub report: Option<PathBuf>,
    pub compare_url: Option<String>,
    pub code_url: Option<String>,
    /// Ignored once the ledger has a cursor.
    pub baseline_days: u32,
    pub team: Option<PathBuf>,
    pub spike_min: u64,
    pub spike_factor: f64,
    pub environment: Option<String>,
    /// A disabled site still logs - jobs, stray requests - but nobody will fix it.
    pub skip_sites: Vec<String>,
}

pub struct Outcome {
    pub report: Report,
    /// First run: learned instead of reporting.
    pub baseline: bool,
    /// `YYYYMMDD`.
    pub baseline_from: String,
    /// `YYYY-MM-DD` of the oldest record read: how far back the instance still kept its log.
    pub oldest: Option<String>,
    /// The ledger was signed another way: learned again instead of reporting.
    pub resigned: bool,
    pub known: usize,
    /// The live log's read, archive left out.
    pub reading: logs::Reading,
}
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
        // A retried dispatch, or one already recorded by `log-diff deploy`, is the same deploy.
        if !ledger.has_deploy(sha) && !options.build.is_some_and(|build| ledger.has_build(build)) {
            ledger.record_deploy(sha, options.build, at);
        }
    }

    // The more history the baseline has, the fewer weekly failures get blamed on a deploy.
    let (from, baseline) = match ledger.cursor_for(host) {
        Some(cursor) => (cursor.clone(), false),
        None => (Mark::days_back(options.baseline_days), true),
    };
    let baseline_from = from.day.clone();
    // Signatures computed another way are the same failures under new ids: learn them again.
    let resigned = !baseline && ledger.resigned();
    let day_of = |moment: Option<DateTime<Utc>>| moment.map(|at| at.format("%Y-%m-%d").to_string());
    let mut oldest: Option<String> = None;
    if baseline && options.baseline_days > 0 {
        // Days the instance already archived are only in log_archive, learned one file at a time.
        for file in logs::archive_files(dav, &from.day, &options.levels).await? {
            let mut found = Findings::skipping(&options.skip_sites);
            let mut first: Option<DateTime<Utc>> = None;
            logs::read_archived_each(dav, &file, |entry| {
                first = first.into_iter().chain(entry.moment_utc()).min();
                found.add(&entry);
            })
            .await?;
            oldest = oldest.into_iter().chain(day_of(first)).min();
            for finding in found.done() {
                ledger.record_daily(&finding);
                ledger.observe(&finding, None, false);
            }
        }
    }
    let mut live = Findings::skipping(&options.skip_sites);
    let mut first: Option<DateTime<Utc>> = None;
    let (next, reading) = logs::since_each(dav, &from, &options.levels, |entry| {
        first = first.into_iter().chain(entry.moment_utc()).min();
        live.add(&entry);
    })
    .await?;
    oldest = oldest.into_iter().chain(day_of(first)).min();
    // Read a file after another: in the order each first happened, as the report lists them.
    let mut live = live.done();
    live.sort_by(|left, right| left.first.cmp(&right.first));

    let mut report = Report {
        instance: options.environment.clone().unwrap_or_else(|| host.clone()),
        generated: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        new: Vec::new(),
        spikes: Vec::new(),
    };
    for mut finding in live {
        ledger.record_daily(&finding);
        // The code each record's line is a line of, whether or not the deploy is to blame.
        let deployed = |moment: &str| {
            ledger
                .deploy_at(moment)
                .map(|index| ledger.deploy_log[index].sha.clone())
        };
        let (first_sha, last_sha) = (deployed(&finding.first), deployed(&finding.last));
        if let Some(record) = finding.first_record.as_mut() {
            record.sha = first_sha;
        }
        if let Some(record) = finding.last_record.as_mut() {
            record.sha = last_sha;
        }
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
        // Still laid at its deploy, only not reported.
        if !logs::has_level(&options.notify_levels, &signature.label) {
            continue;
        }
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
            if !logs::has_level(&options.notify_levels, &known.label) {
                continue;
            }
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

    if baseline {
        ledger.baseline = Some(next.taken.clone());
    }
    ledger.forget_gone();
    ledger.advance(host, next);
    ledger.save(&options.state)?;
    if let Some(path) = &options.report {
        report.save(path)?;
    }
    Ok(Outcome {
        report,
        baseline,
        baseline_from,
        oldest,
        resigned,
        known: ledger.known_signatures.len(),
        reading,
    })
}

/// `(written, name)`, oldest first: with a code version per build, the deploy history.
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

#[cfg(test)]
#[path = "throughput_tests.rs"]
mod throughput_tests;
