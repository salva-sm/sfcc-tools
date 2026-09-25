//! The instance log, read over WebDAV: records, a mark, and what was written
//! since it.
//!
//! The uploader follows the log and answers "what did my change throw?"; the
//! log differ compares it against what is already known. Both need the same
//! two things - remember where the log ends right now, and later read only
//! what came after - so that lives here, and neither runs the other.

use crate::webdav::{Dav, encode_path};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The levels worth reading when nobody says otherwise.
pub const DEFAULT_LEVELS: &str = "error,customerror,custom";

/// One record of a log file: the line carrying its timestamp, and every line
/// after it up to the next one - a stack trace, a request dump.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The level, as the file name spells it: `error`, `customerror`...
    pub label: String,
    /// The record's timestamp as written, `2026-09-09 07:26:29.103 GMT`, or
    /// empty for lines that arrived without one.
    pub moment: String,
    /// Every line of the record, the first one included.
    pub lines: Vec<String>,
}

impl Entry {
    /// The record's timestamp, when it has one that parses.
    pub fn moment_utc(&self) -> Option<chrono::DateTime<Utc>> {
        let bare = self.moment.trim_end_matches(" GMT");
        NaiveDateTime::parse_from_str(bare, "%Y-%m-%d %H:%M:%S%.f")
            .ok()
            .map(|naive| naive.and_utc())
    }
}

/// Where the log ended at some moment: the length of every file of the
/// wanted levels. Reading from here on is reading what came after.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// When the mark was taken, RFC 3339 in UTC.
    pub taken: String,
    /// The day the offsets belong to, `YYYYMMDD`. Files of this day and the
    /// days after it are read; the ones before are finished history.
    #[serde(default)]
    pub day: String,
    /// Log file name -> its length in bytes at that moment. Sorted, so a mark
    /// kept under version control changes only where the log did.
    pub offsets: BTreeMap<String, u64>,
}

impl Mark {
    /// The start of today: every record written today counts as new.
    pub fn start_of_today() -> Mark {
        Mark::days_back(0)
    }

    /// The start of the day `days` before today: every record the instance
    /// still keeps from then on counts as new. Older files may be gone, or
    /// moved to `log_archive`, which is not read.
    pub fn days_back(days: u32) -> Mark {
        let day = Utc::now() - Duration::days(i64::from(days));
        Mark {
            taken: now(),
            day: day.format("%Y%m%d").to_string(),
            offsets: BTreeMap::new(),
        }
    }
}

/// What [`since`] found, and where the next read should start.
#[derive(Debug)]
pub struct Since {
    /// The records written after the mark, oldest first.
    pub entries: Vec<Entry>,
    /// A mark just past everything read.
    pub next: Mark,
}

/// Remember how long each of today's log files of the wanted levels is.
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

/// Everything written after `mark` to the files of the wanted levels, and a
/// mark past it.
///
/// A file the mark does not know was opened after it, so all of it counts;
/// one shorter than its offset was rotated, and is read from its start. A
/// trailing line still being written is left for the next read.
pub async fn since(dav: &Dav, mark: &Mark, levels: &[String]) -> Result<Since> {
    let today = today();
    let first_day = match mark.day.is_empty() {
        true => today.as_str(),
        false => mark.day.as_str(),
    };

    let mut entries = Vec::new();
    let mut offsets = BTreeMap::new();
    for file in listing(dav).await? {
        let Some(day) = file_day(&file.name) else {
            continue;
        };
        if day < first_day || !is_wanted(&file.name, levels, day) {
            continue;
        }

        let mut offset = mark.offsets.get(&file.name).copied().unwrap_or(0);
        if file.size < offset {
            offset = 0;
        }
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
            entries.extend(parse_entries(&file.name, complete));
        }
        // Yesterday's files are done; keeping their offsets would only grow the mark.
        if day == today {
            offsets.insert(file.name, offset);
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

/// The records of the wanted levels in `log_archive`, from `first_day` on -
/// the days the instance has already compressed and moved out of the log
/// folder. A day still in the log folder is left to [`since`], so nothing is
/// read twice. The archive is read whole, which is only worth it once: for a
/// baseline, not on every run.
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

    let mut entries = Vec::new();
    for (url, name) in files {
        let Some(plain) = name.strip_suffix(".gz") else {
            continue;
        };
        let Some(day) = file_day(plain) else {
            continue;
        };
        if day < first_day || live.contains(plain) || !is_wanted(plain, levels, day) {
            continue;
        }
        let compressed = dav
            .read_bytes(&url)
            .await
            .with_context(|| format!("cannot read {name}"))?;
        let mut text = String::new();
        use std::io::Read;
        flate2::read::MultiGzDecoder::new(compressed.as_slice())
            .read_to_string(&mut text)
            .with_context(|| format!("{name} is not a readable gzip file"))?;
        entries.extend(parse_entries(plain, &text));
    }
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

/// The day the instance is on. It runs on GMT, and so do its file names.
pub fn today() -> String {
    Utc::now().format("%Y%m%d").to_string()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The `YYYYMMDD` a log file is named after: `error-blade1-4-appserver-20260905.log`.
pub fn file_day(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".log")?;
    let day = stem.rsplit('-').next()?;
    (day.len() == 8 && day.bytes().all(|byte| byte.is_ascii_digit())).then_some(day)
}

/// A comma-separated level list, lowercased.
pub fn parse_levels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty())
        .collect()
}

/// Whether a file is a log of `day` at one of the levels, or `all`.
pub fn is_wanted(name: &str, levels: &[String], day: &str) -> bool {
    if !name.ends_with(".log") || !name.contains(day) {
        return false;
    }
    levels
        .iter()
        .any(|level| level == "all" || name.starts_with(level.as_str()))
}

/// Split the text of a log file into records.
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

/// Records of several files, in the order they were written. Leftovers from
/// an entry of an earlier read carry no moment and stay in front.
pub fn order(batch: &mut [Entry]) {
    batch.sort_by(|left, right| left.moment.cmp(&right.moment));
}

/// The timestamp every record opens with: `[2026-09-09 07:26:29.103 GMT]`.
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
