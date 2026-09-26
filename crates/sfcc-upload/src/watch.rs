use crate::daemon;
use crate::logging::{self, Change};
use crate::manifest::Manifest;
use crate::problems;
use crate::push::{
    Ctx, PushOptions, delete_paths, forget_files, push, select_changed, upload_files,
};
use crate::reload::{Browser, worth_reloading};
use crate::scan::{LocalFile, collect_files, describe};
use crate::sync_status;
use crate::webdav::{Availability, Ready};
use anyhow::{Context, Result, bail};
use notify::RecursiveMode;
use notify_debouncer_full::new_debouncer;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

const DEBOUNCE: Duration = Duration::from_millis(300);
const DRAIN_QUIET: Duration = Duration::from_millis(120);
const DRAIN_BURST: Duration = Duration::from_millis(500);
// Shortening this splits a burst across batches and measures worse, not better.
const DRAIN_CAP: Duration = Duration::from_secs(3);
const BURST_PATHS: usize = 25;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(20);
// After a failure: probe after this, doubling up to RETRY_MAX; a save probes at once.
const RETRY_FIRST: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub struct WatchOptions {
    pub initial_push: bool,
    pub full: bool,
    pub reload_port: Option<u16>,
    pub problems: bool,
}

pub async fn watch(ctx: Ctx, options: WatchOptions) -> Result<()> {
    let heartbeat = daemon::heartbeat_path(&ctx.config);
    daemon::write_heartbeat(&heartbeat);
    sync_status::publish(&ctx.config, sync_status::State::Uploading);
    if options.problems {
        problems::enable();
    }
    problems::begin("uploading");

    if let Err(error) = initial_sync(&ctx, options).await {
        problems::failed([], &ctx.config.dw_json, &format!("{error:#}"));
        return Err(error);
    }
    problems::synced();
    sync_status::publish(&ctx.config, sync_status::State::Synced);

    let (sender, mut receiver) = unbounded_channel::<Vec<PathBuf>>();
    let mut debouncer = new_debouncer(
        DEBOUNCE,
        None,
        move |result: notify_debouncer_full::DebounceEventResult| {
            if let Ok(events) = result {
                let paths: Vec<PathBuf> = events
                    .into_iter()
                    .flat_map(|event| event.event.paths)
                    .collect();
                if !paths.is_empty() {
                    let _ = sender.send(paths);
                }
            }
        },
    )
    .context("cannot start the file watcher")?;

    debouncer
        .watch(&ctx.config.cartridges_dir, RecursiveMode::Recursive)
        .with_context(|| format!("cannot watch {}", ctx.config.cartridges_dir.display()))?;

    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);

    logging::ok(format!(
        "watching {} -> {}",
        ctx.config.cartridges_dir.display(),
        ctx.dav.base_url()
    ));

    let browser = match options.reload_port {
        Some(port) => {
            logging::info(format!(
                "reloading storefront tabs through DevTools on port {port}"
            ));
            Some(Browser::new(port, ctx.config.hostname.clone())?)
        }
        None => None,
    };

    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut retry: Option<Retry> = None;

    loop {
        let due = retry.map(|retry| retry.at);
        tokio::select! {
            _ = ticker.tick() => daemon::write_heartbeat(&heartbeat),
            _ = tokio::signal::ctrl_c() => {
                logging::info("stopping the watcher");
                manifest.save(&ctx.manifest_path)?;
                sync_status::clear(&ctx.config);
                return Ok(());
            }
            _ = sleep_until(due), if due.is_some() => {
                retry_queued(&ctx, &mut manifest, &mut pending, browser.as_ref(), &mut retry).await;
            }
            batch = receiver.recv() => {
                let Some(paths) = batch else {
                    manifest.save(&ctx.manifest_path)?;
                    sync_status::clear(&ctx.config);
                    return Ok(());
                };
                pending.extend(paths);
                drain_into(&mut receiver, &mut pending).await;
                match retry {
                    None => attempt(&ctx, &mut manifest, &mut pending, browser.as_ref(), &mut retry).await,
                    Some(_) => retry_queued(&ctx, &mut manifest, &mut pending, browser.as_ref(), &mut retry).await,
                }
            }
        }
    }
}

async fn initial_sync(ctx: &Ctx, options: WatchOptions) -> Result<()> {
    ctx.dav.wait_until_ready(None).await?;
    if options.initial_push {
        push(
            ctx,
            PushOptions {
                full: options.full,
                dry_run: false,
                show_progress: !options.problems,
            },
        )
        .await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Retry {
    at: tokio::time::Instant,
    delay: Duration,
}

impl Retry {
    fn after(previous: Option<Retry>) -> Retry {
        let delay = previous.map_or(RETRY_FIRST, |previous| (previous.delay * 2).min(RETRY_MAX));
        Retry {
            at: tokio::time::Instant::now() + delay,
            delay,
        }
    }
}

async fn sleep_until(due: Option<tokio::time::Instant>) {
    match due {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn attempt(
    ctx: &Ctx,
    manifest: &mut Manifest,
    pending: &mut BTreeSet<PathBuf>,
    browser: Option<&Browser>,
    retry: &mut Option<Retry>,
) {
    let touched = std::mem::take(pending);
    sync_status::publish_uploading(&ctx.config, touched.len());
    problems::begin(&format!("uploading {} change(s)", touched.len()));
    match synchronize(ctx, manifest, &touched).await {
        Ok(sent) => {
            if retry.take().is_some() {
                logging::ok("sandbox is back - the queued changes are uploaded");
            }
            problems::synced();
            sync_status::publish(&ctx.config, sync_status::State::Synced);
            refresh_browser(browser, &sent).await;
        }
        Err(error) => {
            pending.extend(touched);
            fail(ctx, pending, format!("{error:#}"), retry);
        }
    }
}

async fn retry_queued(
    ctx: &Ctx,
    manifest: &mut Manifest,
    pending: &mut BTreeSet<PathBuf>,
    browser: Option<&Browser>,
    retry: &mut Option<Retry>,
) {
    match probe(ctx).await {
        Ok(()) => attempt(ctx, manifest, pending, browser, retry).await,
        Err(reason) => {
            problems::begin("retrying");
            fail(ctx, pending, reason, retry)
        }
    }
}

fn fail(ctx: &Ctx, pending: &BTreeSet<PathBuf>, reason: String, retry: &mut Option<Retry>) {
    let next = Retry::after(*retry);
    let detail = format!(
        "{reason} - {} change(s) queued, retrying in {}s",
        pending.len(),
        next.delay.as_secs()
    );
    logging::error(&detail);
    problems::failed(pending, &ctx.config.dw_json, &detail);
    sync_status::publish_failure(&ctx.config, detail);
    *retry = Some(next);
}

async fn probe(ctx: &Ctx) -> std::result::Result<(), String> {
    match ctx.dav.availability().await {
        Availability::Ready => Ok(()),
        Availability::MissingCodeVersion => ctx
            .dav
            .mkcol(ctx.dav.base_url())
            .await
            .map_err(|error| format!("{error:#}")),
        Availability::Unauthorized => {
            Err("the sandbox rejected the credentials from dw.json (HTTP 401/403)".to_string())
        }
        Availability::Unavailable(reason) => Err(format!("sandbox unavailable ({reason})")),
    }
}

async fn drain_into(
    receiver: &mut UnboundedReceiver<Vec<PathBuf>>,
    pending: &mut BTreeSet<PathBuf>,
) {
    let started = Instant::now();
    while started.elapsed() < DRAIN_CAP {
        let quiet = if pending.len() > BURST_PATHS {
            DRAIN_BURST
        } else {
            DRAIN_QUIET
        };
        match tokio::time::timeout(quiet, receiver.recv()).await {
            Ok(Some(more)) => pending.extend(more),
            _ => break,
        }
    }
}

struct Work {
    upserts: Vec<LocalFile>,
    removals: Vec<String>,
}

async fn synchronize(
    ctx: &Ctx,
    manifest: &mut Manifest,
    touched: &BTreeSet<PathBuf>,
) -> Result<Vec<String>> {
    let (candidates, removals) = classify(ctx, touched);
    let work = Work {
        upserts: select_changed(&candidates, manifest),
        removals,
    };
    if work.upserts.is_empty() && work.removals.is_empty() {
        return Ok(Vec::new());
    }

    let outcome = transfer(ctx, manifest, work).await;
    // Saved on failure too, so forgotten files stay forgotten if the watcher stops now.
    manifest.save(&ctx.manifest_path)?;
    outcome
}

/// What did reach the sandbox is recorded even on error, so a resend sends only the rest.
async fn transfer(ctx: &Ctx, manifest: &mut Manifest, work: Work) -> Result<Vec<String>> {
    let mut sent = Vec::new();
    let mut failed = 0;

    if !work.removals.is_empty() {
        let deleted = delete_paths(ctx, &work.removals).await?;
        logging::changes(Change::Deleted, &deleted.gone);
        for path in &work.removals {
            if !deleted.failed.contains(path) {
                manifest.forget(path);
                manifest.forget_prefix(path);
                sent.push(path.clone());
            }
        }
        failed += deleted.failed.len();
    }

    if !work.upserts.is_empty() {
        let wanted = work.upserts.len();
        forget_files(manifest, &work.upserts);
        let recorded = upload_files(ctx, work.upserts, None).await?;
        failed += wanted - recorded.len();
        let names: Vec<String> = recorded
            .iter()
            .map(|(relative, _)| relative.clone())
            .collect();
        for (relative, entry) in recorded {
            manifest.record(relative, entry);
        }
        logging::changes(Change::Uploaded, &names);
        sent.extend(names);
    }

    if failed > 0 {
        bail!("{failed} change(s) did not reach the sandbox");
    }
    Ok(sent)
}

async fn refresh_browser(browser: Option<&Browser>, sent: &[String]) {
    let Some(browser) = browser else {
        return;
    };
    if !worth_reloading(sent) {
        return;
    }
    match browser.reload_storefront().await {
        Ok(0) => logging::warn("no storefront tab open to reload"),
        Ok(tabs) => logging::info(format!("reloaded {tabs} tab(s)")),
        Err(error) => logging::warn(format!("{error:#}")),
    }
}

fn classify(ctx: &Ctx, touched: &BTreeSet<PathBuf>) -> (Vec<LocalFile>, Vec<String>) {
    let base = &ctx.config.cartridges_dir;
    let mut upserts: Vec<LocalFile> = Vec::new();
    let mut removals: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for path in touched {
        let Some(relative) = crate::scan::remote_path(path, base) else {
            continue;
        };
        if ctx.ignore.skips(&relative) || !is_inside_cartridge(&relative) {
            continue;
        }

        if path.is_dir() {
            for file in collect_files(path, base, &ctx.ignore) {
                if seen.insert(file.relative.clone()) {
                    upserts.push(file);
                }
            }
        } else if let Some(file) = describe(path, base) {
            if seen.insert(file.relative.clone()) {
                upserts.push(file);
            }
        } else {
            let root = removal_root(path, base).unwrap_or(relative);
            if seen.insert(root.clone()) {
                removals.push(root);
            }
        }
    }

    (upserts, removals)
}

fn is_inside_cartridge(relative: &str) -> bool {
    relative.split('/').count() > 1
}

fn removal_root(path: &Path, base: &Path) -> Option<String> {
    let mut highest = path;
    for ancestor in path.ancestors().skip(1) {
        if ancestor == base || !ancestor.starts_with(base) || ancestor.exists() {
            break;
        }
        highest = ancestor;
    }
    crate::scan::remote_path(highest, base)
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
