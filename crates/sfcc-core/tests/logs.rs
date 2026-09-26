//! The log reader against a WebDAV server in memory: what is read, from
//! where, and what the next read starts from.

use chrono::{Duration, Utc};
use flate2::Compression;
use flate2::write::GzEncoder;
use sfcc_core::logs::{self, Mark};
use sfcc_core::testing::MockDav;
use sfcc_core::webdav::Dav;
use std::io::Write;

fn day(days_back: i64) -> String {
    (Utc::now() - Duration::days(days_back))
        .format("%Y%m%d")
        .to_string()
}

fn record(day: &str, time: &str, message: &str) -> String {
    let dashed = format!("{}-{}-{}", &day[..4], &day[4..6], &day[6..]);
    format!(
        "[{dashed} {time}.000 GMT] ERROR PipelineCallServlet|1|S|Cart-Show {message}\n\tat app_x/cartridge/scripts/a.js:10 (f)\n"
    )
}

fn levels() -> Vec<String> {
    logs::parse_levels("error,customerror")
}

fn gzip(text: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(text.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn reads_what_came_after_the_mark_and_marks_where_it_ends() {
    let server = MockDav::start().await;
    let today = day(0);
    let file = format!("Logs/error-blade1-{today}.log");
    server.put(&file, record(&today, "08:00:00", "first"));
    let dav = Dav::new(&server.config()).unwrap();

    let mark = logs::mark(&dav, &levels()).await.unwrap();
    server.append(&file, &record(&today, "08:05:00", "second"));
    // A record still being written is left for the next read.
    server.append(&file, "[half a line");

    let since = logs::since(&dav, &mark, &levels()).await.unwrap();
    assert_eq!(since.entries.len(), 1);
    assert!(since.entries[0].lines[0].contains("second"));

    server.append(&file, " finished]\n");
    let again = logs::since(&dav, &since.next, &levels()).await.unwrap();
    assert_eq!(again.entries.len(), 1);
    assert_eq!(again.entries[0].lines[0], "[half a line finished]");
}

#[tokio::test]
async fn a_file_shorter_than_its_offset_was_rotated_and_is_read_whole() {
    let server = MockDav::start().await;
    let today = day(0);
    let file = format!("Logs/error-blade1-{today}.log");
    server.put(&file, record(&today, "08:00:00", "after rotation"));
    let dav = Dav::new(&server.config()).unwrap();

    let mut mark = Mark::start_of_today();
    mark.offsets
        .insert(format!("error-blade1-{today}.log"), 1_000_000);
    let since = logs::since(&dav, &mark, &levels()).await.unwrap();
    assert_eq!(since.entries.len(), 1);
}

#[tokio::test]
async fn reads_every_wanted_file_at_once_in_the_order_things_happened() {
    let server = MockDav::start().await;
    let today = day(0);
    // More files than are read at once, written out of order.
    for blade in 0..20 {
        let time = format!("08:{:02}:00", 59 - blade);
        server.put(
            &format!("Logs/error-blade{blade}-{today}.log"),
            record(&today, &time, &format!("blade {blade}")),
        );
    }
    server.put(
        &format!("Logs/customerror-blade1-{today}.log"),
        record(&today, "07:00:00", "custom"),
    );
    // Not a wanted level, and a day before the mark.
    server.put(
        &format!("Logs/info-blade1-{today}.log"),
        record(&today, "06:00:00", "info"),
    );
    server.put(
        &format!("Logs/error-blade1-{}.log", day(3)),
        record(&day(3), "06:00:00", "old"),
    );
    let dav = Dav::new(&server.config()).unwrap();

    let since = logs::since(&dav, &Mark::start_of_today(), &levels())
        .await
        .unwrap();
    assert_eq!(since.entries.len(), 21);
    let moments: Vec<&str> = since
        .entries
        .iter()
        .map(|entry| entry.moment.as_str())
        .collect();
    let mut sorted = moments.clone();
    sorted.sort();
    assert_eq!(moments, sorted);
    assert!(since.entries[0].lines[0].contains("custom"));
    assert_eq!(
        since.next.offsets.len(),
        21,
        "one offset per file read today"
    );
    assert!(
        !server
            .requests()
            .iter()
            .any(|request| request.contains("info-"))
    );
}

#[tokio::test]
async fn days_before_today_are_read_but_not_remembered() {
    let server = MockDav::start().await;
    let yesterday = day(1);
    server.put(
        &format!("Logs/error-blade1-{yesterday}.log"),
        record(&yesterday, "22:00:00", "yesterday"),
    );
    let dav = Dav::new(&server.config()).unwrap();

    let since = logs::since(&dav, &Mark::days_back(1), &levels())
        .await
        .unwrap();
    assert_eq!(since.entries.len(), 1);
    assert!(since.next.offsets.is_empty());
    assert_eq!(since.next.day, day(0));
}

#[tokio::test]
async fn the_archive_holds_the_days_the_log_folder_no_longer_does() {
    let server = MockDav::start().await;
    let (old, older, live) = (day(3), day(5), day(1));
    server.put(
        &format!("Logs/log_archive/error-blade1-{old}.log.gz"),
        gzip(&record(&old, "10:00:00", "archived")),
    );
    // Some instances keep a folder a month inside.
    server.put(
        &format!(
            "Logs/log_archive/{}/error-blade2-{older}.log.gz",
            &older[..6]
        ),
        gzip(&record(&older, "10:00:00", "in a folder")),
    );
    // Still in the log folder: left to `since`, not read twice.
    server.put(
        &format!("Logs/log_archive/error-blade1-{live}.log.gz"),
        gzip(&record(&live, "10:00:00", "twice")),
    );
    server.put(
        &format!("Logs/error-blade1-{live}.log"),
        record(&live, "10:00:00", "twice"),
    );
    // Before the first day asked for.
    server.put(
        &format!("Logs/log_archive/error-blade1-{}.log.gz", day(30)),
        gzip(&record(&day(30), "10:00:00", "too old")),
    );
    let dav = Dav::new(&server.config()).unwrap();

    let entries = logs::archived(&dav, &day(14), &levels()).await.unwrap();
    let messages: Vec<&str> = entries
        .iter()
        .map(|entry| entry.lines[0].as_str())
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages[0].contains("in a folder"));
    assert!(messages[1].contains("archived"));
}

#[tokio::test]
async fn an_instance_without_an_archive_has_nothing_archived() {
    let server = MockDav::start().await;
    server.put(&format!("Logs/error-blade1-{}.log", day(0)), "");
    let dav = Dav::new(&server.config()).unwrap();
    assert!(
        logs::archived(&dav, &day(14), &levels())
            .await
            .unwrap()
            .is_empty()
    );
}
