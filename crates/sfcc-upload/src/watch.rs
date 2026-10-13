use crate::active;
use crate::checkout;
use crate::daemon;
use crate::guard::{self, Overwrite};
use crate::handoff::{self, Reply, Request};
use crate::lock;
use crate::logging::{self, Change};
use crate::manifest::Manifest;
use crate::problems;
use crate::push::{
    Ctx, PushOptions, Stats, delete_paths, forget_files, push_with, select_changed, upload_files,
};
use crate::reload::{Browser, worth_reloading};
use crate::scan::{LocalFile, collect_files, describe};
use crate::sync_status;
use crate::webdav::Availability;
use anyhow::{Context, Result, bail};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{NoCache, new_debouncer_opt};
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
// After a failure: probe after this, doubling up to RETRY_MAX; a save probes at once.
const RETRY_FIRST: Duration = Duration::from_secs(10);
const RETRY_MAX: Duration = Duration::from_secs(60);
const REQUESTS_EVERY: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy)]
pub struct WatchOptions {
    pub initial_push: bool,
    pub full: bool,
    pub reload_port: Option<u16>,
    pub problems: bool,
}

/// What waits for the sandbox besides the saves.
#[derive(Debug, Default)]
struct Backlog {
    push: Option<PushOptions>,
    activate: bool,
    /// So a push handed over is answered at once, not after a probe times out.
    down: Option<String>,
}

/// On a task of its own, so a watcher waiting or uploading is not taken for gone.
struct Beating(tokio::task::JoinHandle<()>);

impl Drop for Beating {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn beat(heartbeat: sfcc_core::state::Daemon) -> Beating {
    Beating(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(sfcc_core::state::HEARTBEAT_EVERY);
        loop {
            ticker.tick().await;
            heartbeat.beat();
        }
    }))
}

pub async fn watch(ctx: Ctx, options: WatchOptions) -> Result<()> {
    let _uploader = match lock::take(&ctx.config, lock::Kind::Watcher).await? {
        Ok(held) => held,
        Err(owner) => bail!(
            "{} already uploads to {} - one watcher at a time; `sfcc-upload stop` ends a background one",
            owner.describe(),
            ctx.config.code_version
        ),
    };
    let mut manifest = Manifest::load(&ctx.manifest_path);
    if let Some(owner) = checkout::other(&ctx.config, &manifest) {
        bail!(
            "{}\n  {}",
            checkout::refusal(&ctx.config, &owner),
            checkout::way_out(&ctx.config)
        );
    }
    checkout::claim(&ctx.config, &mut manifest);
    handoff::clear_replies(&ctx.config);
    let _beating = beat(daemon::of(&ctx.config));

    sync_status::publish(&ctx.config, sync_status::State::Uploading);
    if options.problems {
        problems::enable();
    }
    problems::begin("uploading");
    let first = ctx.dav.availability().await;
    if let Availability::Unauthorized = first {
        let reason = "the sandbox rejected the credentials from dw.json (HTTP 401/403)";
        problems::failed([], &ctx.config.dw_json, reason);
        bail!(reason);
    }

    // Before the first push, so no save is missed while it runs or waits.
    let (sender, mut receiver) = unbounded_channel::<Vec<PathBuf>>();
    // No file-id cache: with it, on a 9,000-file checkout, most of a 200-file deletion never
    // came out of the debouncer - no error, no rescan. Only paths are used here anyway.
    let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
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
        NoCache,
        notify::Config::default(),
    )
    .context("cannot start the file watcher")?;

    debouncer
        .watch(&ctx.config.cartridges_dir, RecursiveMode::Recursive)
        .with_context(|| format!("cannot watch {}", ctx.config.cartridges_dir.display()))?;

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

    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut retry: Option<Retry> = None;
    let mut backlog = Backlog {
        push: options.initial_push.then_some(PushOptions {
            full: options.full,
            dry_run: false,
            show_progress: !options.problems,
            overwrite: Overwrite::Never,
        }),
        activate: true,
        down: None,
    };
    let browser = browser.as_ref();
    match first {
        Availability::Unavailable(reason) => {
            let not_ready = NotReady {
                reason: format!("sandbox unavailable ({reason})"),
                down: true,
            };
            backlog.down = Some(not_ready.reason.clone());
            fail(
                &ctx,
                &pending,
                backlog.push.is_some(),
                &not_ready,
                &mut retry,
            );
        }
        _ => {
            retry_queued(
                &ctx,
                &mut manifest,
                &mut pending,
                &mut backlog,
                browser,
                &mut retry,
            )
            .await
        }
    }

    let mut requests = tokio::time::interval(REQUESTS_EVERY);
    loop {
        let due = retry.map(|retry| retry.at);
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                logging::info("stopping the watcher");
                manifest.save(&ctx.manifest_path)?;
                sync_status::clear(&ctx.config);
                return Ok(());
            }
            _ = sleep_until(due), if due.is_some() => {
                retry_queued(&ctx, &mut manifest, &mut pending, &mut backlog, browser, &mut retry).await;
            }
            _ = requests.tick() => {
                for request in handoff::take(&ctx.config) {
                    let reply = serve(&ctx, &mut manifest, &mut pending, &mut backlog, &mut retry, &request).await;
                    handoff::answer(&ctx.config, &request, &reply);
                }
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
                    None => attempt(&ctx, &mut manifest, &mut pending, browser, &mut retry).await,
                    Some(_) => retry_queued(&ctx, &mut manifest, &mut pending, &mut backlog, browser, &mut retry).await,
                }
            }
        }
    }
}

async fn serve(
    ctx: &Ctx,
    manifest: &mut Manifest,
    pending: &mut BTreeSet<PathBuf>,
    backlog: &mut Backlog,
    retry: &mut Option<Retry>,
    request: &Request,
) -> Reply {
    let options = PushOptions {
        full: request.full,
        dry_run: false,
        show_progress: false,
        overwrite: match request.overwrite {
            true => Overwrite::Always,
            false => Overwrite::Never,
        },
    };
    logging::info(match request.full {
        true => "push --full handed over",
        false => "push handed over",
    });
    let owe = |backlog: &mut Backlog| {
        let owed = backlog.push.get_or_insert(PushOptions {
            overwrite: Overwrite::Never,
            ..options
        });
        owed.full |= request.full;
    };
    // Probed again now, so a sandbox just started gets its push straight away.
    if let Some(reason) = backlog.down.clone() {
        owe(backlog);
        if let Some(due) = retry.as_mut() {
            due.at = tokio::time::Instant::now();
        }
        return Reply::Down { reason };
    }
    if let Err(not_ready) = probe(ctx).await {
        owe(backlog);
        backlog.down = not_ready.down.then(|| not_ready.reason.clone());
        fail(ctx, pending, true, &not_ready, retry);
        return Reply::Down {
            reason: not_ready.reason,
        };
    }
    match catch_up(ctx, manifest, pending, backlog, retry, Some(options)).await {
        Ok(stats) => Reply::Done {
            uploaded: stats.uploaded,
            deleted: stats.deleted,
            held: stats.held,
        },
        Err(error) => Reply::Failed { error },
    }
}

/// A push scans the whole checkout, so it takes the queued saves along.
async fn catch_up(
    ctx: &Ctx,
    manifest: &mut Manifest,
    pending: &mut BTreeSet<PathBuf>,
    backlog: &mut Backlog,
    retry: &mut Option<Retry>,
    asked: Option<PushOptions>,
) -> std::result::Result<Stats, String> {
    let owed = backlog.push;
    let Some(options) = asked.or(owed) else {
        return Ok(Stats::default());
    };
    let options = PushOptions {
        full: options.full || owed.is_some_and(|owed| owed.full),
        ..options
    };
    sync_status::publish_uploading(&ctx.config, pending.len());
    problems::begin("uploading");
    match push_with(ctx, manifest, options).await {
        Ok(stats) => {
            backlog.push = None;
            pending.clear();
            if retry.take().is_some() {
                logging::ok("sandbox is back - the queued changes are uploaded");
            }
            report(ctx, &stats.held);
            if std::mem::take(&mut backlog.activate) {
                active::ensure(&ctx.config).await;
            }
            Ok(stats)
        }
        Err(error) => {
            let not_ready = NotReady {
                reason: format!("{error:#}"),
                down: false,
            };
            fail(ctx, pending, owed.is_some(), &not_ready, retry);
            Err(not_ready.reason)
        }
    }
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
        Ok(synced) => {
            if retry.take().is_some() {
                logging::ok("sandbox is back - the queued changes are uploaded");
            }
            report(ctx, &synced.held);
            refresh_browser(browser, &synced.sent).await;
        }
        Err(error) => {
            pending.extend(touched);
            let not_ready = NotReady {
                reason: format!("{error:#}"),
                down: false,
            };
            fail(ctx, pending, false, &not_ready, retry);
        }
    }
}

fn report(ctx: &Ctx, held: &[String]) {
    match held.is_empty() {
        true => {
            problems::synced();
            sync_status::publish(&ctx.config, sync_status::State::Synced);
        }
        false => hold(ctx, held),
    }
}

/// Not retried: waiting will not make someone else's change ours.
fn hold(ctx: &Ctx, held: &[String]) {
    let detail = format!(
        "{} file(s) changed on the sandbox by someone else, not overwritten - `sfcc-upload push --overwrite` replaces them",
        held.len()
    );
    logging::warn(&detail);
    let files: Vec<PathBuf> = held
        .iter()
        .map(|relative| ctx.config.cartridges_dir.join(relative))
        .collect();
    problems::failed(&files, &ctx.config.dw_json, &detail);
    sync_status::publish_failure(&ctx.config, detail);
}

async fn retry_queued(
    ctx: &Ctx,
    manifest: &mut Manifest,
    pending: &mut BTreeSet<PathBuf>,
    backlog: &mut Backlog,
    browser: Option<&Browser>,
    retry: &mut Option<Retry>,
) {
    if let Err(not_ready) = probe(ctx).await {
        problems::begin("retrying");
        backlog.down = not_ready.down.then(|| not_ready.reason.clone());
        fail(ctx, pending, backlog.push.is_some(), &not_ready, retry);
        return;
    }
    backlog.down = None;
    if backlog.push.is_some() {
        let _ = catch_up(ctx, manifest, pending, backlog, retry, None).await;
        return;
    }
    attempt(ctx, manifest, pending, browser, retry).await;
    if retry.is_none() && std::mem::take(&mut backlog.activate) {
        active::ensure(&ctx.config).await;
    }
}

fn fail(
    ctx: &Ctx,
    pending: &BTreeSet<PathBuf>,
    push_owed: bool,
    not_ready: &NotReady,
    retry: &mut Option<Retry>,
) {
    let next = Retry::after(*retry);
    let queued = match (push_owed, pending.len()) {
        (true, 0) => "the push is queued".to_string(),
        (true, changes) => format!("the push and {changes} change(s) are queued"),
        (false, changes) => format!("{changes} change(s) queued"),
    };
    let detail = format!(
        "{} - {queued}, retrying in {}s",
        not_ready.reason,
        next.delay.as_secs()
    );
    logging::error(&detail);
    problems::failed(pending, &ctx.config.dw_json, &detail);
    match not_ready.down {
        true => sync_status::publish_down(&ctx.config, detail, not_ready.reason.clone()),
        false => sync_status::publish_failure(&ctx.config, detail),
    }
    *retry = Some(next);
}

struct NotReady {
    reason: String,
    down: bool,
}

async fn probe(ctx: &Ctx) -> std::result::Result<(), NotReady> {
    let refused = |reason: String| NotReady {
        reason,
        down: false,
    };
    match ctx.dav.availability().await {
        Availability::Ready => Ok(()),
        Availability::MissingCodeVersion => ctx
            .dav
            .mkcol(ctx.dav.base_url())
            .await
            .map_err(|error| refused(format!("{error:#}"))),
        Availability::Unauthorized => Err(refused(
            "the sandbox rejected the credentials from dw.json (HTTP 401/403)".to_string(),
        )),
        Availability::Unavailable(reason) => Err(NotReady {
            reason: format!("sandbox unavailable ({reason})"),
            down: true,
        }),
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

struct Synced {
    sent: Vec<String>,
    held: Vec<String>,
}

async fn synchronize(
    ctx: &Ctx,
    manifest: &mut Manifest,
    touched: &BTreeSet<PathBuf>,
) -> Result<Synced> {
    let (candidates, removals) = classify(ctx, touched);
    let upserts = select_changed(&candidates, manifest);
    let settled = guard::settle(ctx, manifest, upserts, removals, Overwrite::Never).await?;
    let work = Work {
        upserts: settled.upserts,
        removals: settled.removals,
    };
    let held = settled.held;
    if work.upserts.is_empty() && work.removals.is_empty() {
        return Ok(Synced {
            sent: Vec::new(),
            held,
        });
    }

    let outcome = transfer(ctx, manifest, work).await;
    // Saved on failure too, so forgotten files stay forgotten if the watcher stops now.
    manifest.save(&ctx.manifest_path)?;
    outcome.map(|sent| Synced { sent, held })
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
        guard::remember(ctx, manifest, &names).await;
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
