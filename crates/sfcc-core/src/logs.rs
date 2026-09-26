//! The instance log over WebDAV: a mark, and the records written since.

use crate::webdav::{Dav, encode_path};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDateTime, SecondsFormat, Utc};
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_LEVELS: &str = "error,customerror,custom";

/// One file per level, app server and day; more at once would only be throttled.
const READS_AT_ONCE: usize = 6;

/// A timestamped line and every line up to the next one: a stack trace, a request dump.
#[derive(Debug, Clone)]
pub struct Entry {
    pub label: String,
    /// `2026-09-09 07:26:29.103 GMT`, or empty for lines that arrived without one.
    pub moment: String,
    pub lines: Vec<String>,
}

impl Entry {
    pub fn moment_utc(&self) -> Option<chrono::DateTime<Utc>> {
        let bare = self.moment.trim_end_matches(" GMT");
        NaiveDateTime::parse_from_str(bare, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|naive| naive.and_utc())
    }
}

/// Where the log ended at some moment: the length of every file of the wanted levels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub taken: String,
    /// `YYYYMMDD`; files of earlier days are finished history and not read.
    #[serde(default)]
    pub day: String,
    /// Sorted, so a mark kept under version control changes only where the log did.
    pub offsets: BTreeMap<String, u64>,
}

impl Mark {
    pub fn start_of_today() -> Mark {
        Mark::days_back(0)
    }

    /// Older files may be gone, or moved to `log_archive`, which is not read.
    pub fn days_back(days: u32) -> Mark {
        let day = Utc::now() - Duration::days(i64::from(days));
        Mark {
            taken: now(),
            day: day.format("%Y%m%d").to_string(),
            offsets: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct Since {
    pub entries: Vec<Entry>,
    pub next: Mark,
}

pub async fn mark(dav: &Dav, levels: &[String]) -> Result<Mark> {
    let day = today();
    let offsets = listing(dav)
        .await?
        .into_iter()
        .filter(|file| is_wanted(&file.name, levels, &day))
        .map(|file| (file.name, file.size))
        .collect();
    Ok(Mark {
        taken: now(),
        day,
        offsets,
    })
}

/// A file the mark does not know counts whole; one shorter than its offset was
/// rotated. A trailing line still being written is left for the next read.
pub async fn since(dav: &Dav, mark: &Mark, levels: &[String]) -> Result<Since> {
    let today = today();
    let first_day = match mark.day.is_empty() {
        true => today.as_str(),
        false => mark.day.as_str(),
    };

    let wanted: Vec<_> = listing(dav)
        .await?
        .into_iter()
        .filter_map(|file| {
            let day = file_day(&file.name)?.to_string();
            (day.as_str() >= first_day && is_wanted(&file.name, levels, &day))
                .then_some((file, day))
        })
        .collect();

    let reads = wanted.into_iter().map(|(file, day)| async move {
        let mut offset = mark.offsets.get(&file.name).copied().unwrap_or(0);
        if file.size < offset {
            offset = 0;
        }
        let mut read = Vec::new();
        if file.size > offset {
            let url = format!("{}/{}", dav.logs_url(), encode_path(&file.name));
            let text = dav
                .read_from(&url, offset)
                .await
                .with_context(|| format!("cannot read {}", file.name))?;
            let complete = match text.rfind('\n') {
                Some(end) => &text[..=end],
                None => "",
            };
            offset += complete.len() as u64;
            read = parse_entries(&file.name, complete);
        }
        Ok::<_, anyhow::Error>((file.name, day, offset, read))
    });
    let read: Vec<_> = stream::iter(reads)
        .buffered(READS_AT_ONCE)
        .try_collect()
        .await?;

    let mut entries = Vec::new();
    let mut offsets = BTreeMap::new();
    for (name, day, offset, read) in read {
        entries.extend(read);
        // Yesterday's files are done; keeping their offsets would only grow the mark.
        if day == today {
            offsets.insert(name, offset);
        }
    }

    order(&mut entries);
    Ok(Since {
        entries,
        next: Mark {
            taken: now(),
            day: today,
            offsets,
        },
    })
}

/// Days still in the log folder are left to [`since`]. Reads the archive whole:
/// for a baseline, not every run.
pub async fn archived(dav: &Dav, first_day: &str, levels: &[String]) -> Result<Vec<Entry>> {
    let live: std::collections::HashSet<String> = listing(dav)
        .await?
        .into_iter()
        .map(|file| file.name)
        .collect();

    let archive = format!("{}/log_archive", dav.logs_url());
    let mut files = Vec::new();
    for entry in dav.list(&archive).await.unwrap_or_default() {
        match entry.is_dir {
            // Some instances keep a folder per day or per month inside.
            true => {
                let folder = format!("{archive}/{}", encode_path(&entry.name));
                for file in dav.list(&folder).await.unwrap_or_default() {
                    if !file.is_dir {
                        files.push((format!("{folder}/{}", encode_path(&file.name)), file.name));
                    }
                }
            }
            false => files.push((
                format!("{archive}/{}", encode_path(&entry.name)),
                entry.name,
            )),
        }
    }

    let wanted = files.into_iter().filter(|(_, name)| {
        let Some(plain) = name.strip_suffix(".gz") else {
            return false;
        };
        file_day(plain).is_some_and(|day| {
            day >= first_day && !live.contains(plain) && is_wanted(plain, levels, day)
        })
    });
    let reads = wanted.map(|(url, name)| async move {
        let compressed = dav
            .read_bytes(&url)
            .await
            .with_context(|| format!("cannot read {name}"))?;
        let plain = name.strip_suffix(".gz").unwrap_or(&name);
        let mut text = String::new();
        use std::io::Read;
        flate2::read::MultiGzDecoder::new(compressed.as_slice())
            .read_to_string(&mut text)
            .with_context(|| format!("{name} is not a readable gzip file"))?;
        Ok::<_, anyhow::Error>(parse_entries(plain, &text))
    });
    let read: Vec<Vec<Entry>> = stream::iter(reads)
        .buffered(READS_AT_ONCE)
        .try_collect()
        .await?;
    let mut entries: Vec<Entry> = read.into_iter().flatten().collect();
    order(&mut entries);
    Ok(entries)
}

async fn listing(dav: &Dav) -> Result<Vec<crate::webdav::DavEntry>> {
    let files = dav
        .list(dav.logs_url())
        .await
        .context("cannot list the instance logs")?;
    Ok(files.into_iter().filter(|file| !file.is_dir).collect())
}

/// The instance runs on GMT, and so do its file names.
pub fn today() -> String {
    Utc::now().format("%Y%m%d").to_string()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `error-blade1-4-appserver-20260905.log` -> `20260905`.
pub fn file_day(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".log")?;
    let day = stem.rsplit('-').next()?;
    (day.len() == 8 && day.bytes().all(|byte| byte.is_ascii_digit())).then_some(day)
}

pub fn parse_levels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty())
        .collect()
}

pub fn is_wanted(name: &str, levels: &[String], day: &str) -> bool {
    if !name.ends_with(".log") || !name.contains(day) {
        return false;
    }
    levels
        .iter()
        .any(|level| level == "all" || name.starts_with(level.as_str()))
}

pub fn parse_entries(file: &str, text: &str) -> Vec<Entry> {
    let label = file.split('-').next().unwrap_or(file).to_string();
    let mut entries: Vec<Entry> = Vec::new();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match (moment(line), entries.last_mut()) {
            (None, Some(entry)) => entry.lines.push(line.to_string()),
            (moment, _) => entries.push(Entry {
                label: label.clone(),
                moment: moment.unwrap_or_default(),
                lines: vec![line.to_string()],
            }),
        }
    }
    entries
}

/// Leftovers of an entry from an earlier read carry no moment and stay in front.
pub fn order(batch: &mut [Entry]) {
    batch.sort_by(|left, right| left.moment.cmp(&right.moment));
}

/// `[2026-09-09 07:26:29.103 GMT]`
fn moment(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.split_once(']')?.0;
    let shape = inner.as_bytes();
    if shape.len() < 19 || shape[4] != b'-' || shape[7] != b'-' || shape[13] != b':' {
        return None;
    }
    Some(inner.to_string())
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
