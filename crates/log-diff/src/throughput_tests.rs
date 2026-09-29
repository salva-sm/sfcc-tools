//! How long each part of a read takes, on made-up records shaped like PRD's: no real log.
//!
//!   cargo test --release -p log-diff throughput -- --ignored --nocapture
//!
//! `THROUGHPUT_MB` (default 100) is the size of the log read, split over 26 app servers.

use super::*;
use crate::finding::Findings;
use crate::normalize::signature;
use sfcc_core::logs::EntryParser;
use sfcc_core::testing::MockDav;
use std::time::Instant;

const SERVERS: usize = 26;

/// A tiny deterministic generator: the same log on every run.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn hex(&mut self, len: usize) -> String {
        (0..len).map(|_| format!("{:x}", self.below(16))).collect()
    }
}

/// One record: a header like the instance's, a message with the values that vary, a stack.
fn record(dice: &mut Dice, at: &str) -> String {
    let sites = ["guess_eu", "guess_uk", "guess_de", "guess_fr", "rb_eu"];
    let controllers = [
        "Cart-Show",
        "Product-Show",
        "Search-Show",
        "CheckoutServices-PlaceOrder",
        "Account-Show",
        "Page-Include",
    ];
    let site = sites[dice.below(sites.len() as u64) as usize];
    let controller = controllers[dice.below(controllers.len() as u64) as usize];
    let kind = dice.below(40);
    let message = match kind % 8 {
        0 => format!(
            "TypeError: Cannot read property \"price{}\" from null",
            kind
        ),
        1 => format!(
            "Order {} could not be placed: payment declined for {}@example.com",
            10_000_000 + dice.below(9_000_000),
            dice.hex(8)
        ),
        2 => format!(
            "Product {}_{} not found in catalog guess-master-{}",
            dice.below(99999),
            dice.below(999),
            kind
        ),
        3 => format!(
            "Request to https://api.example.com/v{}/stock/{}?session={} timed out after {}.{}s",
            kind % 3,
            dice.hex(12),
            dice.hex(24),
            dice.below(30),
            dice.below(999)
        ),
        4 => format!(
            "com.demandware.beehive.core.capi.pipeline.PipelineExecutionException{}: script failed at line {}",
            kind,
            dice.below(400)
        ),
        5 => format!(
            "Custom object Kind{} with key {} already exists, token={}",
            kind,
            dice.hex(32),
            dice.hex(40)
        ),
        6 => format!(
            "Invalid address from {}.{}.{}.{}: postal code {} is not valid for {}",
            dice.below(255),
            dice.below(255),
            dice.below(255),
            dice.below(255),
            dice.below(99999),
            site
        ),
        _ => format!("ReferenceError: helper{} is not defined", kind),
    };
    let mut text = format!(
        "[{at}] ERROR PipelineCallServlet|{}|Sites-{site}-Site|{controller}|PipelineCall|{} custom.Kind{kind} Sites-{site}-Site STOREFRONT {} {} {} - {message}\n",
        dice.below(1_000_000_000),
        dice.hex(8),
        dice.hex(40),
        dice.hex(24),
        dice.below(10_000_000_000),
    );
    for frame in 0..(4 + dice.below(12)) {
        text.push_str(&format!(
            "\tat app_guess/cartridge/scripts/kind{kind}/file{frame}.js:{} (fn{frame})\n",
            10 + dice.below(500)
        ));
    }
    // The request dump SFCC writes after many errors.
    if dice.below(3) == 0 {
        text.push_str(&format!(
            "Request: https://www.example.com/{site}/p/{}.html?dwvar={}\n",
            dice.hex(10),
            dice.hex(16)
        ));
    }
    text
}

fn make_log(bytes: usize, seed: u64) -> String {
    let mut dice = Dice(seed | 1);
    let at = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f GMT").to_string();
    let mut log = String::with_capacity(bytes + 4096);
    while log.len() < bytes {
        log.push_str(&record(&mut dice, &at));
    }
    log
}

fn seconds(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a test: run it with --release --ignored --nocapture"]
async fn throughput() {
    let megabytes: usize = std::env::var("THROUGHPUT_MB")
        .ok()
        .and_then(|mb| mb.parse().ok())
        .unwrap_or(100);
    let per_server = megabytes * 1024 * 1024 / SERVERS;

    let started = Instant::now();
    let files: Vec<(String, String)> = (0..SERVERS)
        .map(|server| {
            (
                format!("error-blade{server}-0-appserver-{}.log", logs::today()),
                make_log(per_server, server as u64 + 1),
            )
        })
        .collect();
    let total: usize = files.iter().map(|(_, log)| log.len()).sum();
    println!(
        "generated {:.0} MB in {} files in {:.1}s",
        total as f64 / 1048576.0,
        files.len(),
        seconds(started)
    );

    // Parsing alone, from memory: what reading costs without the network.
    let started = Instant::now();
    let mut entries = Vec::new();
    for (name, log) in &files {
        let mut parser = EntryParser::new(name);
        for line in log.lines() {
            entries.extend(parser.line(line));
        }
        entries.extend(parser.finish());
    }
    println!(
        "parse:      {:>7.2}s  {} records",
        seconds(started),
        entries.len()
    );

    let started = Instant::now();
    let mut ids = std::collections::HashSet::new();
    for entry in &entries {
        ids.insert(signature(entry).id);
    }
    let sign = seconds(started);
    println!(
        "signatures: {:>7.2}s  {} distinct ({:.1} us a record)",
        sign,
        ids.len(),
        sign * 1e6 / entries.len() as f64
    );

    let started = Instant::now();
    let mut found = Findings::new();
    for entry in &entries {
        found.add(entry);
    }
    println!(
        "findings:   {:>7.2}s  (signatures included)",
        seconds(started)
    );
    drop(entries);

    // The whole read, over a local WebDAV server: a baseline of an empty day, then the log.
    let server = MockDav::start().await;
    let config = server.config();
    let dav = Dav::new(&config).unwrap();
    let dir = std::env::temp_dir().join(format!("log-diff-throughput-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = dir.join("prd.json");
    let mut options = RunOptions {
        levels: logs::parse_levels("error"),
        notify_levels: logs::parse_levels("error"),
        state: state.clone(),
        sha: None,
        build: None,
        at: None,
        report: None,
        compare_url: None,
        code_url: None,
        baseline_days: 0,
        team: None,
        spike_min: 20,
        spike_factor: 5.0,
        environment: Some("prd".to_string()),
    };
    for (name, _) in &files {
        server.put(&format!("Logs/{name}"), Vec::new());
    }
    run(&config, &dav, &options).await.unwrap();
    for (name, log) in files {
        server.put(&format!("Logs/{name}"), log.into_bytes());
    }

    let started = Instant::now();
    let mut records = 0usize;
    let (_, reading) = logs::since_each(
        &dav,
        &Ledger::load(&state).unwrap().cursor.unwrap(),
        &options.levels,
        |_| records += 1,
    )
    .await
    .unwrap();
    println!(
        "read:       {:>7.2}s  {} records; {reading}",
        seconds(started),
        records
    );

    options.report = Some(dir.join("report.json"));
    let started = Instant::now();
    let outcome = run(&config, &dav, &options).await.unwrap();
    println!(
        "run:        {:>7.2}s  {} new, {} known",
        seconds(started),
        outcome.report.new.len(),
        outcome.known
    );

    let _ = std::fs::remove_dir_all(&dir);
}
