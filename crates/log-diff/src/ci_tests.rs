use super::*;
use chrono::Duration;
use sfcc_core::testing::MockDav;
use std::path::Path;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("log-diff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn path(&self, file: &str) -> PathBuf {
        self.0.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn log_file() -> String {
    format!("Logs/error-blade1-{}.log", logs::today())
}

fn record(ago: Duration, script: &str, message: &str) -> String {
    let at = (Utc::now() - ago).format("%Y-%m-%d %H:%M:%S%.3f GMT");
    format!(
        "[{at}] ERROR PipelineCallServlet|1|S|Cart-Show|PipelineCall|x c [] {message}\n\tat app_x/cartridge/scripts/{script}.js:10 (f)\n"
    )
}

fn options(state: &Path) -> RunOptions {
    RunOptions {
        levels: logs::parse_levels("error"),
        state: state.to_path_buf(),
        sha: None,
        build: None,
        at: None,
        report: None,
        compare_url: Some("https://git/compare/{from}...{to}".to_string()),
        code_url: Some("https://git/blob/{sha}/{path}#L{line}".to_string()),
        baseline_days: 14,
        team: None,
        spike_min: 20,
        spike_factor: 5.0,
        environment: Some("dev".to_string()),
    }
}

fn at(ago: Duration) -> Option<String> {
    Some((Utc::now() - ago).to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[tokio::test]
async fn learns_first_then_reports_what_is_new_with_the_deploy_it_came_with() {
    let scratch = Scratch::new("flow");
    let state = scratch.path("dev.json");
    let server = MockDav::start().await;
    let config = server.config();
    let dav = Dav::new(&config).unwrap();

    server.append(
        &log_file(),
        &record(Duration::hours(3), "old", "TypeError: known"),
    );
    let archived_day = (Utc::now() - Duration::days(3)).format("%Y%m%d");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(
        &mut gz,
        record(Duration::days(3), "archived", "TypeError: archived").as_bytes(),
    )
    .unwrap();
    server.put(
        &format!("Logs/log_archive/error-blade1-{archived_day}.log.gz"),
        gz.finish().unwrap(),
    );

    let first = run(&config, &dav, &options(&state)).await.unwrap();
    assert!(first.baseline);
    assert!(first.report.is_empty());
    assert_eq!(first.known, 2, "today's log and the archive");

    let deploy = |sha: &str, build: u64, ago: Duration| RunOptions {
        sha: Some(sha.to_string()),
        build: Some(build),
        at: at(ago),
        ..options(&state)
    };
    let quiet = run(
        &config,
        &dav,
        &deploy("1111111111111", 11, Duration::hours(2)),
    )
    .await
    .unwrap();
    assert!(!quiet.baseline && quiet.report.is_empty());

    server.append(
        &log_file(),
        &record(Duration::minutes(50), "old", "TypeError: known"),
    );
    server.append(
        &log_file(),
        &record(Duration::minutes(30), "cart", "TypeError: broken"),
    );
    server.append(
        &log_file(),
        &record(Duration::minutes(20), "cart", "TypeError: broken"),
    );
    let report_path = scratch.path("report.json");
    let second = run(
        &config,
        &dav,
        &RunOptions {
            report: Some(report_path.clone()),
            ..deploy("2222222222222", 12, Duration::hours(1))
        },
    )
    .await
    .unwrap();

    assert_eq!(second.report.new.len(), 1, "the known one is not news");
    let item = &second.report.new[0];
    assert_eq!(item.count, 2);
    assert_eq!(
        item.location.as_deref(),
        Some("app_x/cartridge/scripts/cart.js:10")
    );
    let laid = item.deploy.as_ref().expect("laid at a deploy");
    assert_eq!(laid.sha, "2222222222222");
    assert_eq!(laid.build, Some(12));
    assert_eq!(laid.previous_sha.as_deref(), Some("1111111111111"));
    assert_eq!(
        laid.compare_url.as_deref(),
        Some("https://git/compare/1111111111111...2222222222222")
    );
    assert_eq!(
        item.code_url.as_deref(),
        Some("https://git/blob/2222222222222/app_x/cartridge/scripts/cart.js#L10")
    );
    assert_eq!(Report::load(&report_path).unwrap().new.len(), 1);

    let third = run(&config, &dav, &options(&state)).await.unwrap();
    assert!(third.report.is_empty());
    let ledger = Ledger::load(&state).unwrap();
    assert_eq!(ledger.deploy_log.len(), 2);
    assert_eq!(ledger.deploy_log[1].new_signatures, vec![item.id.clone()]);
}

#[tokio::test]
async fn what_the_team_muted_is_counted_but_not_reported() {
    let scratch = Scratch::new("muted");
    let state = scratch.path("dev.json");
    let server = MockDav::start().await;
    let config = server.config();
    let dav = Dav::new(&config).unwrap();
    server.put(&log_file(), "");
    run(&config, &dav, &options(&state)).await.unwrap();

    server.append(
        &log_file(),
        &record(Duration::minutes(5), "bots", "TypeError: noise"),
    );
    let id = crate::finding::findings(&logs::parse_entries(
        "error-blade1-x.log",
        &record(Duration::minutes(5), "bots", "TypeError: noise"),
    ))[0]
        .signature
        .id
        .clone();
    let team = scratch.path("team.json");
    std::fs::write(
        &team,
        format!(r#"{{"muted": {{"{id}": {{"reason": "bots", "by": "someone"}}}}}}"#),
    )
    .unwrap();

    let outcome = run(
        &config,
        &dav,
        &RunOptions {
            team: Some(team),
            ..options(&state)
        },
    )
    .await
    .unwrap();
    assert!(outcome.report.is_empty());
    assert!(
        Ledger::load(&state)
            .unwrap()
            .known_signatures
            .contains_key(&id)
    );
}

#[tokio::test]
async fn signatures_computed_another_way_are_learned_again_without_a_word() {
    let scratch = Scratch::new("resigned");
    let state = scratch.path("dev.json");
    let server = MockDav::start().await;
    let config = server.config();
    let dav = Dav::new(&config).unwrap();
    server.append(
        &log_file(),
        &record(Duration::hours(1), "a", "TypeError: one"),
    );
    run(&config, &dav, &options(&state)).await.unwrap();

    // As if written by a log-diff that signed otherwise.
    let mut ledger = Ledger::load(&state).unwrap();
    ledger.signatures = crate::normalize::SIGNATURES + 1;
    ledger.save(&state).unwrap();

    server.append(
        &log_file(),
        &record(Duration::minutes(10), "b", "TypeError: two"),
    );
    let quiet = run(&config, &dav, &options(&state)).await.unwrap();
    assert!(quiet.resigned);
    assert!(quiet.report.is_empty());

    server.append(
        &log_file(),
        &record(Duration::minutes(5), "c", "TypeError: three"),
    );
    let loud = run(&config, &dav, &options(&state)).await.unwrap();
    assert!(!loud.resigned);
    assert_eq!(loud.report.new.len(), 1);
}

#[tokio::test]
async fn code_versions_come_oldest_first_with_when_they_were_written() {
    let server = MockDav::start().await;
    server.folder("Cartridges/b12_20260921", "Mon, 21 Sep 2026 10:00:00 GMT");
    server.folder("Cartridges/b11_20260920", "Sun, 20 Sep 2026 09:30:00 GMT");
    server.put("Cartridges/readme.txt", "not a code version");
    let dav = Dav::new(&server.config()).unwrap();

    let versions = code_versions(&dav).await.unwrap();
    assert_eq!(
        versions,
        vec![
            (
                "2026-09-20T09:30:00Z".to_string(),
                "b11_20260920".to_string()
            ),
            (
                "2026-09-21T10:00:00Z".to_string(),
                "b12_20260921".to_string()
            ),
        ]
    );
}
