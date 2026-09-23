use super::*;

#[test]
fn keeps_only_the_days_files_of_the_wanted_levels() {
    let levels = parse_levels(DEFAULT_LEVELS);
    assert!(is_wanted(
        "error-blade1-1-appserver-20260905.log",
        &levels,
        "20260905"
    ));
    assert!(is_wanted(
        "customerror-blade1-20260905.log",
        &levels,
        "20260905"
    ));
    assert!(!is_wanted("warn-blade1-20260905.log", &levels, "20260905"));
    assert!(!is_wanted("error-blade1-20260904.log", &levels, "20260905"));
    assert!(!is_wanted("error-blade1-20260905.txt", &levels, "20260905"));
}

#[test]
fn the_all_level_keeps_every_log_of_the_day() {
    let levels = parse_levels("all");
    assert!(is_wanted("warn-blade1-20260905.log", &levels, "20260905"));
    assert!(!is_wanted("warn-blade1-20260904.log", &levels, "20260905"));
}

#[test]
fn reads_the_day_out_of_a_log_file_name() {
    assert_eq!(
        file_day("error-blade1-1-appserver-20260905.log"),
        Some("20260905")
    );
    assert_eq!(
        file_day("customerror-blade1-20261231.log"),
        Some("20261231")
    );
    assert_eq!(file_day("error-blade1-20260905.log.gz"), None);
    assert_eq!(file_day("jobs-blade1.log"), None);
}

#[test]
fn a_stack_trace_belongs_to_the_record_above_it() {
    let text = concat!(
        "[2026-09-09 07:26:29.103 GMT] ERROR PipelineCallServlet custom [] TypeError\n",
        "\tat app_common_brand/cartridge/controllers/Account.js:99 (anonymous)\n",
        "\tat modules/server/route.js:83 (next)\n",
        "[2026-09-09 07:26:30.000 GMT] ERROR PipelineCallServlet custom [] another one\n"
    );

    let entries = parse_entries("error-blade1-20260909.log", text);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].label, "error");
    assert_eq!(entries[0].lines.len(), 3);
    assert_eq!(entries[1].lines.len(), 1);
}

#[test]
fn lines_arriving_without_a_record_of_their_own_open_one() {
    let entries = parse_entries(
        "error-blade1-20260909.log",
        "\tat modules/server/route.js:83",
    );

    assert_eq!(entries.len(), 1);
    assert!(entries[0].moment.is_empty());
}

#[test]
fn records_come_out_in_the_order_the_instance_wrote_them() {
    let mut batch = parse_entries(
        "error-blade1-20260909.log",
        "[2026-09-09 07:26:31.000 GMT] late",
    );
    batch.extend(parse_entries(
        "customerror-blade1-20260909.log",
        "[2026-09-09 07:26:29.000 GMT] early",
    ));
    batch.extend(parse_entries(
        "error-blade1-20260909.log",
        "\tat modules/server/route.js:83",
    ));

    order(&mut batch);

    let moments: Vec<&str> = batch.iter().map(|entry| entry.moment.as_str()).collect();
    assert_eq!(
        moments,
        [
            "",
            "2026-09-09 07:26:29.000 GMT",
            "2026-09-09 07:26:31.000 GMT"
        ]
    );
}

#[test]
fn reads_the_moment_only_out_of_a_real_timestamp() {
    assert_eq!(
        moment("[2026-09-09 07:26:29.103 GMT] ERROR").as_deref(),
        Some("2026-09-09 07:26:29.103 GMT")
    );
    assert!(moment("\tat modules/server/route.js:83 (next)").is_none());
    assert!(moment("[main] Quota object.CouponPO").is_none());
}

#[test]
fn the_moment_parses_as_utc() {
    let entry = &parse_entries(
        "error-blade1-20260909.log",
        "[2026-09-09 07:26:29.103 GMT] ERROR x",
    )[0];
    assert_eq!(
        entry.moment_utc().map(|moment| moment.to_rfc3339()),
        Some("2026-09-09T07:26:29.103+00:00".to_string())
    );
}

#[test]
fn a_mark_written_before_it_knew_its_day_still_reads() {
    let mark: Mark = serde_json::from_str(
        r#"{"taken":"2026-09-10 09:00:00","offsets":{"error-blade1-20260910.log":42}}"#,
    )
    .unwrap();
    assert!(mark.day.is_empty());
    assert_eq!(mark.offsets["error-blade1-20260910.log"], 42);
}

#[test]
fn a_mark_days_back_starts_that_many_days_before_today() {
    assert_eq!(Mark::days_back(0).day, today());
    let week = Mark::days_back(7);
    assert!(week.offsets.is_empty());
    assert!(week.day < today());
    assert_eq!(
        week.day,
        (Utc::now() - chrono::Duration::days(7))
            .format("%Y%m%d")
            .to_string()
    );
}
