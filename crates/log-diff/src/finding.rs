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
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut found: Vec<Finding> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for entry in entries {
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
    found
}
