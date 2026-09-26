//! What the sandbox logged between a mark and now.

use crate::logging;
use crate::manifest::state_dir;
use crate::push::Ctx;
use crate::tail::{Entry, Printer};
use crate::webdav::Ready;
use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use sfcc_core::config::Config;
use sfcc_core::logs::{self, Mark};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// A sleeping sandbox answers 502 until it wakes, which takes a few seconds.
const WAIT: Duration = Duration::from_secs(120);

pub struct ReportOptions {
    pub levels: Vec<String>,
}

fn mark_path(config: &Config) -> PathBuf {
    state_dir()
        .join("marks")
        .join(format!("{}.json", config.identity()))
}

/// Records the current length of each of today's log files.
pub async fn mark(ctx: &Ctx, levels: &[String]) -> Result<()> {
    ctx.dav.wait_until_ready(Some(WAIT)).await?;
    let mark = logs::mark(&ctx.dav, levels).await?;

    let path = mark_path(&ctx.config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&mark)?)
        .with_context(|| format!("cannot write {}", path.display()))?;

    logging::ok(format!(
        "marked {} log file(s) on {}; run `sfcc-upload errors` after reproducing",
        mark.offsets.len(),
        ctx.config.hostname
    ));
    Ok(())
}

/// Returns false when nothing new showed up.
pub async fn report(ctx: &Ctx, options: ReportOptions) -> Result<bool> {
    let path = mark_path(&ctx.config);
    let mark: Mark = match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw)
            .with_context(|| format!("cannot read {}", path.display()))?,
        Err(_) => {
            anyhow::bail!("no mark for this sandbox yet - run `sfcc-upload errors --mark` first")
        }
    };

    ctx.dav.wait_until_ready(Some(WAIT)).await?;
    let entries = logs::since(&ctx.dav, &mark, &options.levels).await?.entries;
    let groups = group(entries);
    let taken = local_time(&mark.taken);

    if groups.is_empty() {
        logging::ok(format!("nothing logged since {taken}"));
        return Ok(false);
    }

    let total: usize = groups.iter().map(|group| group.times.len()).sum();
    logging::warn(format!(
        "{total} entr{} since {taken}, {} distinct",
        if total == 1 { "y" } else { "ies" },
        groups.len()
    ));

    let printer = Printer::plain(&ctx.config.cartridges_dir);
    for group in &groups {
        if group.times.len() > 1 {
            let first = group.times.first().map(String::as_str).unwrap_or("");
            let last = group.times.last().map(String::as_str).unwrap_or("");
            crate::out!("");
            logging::info(format!("x{} between {first} and {last}", group.times.len()));
        } else {
            crate::out!("");
        }
        printer.entry(&group.entry);
    }
    Ok(true)
}

/// A mark from before the move to UTC is already local, and is quoted as is.
fn local_time(taken: &str) -> String {
    match DateTime::parse_from_rfc3339(taken) {
        Ok(moment) => moment
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => taken.to_string(),
    }
}

struct Group {
    entry: Entry,
    times: Vec<String>,
}

fn group(entries: Vec<Entry>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    for entry in entries {
        let key = fingerprint(&entry);
        match seen.get(&key) {
            Some(&index) => groups[index].times.push(entry.moment.clone()),
            None => {
                seen.insert(key, groups.len());
                groups.push(Group {
                    times: vec![entry.moment.clone()],
                    entry,
                });
            }
        }
    }
    groups
}

fn fingerprint(entry: &Entry) -> String {
    let mut key = entry.label.clone();
    for line in &entry.lines {
        key.push('\n');
        // Drop the leading `[timestamp]` so repeats of one failure match.
        match line.starts_with('[') {
            true => key.push_str(line.split_once(']').map(|(_, rest)| rest).unwrap_or(line)),
            false => key.push_str(line),
        }
    }
    key
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
