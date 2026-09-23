use super::*;
use crate::finding::findings;
use chrono::TimeZone;
use sfcc_core::logs::parse_entries;

fn found(text: &str) -> Finding {
    findings(&parse_entries("error-blade1-20260922.log", text))
        .into_iter()
        .next()
        .expect("the fixture holds a record")
}

const FAILURE: &str = "[2026-09-22 21:38:04.112 GMT] ERROR PipelineCallServlet|1|S|Cart-Show|PipelineCall|x c [] TypeError: boom\n\tat app_x/cartridge/scripts/a.js:10 (f)";

#[test]
fn reads_the_format_it_documents() {
    let ledger = Ledger::parse(
        r#"{
          "cursor": { "taken": "2026-09-22T21:40:00Z", "offsets": {} },
          "known_signatures": {
            "a1b2c3": {
              "label": "error",
              "exception_class": "NullPointerException",
              "location": "checkout/CheckoutServices.js:214",
              "example": "...",
              "first_seen": "2026-09-10T09:12:00Z",
              "last_seen": "2026-09-22T21:38:00Z",
              "count": 143,
              "first_deploy_sha": "9451cff"
            }
          },
          "deploy_log": [ { "sha": "9451cff", "build": 4821, "timestamp": "2026-09-22T21:38:00Z" } ]
        }"#,
    )
    .unwrap();

    assert_eq!(ledger.version, VERSION);
    assert_eq!(ledger.known_signatures["a1b2c3"].count, 143);
    assert_eq!(ledger.deploy_log[0].build, Some(4821));
}

#[test]
fn refuses_a_format_from_the_future() {
    assert!(Ledger::parse(r#"{ "version": 99 }"#).is_err());
}

#[test]
fn a_cursor_only_counts_on_the_instance_it_was_taken_on() {
    let mut ledger = Ledger::default();
    ledger.advance("dev01.example.com", Mark::default());

    assert!(ledger.cursor_for("dev01.example.com").is_some());
    assert!(ledger.cursor_for("sbx-002.example.com").is_none());
}

#[test]
fn a_signature_is_new_once_and_counted_after() {
    let mut ledger = Ledger::default();
    let finding = found(FAILURE);

    assert!(ledger.observe(&finding, Some("9451cff"), false));
    assert!(!ledger.observe(&finding, Some("later"), false));

    let known = &ledger.known_signatures[&finding.signature.id];
    assert_eq!(known.count, 2);
    assert_eq!(known.first_deploy_sha.as_deref(), Some("9451cff"));
    assert_eq!(known.first_seen, "2026-09-22T21:38:04Z");
}

#[test]
fn a_record_belongs_to_the_deploy_that_was_live_when_it_was_logged() {
    let mut ledger = Ledger::default();
    ledger.record_deploy(
        "aaa",
        Some(1),
        Utc.with_ymd_and_hms(2026, 9, 22, 10, 0, 0).unwrap(),
    );
    ledger.record_deploy(
        "bbb",
        Some(2),
        Utc.with_ymd_and_hms(2026, 9, 22, 20, 0, 0).unwrap(),
    );

    assert_eq!(ledger.deploy_at("2026-09-22T09:59:59Z"), None);
    assert_eq!(ledger.deploy_at("2026-09-22T12:00:00Z"), Some(0));
    assert_eq!(ledger.deploy_at("2026-09-22T21:38:04Z"), Some(1));
}

#[test]
fn what_the_team_knows_is_not_pending_for_anyone() {
    let finding = found(FAILURE);
    let mut local = Ledger::default();
    local.observe(&finding, None, true);
    let mut team = Ledger::default();

    assert_eq!(local.pending(&team).count(), 1);
    team.observe(&finding, Some("9451cff"), false);
    assert_eq!(local.pending(&team).count(), 0);
}

#[test]
fn survives_a_round_trip_through_disk() {
    let path = std::env::temp_dir().join(format!("log-diff-test-{}.json", std::process::id()));
    let mut ledger = Ledger::default();
    ledger.observe(&found(FAILURE), None, true);
    ledger.save(&path).unwrap();

    let again = Ledger::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(again.known_signatures, ledger.known_signatures);
}

#[test]
fn no_file_yet_is_an_empty_ledger() {
    let ledger = Ledger::load(Path::new("/nowhere/at/all/ledger.json")).unwrap();
    assert!(ledger.known_signatures.is_empty());
}

#[test]
fn repeats_in_one_read_become_one_finding() {
    let text = format!(
        "{FAILURE}\n{}",
        FAILURE.replace("21:38:04.112", "21:39:00.000")
    );
    let all = findings(&parse_entries("error-blade1-20260922.log", &text));

    assert_eq!(all.len(), 1);
    assert_eq!(all[0].count, 2);
    assert_eq!(all[0].first, "2026-09-22T21:38:04Z");
    assert_eq!(all[0].last, "2026-09-22T21:39:00Z");
}

fn at(moment: &str) -> Finding {
    found(&FAILURE.replace("2026-09-22 21:38:04.112", moment))
}

#[test]
fn a_pending_signature_not_logged_for_long_enough_resolves() {
    let mut mine = Ledger::default();
    let finding = at("2026-09-22 10:00:00.000");
    assert_eq!(mine.observe_local(&finding, false), Seen::New);

    assert_eq!(
        mine.expire("2026-09-22T09:00:00Z", "2026-09-25T10:00:00Z", &[]),
        0
    );
    assert_eq!(
        mine.expire("2026-09-22T11:00:00Z", "2026-09-25T10:00:00Z", &[]),
        1
    );

    let known = &mine.known_signatures[&finding.signature.id];
    assert!(!known.pending);
    assert_eq!(known.resolved_at.as_deref(), Some("2026-09-25T10:00:00Z"));
}

#[test]
fn what_a_pass_just_reported_does_not_expire_in_it() {
    let mut mine = Ledger::default();
    let finding = at("2026-09-22 10:00:00.000");
    mine.observe_local(&finding, false);

    let spared = [finding.signature.id.as_str()];
    assert_eq!(
        mine.expire("2026-09-30T00:00:00Z", "2026-09-30T00:00:00Z", &spared),
        0
    );
}

#[test]
fn a_resolved_signature_logged_again_comes_back_and_can_expire_again() {
    let mut mine = Ledger::default();
    mine.observe_local(&at("2026-09-22 10:00:00.000"), false);
    mine.expire("2026-09-23T00:00:00Z", "2026-09-25T10:00:00Z", &[]);

    // Logged before it was resolved: a late read, not a return.
    assert_eq!(
        mine.observe_local(&at("2026-09-24 10:00:00.000"), false),
        Seen::Known
    );
    // Logged after.
    let again = at("2026-09-26 10:00:00.000");
    assert_eq!(mine.observe_local(&again, false), Seen::Back);
    let known = &mine.known_signatures[&again.signature.id];
    assert!(known.pending && known.back && known.resolved_at.is_none());

    assert_eq!(
        mine.expire("2026-09-30T00:00:00Z", "2026-09-30T00:00:00Z", &[]),
        1
    );
    assert_eq!(
        mine.observe_local(&at("2026-10-01 10:00:00.000"), false),
        Seen::Back
    );
}

#[test]
fn only_pending_signatures_expire() {
    let mut mine = Ledger::default();
    let muted = at("2026-09-22 10:00:00.000");
    mine.observe_local(&muted, true);

    assert_eq!(
        mine.expire("2026-12-31T00:00:00Z", "2026-12-31T00:00:00Z", &[]),
        0
    );
    assert!(
        mine.known_signatures[&muted.signature.id]
            .resolved_at
            .is_none()
    );
}

#[test]
fn a_muted_or_baseline_signature_never_comes_back() {
    let mut mine = Ledger::default();
    assert_eq!(
        mine.observe_local(&at("2026-09-22 10:00:00.000"), true),
        Seen::Known
    );
    assert_eq!(
        mine.observe_local(&at("2026-12-01 10:00:00.000"), false),
        Seen::Known
    );
}
