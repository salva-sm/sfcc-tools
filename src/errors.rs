//! "Did my change start throwing?" — the sandbox log between two points in time.
//!
//! `logger` follows the log; this reads a slice of it. You mark the log before
//! exercising a flow and ask afterwards what is new, which is the difference
//! between watching a stream and getting an answer.

use crate::config::Config;
use crate::logging;
use crate::manifest::state_dir;
use crate::push::Ctx;
use crate::tail::{self, Entry, Printer};
use crate::webdav::encode_path;
use anyhow::{Context, Result};
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// A sleeping sandbox answers 502 until it wakes, which takes a few seconds.
const WAIT: Duration = Duration::from_secs(120);

#[derive(Serialize, Deserialize, Default)]
pub struct Mark {
    /// When the mark was taken, for the report to quote back.
    taken: String,
    /// Log file name -> size in bytes at that moment.
    offsets: HashMap<String, u64>,
}

pub struct ReportOptions {
    pub levels: Vec<String>,
    pub color: bool,
}

fn mark_path(config: &Config) -> PathBuf {
    state_dir().join("marks").join(format!("{}.json", config.identity()))
}

/// Remember how long each of today's log files is right now.
pub async fn mark(ctx: &Ctx, levels: &[String]) -> Result<()> {
    ctx.dav.wait_until_ready(Some(WAIT)).await?;
    let offsets = sizes(ctx, levels).await?;
    let mark = Mark {
        taken: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        offsets,
    };

    let path = mark_path(&ctx.config);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&mark)?)
        .with_context(|| format!("cannot write {}", path.display()))?;

    logging::ok(format!(
        "marked {} log file(s) on {}; run `prost errors` after reproducing",
        mark.offsets.len(),
        ctx.config.hostname
    ));
    Ok(())
}

/// Everything logged since the mark, one block per distinct failure.
/// Returns false when nothing new showed up.
pub async fn report(ctx: &Ctx, options: ReportOptions) -> Result<bool> {
    let path = mark_path(&ctx.config);
    let mark: Mark = match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw).with_context(|| format!("cannot read {}", path.display()))?,
        Err(_) => anyhow::bail!("no mark for this sandbox yet - run `prost errors --mark` first"),
    };

    ctx.dav.wait_until_ready(Some(WAIT)).await?;
    let entries = collect(ctx, &mark, &options.levels).await?;
    let groups = group(entries);

    if groups.is_empty() {
        logging::ok(format!("nothing logged since {}", mark.taken));
        return Ok(false);
    }

    let total: usize = groups.iter().map(|group| group.times.len()).sum();
    logging::warn(format!(
        "{total} entr{} since {}, {} distinct",
        if total == 1 { "y" } else { "ies" },
        mark.taken,
        groups.len()
    ));

    let printer = Printer::plain(&ctx.config.cartridges_dir, options.color);
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

async fn sizes(ctx: &Ctx, levels: &[String]) -> Result<HashMap<String, u64>> {
    let base = ctx.config.logs_url();
    let today = Local::now().format("%Y%m%d").to_string();
    let listing = ctx
        .dav
        .list(&base)
        .await
        .with_context(|| format!("cannot list the logs on {}", ctx.config.hostname))?;

    Ok(listing
        .into_iter()
        .filter(|entry| !entry.is_dir && tail::is_wanted(&entry.name, levels, &today))
        .map(|entry| (entry.name, entry.size))
        .collect())
}

async fn collect(ctx: &Ctx, mark: &Mark, levels: &[String]) -> Result<Vec<Entry>> {
    let base = ctx.config.logs_url();
    let today = Local::now().format("%Y%m%d").to_string();
    let listing = ctx.dav.list(&base).await?;

    let mut entries = Vec::new();
    for file in listing {
        if file.is_dir || !tail::is_wanted(&file.name, levels, &today) {
            continue;
        }
        // A file that did not exist at mark time is new, so all of it counts.
        let offset = mark.offsets.get(&file.name).copied().unwrap_or(0).min(file.size);
        if file.size <= offset {
            continue;
        }

        let url = format!("{base}/{}", encode_path(&file.name));
        match ctx.dav.read_from(&url, offset).await {
            Ok(text) => entries.extend(tail::parse_entries(&file.name, &text)),
            Err(error) => logging::warn(format!("{}: {error:#}", file.name)),
        }
    }

    tail::order(&mut entries);
    Ok(entries)
}

struct Group {
    entry: Entry,
    times: Vec<String>,
}

/// The same failure repeated is one problem, not twenty. Entries collapse on
/// everything but their timestamp.
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
