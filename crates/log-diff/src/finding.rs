use crate::normalize::{Signature, signature};
use chrono::{SecondsFormat, Utc};
use sfcc_core::logs::Entry;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct Finding {
    pub signature: Signature,
    pub count: u64,
    /// RFC 3339, UTC.
    pub first: String,
    pub last: String,
    /// `YYYY-MM-DD`, UTC.
    pub per_day: BTreeMap<String, u64>,
}

/// In the order each signature first appeared.
pub fn findings(entries: &[Entry]) -> Vec<Finding> {
    let mut found = Findings::new();
    for entry in entries {
        found.add(entry);
    }
    found.done()
}

/// [`findings`] an entry at a time, for a log too large to hold whole.
pub struct Findings {
    now: String,
    found: Vec<Finding>,
    index: HashMap<String, usize>,
}

impl Default for Findings {
    fn default() -> Findings {
        Findings::new()
    }
}

impl Findings {
    pub fn new() -> Findings {
        Findings {
            now: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            found: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn done(self) -> Vec<Finding> {
        self.found
    }

    pub fn add(&mut self, entry: &Entry) {
        let (now, found, index) = (&self.now, &mut self.found, &mut self.index);
        let signature = signature(entry);
        // A leftover with no timestamp of its own was logged just before this read.
        let moment = entry
            .moment_utc()
            .map(|moment| moment.to_rfc3339_opts(SecondsFormat::Secs, true))
            .unwrap_or_else(|| now.clone());
        let day = moment[..10].to_string();

        match index.get(&signature.id) {
            Some(&at) => {
                let finding = &mut found[at];
                finding.count += 1;
                *finding.per_day.entry(day).or_default() += 1;
                if moment < finding.first {
                    finding.first = moment.clone();
                }
                if moment > finding.last {
                    finding.last = moment;
                }
            }
            None => {
                index.insert(signature.id.clone(), found.len());
                found.push(Finding {
                    signature,
                    count: 1,
                    first: moment.clone(),
                    last: moment,
                    per_day: BTreeMap::from([(day, 1)]),
                });
            }
        }
    }
}
