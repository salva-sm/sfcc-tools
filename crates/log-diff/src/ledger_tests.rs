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

#[test]
fn a_signature_stands_where_it_was_left() {
    let mut mine = Ledger::default();
    let pending = at("2026-09-22 10:00:00.000");
    mine.observe_local(&pending, false);
    let id = pending.signature.id.clone();
    assert_eq!(mine.known_signatures[&id].standing(), Standing::Pending);

    let known = mine.known_signatures.get_mut(&id).unwrap();
    known.pending = false;
    known.resolved_at = Some("2026-09-23T00:00:00Z".into());
    assert_eq!(known.standing(), Standing::Resolved);
    known.resolved_at = None;
    assert_eq!(known.standing(), Standing::Baseline);
    known.muted = true;
    assert_eq!(known.standing(), Standing::Muted);
}

#[test]
fn what_shows_as_a_500_comes_first_then_what_happened_most() {
    let mut often = found(FAILURE).clone();
    often.count = 50;
    let mut mine = Ledger::default();
    mine.observe(&often, None, true);
    let handled = findings(&parse_entries(
        "customerror-blade1-20260922.log",
        "[2026-09-22 10:00:00.000 GMT] ERROR X|S|Cart-Show|P|x c [] Basket <n> is empty",
    ))
    .remove(0);
    let mut handled_often = handled.clone();
    handled_often.count = 500;
    mine.observe(&handled_often, None, true);
    let says_500 = findings(&parse_entries(
        "customerror-blade1-20260922.log",
        "[2026-09-22 10:00:00.000 GMT] ERROR X|S|Cart-Show|P|x c [] Service answered HTTP 500",
    ))
    .remove(0);
    mine.observe(&says_500, None, true);

    let mut ranked: Vec<&Known> = mine.known_signatures.values().collect();
    ranked.sort_by(|left, right| by_importance(left, right));
    let counts: Vec<u64> = ranked.iter().map(|known| known.count).collect();

    // The uncaught error (50) and the one saying 500 (1) before the handled one (500).
    assert_eq!(counts, vec![50, 1, 500]);
}

#[test]
fn a_deploy_is_recognised_by_its_short_or_full_sha() {
    let mut ledger = Ledger::default();
    ledger.record_deploy(
        "9451cff0123456789abcdef0123456789abcdef0",
        None,
        Utc.with_ymd_and_hms(2026, 9, 22, 10, 0, 0).unwrap(),
    );

    assert!(ledger.has_deploy("9451cff"));
    assert!(ledger.has_deploy("9451cff0123456789abcdef0123456789abcdef0"));
    assert!(!ledger.has_deploy("1234567"));
}

#[test]
fn a_build_already_recorded_is_the_same_deploy_whatever_its_sha() {
    let mut ledger = Ledger::default();
    ledger.record_deploy(
        "b4378_20260925",
        Some(4378),
        Utc.with_ymd_and_hms(2026, 9, 25, 11, 42, 49).unwrap(),
    );

    assert!(ledger.has_build(4378));
    assert!(!ledger.has_build(4379));
}

fn counted(id_source: &Finding, per_day: &[(&str, u64)]) -> Finding {
    let mut finding = id_source.clone();
    finding.per_day = per_day
        .iter()
        .map(|(day, count)| (day.to_string(), *count))
        .collect();
    finding.count = per_day.iter().map(|(_, count)| count).sum();
    finding
}

#[test]
fn records_are_counted_per_day_and_old_days_dropped() {
    let mut team = Ledger::default();
    let base = found(FAILURE);
    team.record_daily(&counted(&base, &[("2026-09-20", 3), ("2026-09-21", 1)]));
    team.record_daily(&counted(&base, &[("2026-09-21", 2)]));

    assert_eq!(team.daily["2026-09-20"][&base.signature.id], 3);
    assert_eq!(team.daily["2026-09-21"][&base.signature.id], 3);

    let many: Vec<String> = (0..DAYS_KEPT + 5)
        .map(|back| {
            (chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
                + chrono::Duration::days(back as i64))
            .format("%Y-%m-%d")
            .to_string()
        })
        .collect();
    let refs: Vec<(&str, u64)> = many.iter().map(|day| (day.as_str(), 1)).collect();
    team.record_daily(&counted(&base, &refs));
    assert_eq!(team.daily.len(), DAYS_KEPT);
}

#[test]
fn a_known_signature_far_above_its_usual_day_spikes_once_a_day() {
    let mut team = Ledger::default();
    let base = at("2026-09-10 10:00:00.000");
    team.observe(&base, None, false);
    team.record_daily(&counted(
        &base,
        &[
            ("2026-09-15", 4),
            ("2026-09-16", 6),
            ("2026-09-17", 5),
            ("2026-09-18", 5),
            ("2026-09-19", 4),
            ("2026-09-20", 6),
            ("2026-09-21", 5),
            ("2026-09-22", 90),
        ],
    ));

    let spikes = team.spikes("2026-09-22", 20, 5.0, &[]);
    assert_eq!(spikes.len(), 1);
    assert_eq!(spikes[0].today, 90);
    assert!((spikes[0].usual - 5.0).abs() < 0.01);
    // Reported once for the day, however many runs follow.
    assert!(team.spikes("2026-09-22", 20, 5.0, &[]).is_empty());
}

#[test]
fn a_steady_or_small_or_muted_or_brand_new_signature_does_not_spike() {
    let week: Vec<(&str, u64)> = vec![
        ("2026-09-15", 60),
        ("2026-09-16", 60),
        ("2026-09-17", 60),
        ("2026-09-18", 60),
        ("2026-09-19", 60),
        ("2026-09-20", 60),
        ("2026-09-21", 60),
    ];

    // Steady: 70 against 60 a day.
    let mut team = Ledger::default();
    let steady = at("2026-09-10 10:00:00.000");
    team.observe(&steady, None, false);
    let mut days = week.clone();
    days.push(("2026-09-22", 70));
    team.record_daily(&counted(&steady, &days));
    assert!(team.spikes("2026-09-22", 20, 5.0, &[]).is_empty());

    // Small: from nothing to 15 is under the floor of 20.
    let mut team = Ledger::default();
    team.observe(&steady, None, false);
    team.record_daily(&counted(&steady, &[("2026-09-22", 15)]));
    assert!(team.spikes("2026-09-22", 20, 5.0, &[]).is_empty());

    // Muted: from nothing to 500, but the team said it does not matter.
    let mut team = Ledger::default();
    team.observe(&steady, None, false);
    team.record_daily(&counted(&steady, &[("2026-09-22", 500)]));
    let quiet = [steady.signature.id.as_str()];
    assert!(team.spikes("2026-09-22", 20, 5.0, &quiet).is_empty());

    // Brand new: first seen today, which makes it new, not a spike.
    let mut team = Ledger::default();
    let fresh = at("2026-09-22 08:00:00.000");
    team.observe(&fresh, None, false);
    team.record_daily(&counted(&fresh, &[("2026-09-22", 500)]));
    assert!(team.spikes("2026-09-22", 20, 5.0, &[]).is_empty());
}

#[test]
fn a_ledger_signed_another_way_learns_again_once() {
    let mut ledger = Ledger::default();
    assert!(!ledger.resigned(), "an empty ledger has nothing to mistake");

    ledger.observe(&found(FAILURE), None, false);
    ledger.signatures = crate::normalize::SIGNATURES + 1;
    assert!(ledger.resigned());

    ledger.advance("dev01", Mark::start_of_today());
    assert!(!ledger.resigned(), "once read again, it is signed this way");
}

#[test]
fn a_ledger_older_than_the_field_was_signed_the_first_way() {
    assert_eq!(Ledger::parse("{}").unwrap().signatures, 1);
}
