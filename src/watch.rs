use crate::daemon;
use crate::logging;
use crate::manifest::Manifest;
use crate::push::{Ctx, PushOptions, delete_paths, push, select_changed, upload_files};
use crate::reload::{Browser, worth_reloading};
use crate::scan::{LocalFile, collect_files, describe};
use anyhow::{Context, Result};
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

#[derive(Debug, Clone, Copy)]
pub struct WatchOptions {
    pub initial_push: bool,
    pub full: bool,
    pub reload_port: Option<u16>,
}

pub async fn watch(ctx: Ctx, options: WatchOptions) -> Result<()> {
    let heartbeat = daemon::heartbeat_path(&ctx.config);
    daemon::write_heartbeat(&heartbeat);

    ctx.dav.wait_until_ready(None).await?;
    if options.initial_push {
        push(&ctx, PushOptions { full: options.full, dry_run: false, show_progress: true }).await?;
    }

    let (sender, mut receiver) = unbounded_channel::<Vec<PathBuf>>();
    let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: notify_debouncer_full::DebounceEventResult| {
        if let Ok(events) = result {
            let paths: Vec<PathBuf> = events.into_iter().flat_map(|event| event.event.paths).collect();
            if !paths.is_empty() {
                let _ = sender.send(paths);
            }
        }
    })
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
            logging::info(format!("reloading storefront tabs through DevTools on port {port}"));
            Some(Browser::new(port, ctx.config.hostname.clone())?)
        }
        None => None,
    };

    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => daemon::write_heartbeat(&heartbeat),
            _ = tokio::signal::ctrl_c() => {
                logging::info("stopping the watcher");
                manifest.save(&ctx.manifest_path)?;
                return Ok(());
            }
            batch = receiver.recv() => {
                let Some(paths) = batch else {
                    manifest.save(&ctx.manifest_path)?;
                    return Ok(());
                };
                pending.extend(paths);
                drain_into(&mut receiver, &mut pending).await;
                let touched = std::mem::take(&mut pending);
                match synchronize(&ctx, &mut manifest, &touched).await {
                    Ok(sent) => refresh_browser(browser.as_ref(), &sent).await,
                    Err(error) => {
                        logging::error(format!("{error:#}"));
                        pending.extend(touched);
                    }
                }
            }
        }
    }
}

async fn drain_into(receiver: &mut UnboundedReceiver<Vec<PathBuf>>, pending: &mut BTreeSet<PathBuf>) {
    let started = Instant::now();
    while started.elapsed() < DRAIN_CAP {
        let quiet = if pending.len() > BURST_PATHS { DRAIN_BURST } else { DRAIN_QUIET };
        match tokio::time::timeout(quiet, receiver.recv()).await {
            Ok(Some(more)) => pending.extend(more),
            _ => break,
        }
    }
}

#[derive(Clone)]
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
    let work = Work { upserts: select_changed(&candidates, manifest), removals };
    if work.upserts.is_empty() && work.removals.is_empty() {
        return Ok(Vec::new());
    }

    let sent = match transfer(ctx, manifest, work.clone()).await {
        Ok(sent) => sent,
        Err(error) => {
            logging::warn(format!("{error:#}"));
            ctx.dav.wait_until_ready(None).await?;
            transfer(ctx, manifest, work).await?
        }
    };

    manifest.save(&ctx.manifest_path)?;
    Ok(sent)
}

async fn transfer(ctx: &Ctx, manifest: &mut Manifest, work: Work) -> Result<Vec<String>> {
    let mut sent = work.removals.clone();

    if !work.removals.is_empty() {
        delete_paths(ctx, &work.removals).await?;
        for path in &work.removals {
            manifest.forget(path);
            manifest.forget_prefix(path);
        }
    }

    if work.upserts.is_empty() {
        return Ok(sent);
    }

    let names: Vec<String> = work.upserts.iter().map(|file| file.relative.clone()).collect();
    let recorded = upload_files(ctx, work.upserts, None).await?;
    for (relative, entry) in recorded {
        manifest.record(relative, entry);
    }
    for name in names.iter().take(20) {
        logging::upload(name);
    }
    if names.len() > 20 {
        logging::info(format!("... and {} more file(s)", names.len() - 20));
    }

    sent.extend(names);
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
