//! A signature's record as the instance logged it, read again when asked: the ledgers keep where
//! a record is, never what it said, and what is read here is printed, never kept.

use crate::ledger::{Known, Ledger};
use crate::normalize::{SIGNATURES, signature};
use crate::output::{Tone, status};
use anyhow::{Result, bail};
use sfcc_core::logs;
use sfcc_core::webdav::Dav;

/// A ledger to look in, and what to call it.
pub type Source<'a> = (&'a str, &'a Ledger);

pub async fn show(
    dav: &Dav,
    hostname: &str,
    sources: &[Source<'_>],
    id: &str,
    first: bool,
) -> Result<()> {
    let (name, ledger, id, known) = find(sources, hostname, id)?;
    if let Some(instance) = ledger.instance.as_deref().filter(|host| *host != hostname) {
        bail!(
            "{id} is in {name} ledger, which reads {instance} - give --config the dw.json of that instance"
        );
    }
    let (which, record) = match first {
        true => ("first", &known.first_record),
        false => ("last", &known.last_record),
    };
    let Some(record) = record else {
        bail!(
            "{name} ledger has no {which} record of {id}: it was learned before log-diff kept \
             where records are, and it will the next time {id} is logged"
        );
    };

    // Signed another way, the ids no longer match: the moment alone has to do.
    let signed_alike = ledger.signatures == SIGNATURES;
    let found = logs::record(dav, &record.file, record.offset, &record.moment, |entry| {
        !signed_alike || signature(entry).id == *id
    })
    .await?;
    let Some(entry) = found else {
        bail!(
            "{hostname} no longer keeps the {which} record of {id} ({}, {}) - it keeps its log \
             for a limited time",
            record.file,
            record.moment
        );
    };

    status(
        Tone::Info,
        &format!(
            "{which} record of {id} · {} · {}{}",
            entry.file,
            entry.moment,
            match first {
                true => String::new(),
                false => format!(" · {} logged", known.count),
            }
        ),
    );
    status(
        Tone::Warn,
        "as logged, customer data included - not for tickets or chats",
    );
    for line in &entry.lines {
        println!("{line}");
    }
    Ok(())
}

/// The signature `id` starts, in the ledger that reads `hostname` when more than one knows it.
fn find<'a>(
    sources: &[Source<'a>],
    hostname: &str,
    id: &str,
) -> Result<(&'a str, &'a Ledger, &'a String, &'a Known)> {
    let mut found = Vec::new();
    for &(name, ledger) in sources {
        let matches: Vec<(&String, &Known)> = ledger
            .known_signatures
            .iter()
            .filter(|(known, _)| known.starts_with(id))
            .collect();
        match matches.as_slice() {
            [] => {}
            [(full, known)] => found.push((name, ledger, *full, *known)),
            _ => bail!(
                "{id} matches {} signatures in {name} ledger - give more of the id",
                matches.len()
            ),
        }
    }
    let here = found
        .iter()
        .position(|(_, ledger, _, _)| ledger.instance.as_deref() == Some(hostname));
    match (here, found.first()) {
        (Some(at), _) => Ok(found[at]),
        (None, Some(first)) => Ok(*first),
        (None, None) => bail!("no signature {id} in your ledger or the team's"),
    }
}

#[cfg(test)]
mod tests {
    use super::find;
    use crate::finding::findings;
    use crate::ledger::Ledger;
    use sfcc_core::logs::parse_entries;

    const FAILURE: &str = "[2026-09-22 21:38:04.112 GMT] ERROR PipelineCallServlet|1|S|Cart-Show|PipelineCall|x c [] TypeError: boom\n\tat app_x/cartridge/scripts/a.js:10 (f)";

    fn reading(host: &str) -> Ledger {
        let mut ledger = Ledger::default();
        for finding in findings(&parse_entries("error-blade1-20260922.log", FAILURE)) {
            ledger.observe(&finding, None, false);
        }
        ledger.instance = Some(host.to_string());
        ledger
    }

    #[test]
    fn the_ledger_of_the_instance_asked_about_wins() {
        let (mine, team) = (reading("sandbox"), reading("dev"));
        let id = mine.known_signatures.keys().next().unwrap().clone();
        let sources = [("your", &mine), ("the team's", &team)];

        assert_eq!(find(&sources, "dev", &id[..6]).unwrap().0, "the team's");
        assert_eq!(find(&sources, "sandbox", &id[..6]).unwrap().0, "your");
        assert_eq!(find(&sources, "staging", &id).unwrap().0, "your");
        assert!(find(&sources, "dev", "zz").is_err());
    }
}
