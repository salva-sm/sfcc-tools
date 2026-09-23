//! Records grouped by signature: the same failure forty times is one finding
//! with a count.

use crate::normalize::{Signature, signature};
use chrono::{SecondsFormat, Utc};
use sfcc_core::logs::Entry;
use std::collections::HashMap;

/// One signature, and how often and when it was logged in one read.
#[derive(Debug, Clone)]
pub struct Finding {
    /// What failed.
    pub signature: Signature,
    /// How many records carried it.
    pub count: u64,
    /// The first of them, RFC 3339 in UTC.
    pub first: String,
    /// The last of them.
    pub last: String,
}

/// Group records by signature, in the order each first appeared.
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

        match index.get(&signature.id) {
            Some(&at) => {
                let finding = &mut found[at];
                finding.count += 1;
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
                });
            }
        }
    }
    found
}
