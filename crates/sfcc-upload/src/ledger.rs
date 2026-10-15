//! Who, on this machine, produced each sandbox etag, so a conflict can name a local writer.
//!
//! Append-only and shared by every uploader of one identity: each successful upload adds a
//! line per file sent. A conflict looks up the etag the sandbox now carries - found means a
//! sibling process here wrote it, absent means it came from another machine, which the
//! instance never attributes over WebDAV.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sfcc_core::config::Config;
use sfcc_core::state;

/// Past this size the ledger is rewritten to its most recent lines; the lost ones are oldest.
const PRUNE_OVER_BYTES: u64 = 512 * 1024;
const KEEP_LINES: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Watcher,
    /// `push`, with whether a terminal was driving it.
    Push { interactive: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Writer {
    /// The dw.json Business Manager user, or the OAuth client id.
    pub user: String,
    pub source: Source,
    /// Epoch seconds of the write.
    pub at: i64,
}

impl Writer {
    pub fn here(config: &Config, source: Source) -> Writer {
        Writer {
            user: config.credentials.user().to_string(),
            source,
            at: state::now_seconds(),
        }
    }

    /// For a conflict line: "the watcher (jsmith), 2 min ago".
    pub fn describe(&self) -> String {
        let who = match self.source {
            Source::Watcher => "the watcher",
            Source::Push { interactive: true } => "a push you ran",
            Source::Push { interactive: false } => "a push (agent or script)",
        };
        format!("{who} ({}){}", self.user, self.ago())
    }

    fn ago(&self) -> String {
        let seconds = state::now_seconds() - self.at;
        if seconds < 0 {
            return String::new();
        }
        let (value, unit) = match seconds {
            s if s < 90 => (s, "s"),
            s if s < 90 * 60 => (s / 60, " min"),
            s if s < 48 * 3600 => (s / 3600, " h"),
            s => (s / 86_400, " d"),
        };
        format!(", {value}{unit} ago")
    }
}

#[derive(Serialize, Deserialize)]
struct Line {
    etag: String,
    #[serde(flatten)]
    writer: Writer,
}

pub struct Ledger {
    by_etag: HashMap<String, Writer>,
}

impl Ledger {
    pub fn load(identity: &str) -> Ledger {
        let mut by_etag = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(state::upload::writers_path(identity)) {
            for line in text.lines() {
                if let Ok(entry) = serde_json::from_str::<Line>(line) {
                    // A later line for the same etag is the newer truth.
                    by_etag.insert(entry.etag, entry.writer);
                }
            }
        }
        Ledger { by_etag }
    }

    /// Who, on this machine, last wrote the file now carrying this etag.
    pub fn writer_of(&self, etag: &str) -> Option<Writer> {
        if etag.is_empty() {
            return None;
        }
        self.by_etag.get(etag).cloned()
    }
}

/// Records an etag per written file. Best effort: a failure here never fails the upload it
/// follows, and only files whose etag was read back (small batches) can be attributed.
pub fn record(identity: &str, writer: &Writer, etags: impl Iterator<Item = String>) {
    let lines: Vec<String> = etags
        .filter(|etag| !etag.is_empty())
        .filter_map(|etag| serde_json::to_string(&Line { etag, writer: writer.clone() }).ok())
        .collect();
    if lines.is_empty() {
        return;
    }

    let path = state::upload::writers_path(identity);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all((lines.join("\n") + "\n").as_bytes());
    }
    prune(&path);
}

fn prune(path: &Path) {
    let too_big = std::fs::metadata(path).is_ok_and(|meta| meta.len() > PRUNE_OVER_BYTES);
    if !too_big {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= KEEP_LINES {
        return;
    }
    let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
    let _ = std::fs::write(path, kept);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(user: &str, source: Source) -> Writer {
        Writer {
            user: user.to_string(),
            source,
            at: 1_792_000_000,
        }
    }

    #[test]
    fn a_later_line_for_an_etag_wins() {
        let text = concat!(
            r#"{"etag":"a1","user":"jsmith","source":"watcher","at":1}"#,
            "\n",
            r#"{"etag":"a1","user":"agent","source":{"push":{"interactive":false}},"at":2}"#,
            "\n",
        );
        let mut by_etag = HashMap::new();
        for line in text.lines() {
            let entry: Line = serde_json::from_str(line).unwrap();
            by_etag.insert(entry.etag, entry.writer);
        }
        let ledger = Ledger { by_etag };
        let found = ledger.writer_of("a1").unwrap();
        assert_eq!(found.user, "agent");
        assert_eq!(found.source, Source::Push { interactive: false });
    }

    #[test]
    fn an_unknown_or_empty_etag_has_no_writer() {
        let ledger = Ledger {
            by_etag: HashMap::new(),
        };
        assert!(ledger.writer_of("nope").is_none());
        assert!(ledger.writer_of("").is_none());
    }

    #[test]
    fn describe_names_the_source_and_user() {
        assert!(writer("jsmith", Source::Watcher).describe().starts_with("the watcher (jsmith)"));
        assert!(
            writer("agent", Source::Push { interactive: false })
                .describe()
                .starts_with("a push (agent or script) (agent)")
        );
    }
}
