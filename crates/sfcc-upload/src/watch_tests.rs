//! A save while the sandbox is down, against a WebDAV server in memory.

use super::*;
use crate::handoff::{Reply, Request};
use crate::push::push;
use crate::sync_status::{State, Status, status_path};
use sfcc_core::testing::MockDav;

struct Checkout {
    root: PathBuf,
    ctx: Ctx,
}

impl Checkout {
    fn new(name: &str, server: &MockDav) -> Checkout {
        let root = std::env::temp_dir().join(format!("sfcc-upload-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("cartridges/app_x/cartridge")).unwrap();
        let mut config = server.config();
        config.cartridges_dir = root.join("cartridges");
        let mut ctx = Ctx::new(config, 2).unwrap();
        ctx.manifest_path = root.join("manifest.json");
        Checkout { root, ctx }
    }

    fn save(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.ctx.config.cartridges_dir.join(relative);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn status(&self) -> Status {
        serde_json::from_str(&std::fs::read_to_string(status_path(&self.ctx.config)).unwrap())
            .unwrap()
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        sync_status::clear(&self.ctx.config);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn a_save_while_the_sandbox_is_down_is_queued_said_and_sent_when_it_is_back() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("down", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut retry = None;
    let mut held = Held::default();

    server.set_down(true);
    let mut pending = BTreeSet::from([checkout.save("app_x/cartridge/a.js", "one")]);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;

    assert_eq!(pending.len(), 1, "kept for the retry");
    assert_eq!(retry.map(|retry| retry.delay), Some(RETRY_FIRST));
    let status = checkout.status();
    assert_eq!(status.state, State::Failed);
    assert!(status.detail.unwrap().contains("1 change(s) queued"));

    // Still down: probed, not uploaded, and the wait grows.
    pending.insert(checkout.save("app_x/cartridge/b.js", "two"));
    retry_queued(
        ctx,
        &mut manifest,
        &mut pending,
        &mut Backlog::default(),
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(retry.map(|retry| retry.delay), Some(RETRY_FIRST * 2));
    assert!(
        checkout
            .status()
            .detail
            .unwrap()
            .contains("sandbox unavailable (HTTP 503")
    );

    server.set_down(false);
    retry_queued(
        ctx,
        &mut manifest,
        &mut pending,
        &mut Backlog::default(),
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert!(pending.is_empty());
    assert!(retry.is_none());
    assert_eq!(checkout.status().state, State::Synced);
    assert_eq!(
        server
            .file("Cartridges/version1/app_x/cartridge/a.js")
            .as_deref(),
        Some(b"one".as_slice())
    );
    assert!(
        server
            .file("Cartridges/version1/app_x/cartridge/b.js")
            .is_some()
    );
}

#[test]
fn the_wait_between_retries_doubles_up_to_a_minute() {
    let mut retry = Retry::after(None);
    let mut delays = vec![retry.delay.as_secs()];
    for _ in 0..4 {
        retry = Retry::after(Some(retry));
        delays.push(retry.delay.as_secs());
    }
    assert_eq!(delays, vec![10, 20, 40, 60, 60]);
}

const A_JS: &str = "Cartridges/version1/app_x/cartridge/a.js";
const B_JS: &str = "Cartridges/version1/app_x/cartridge/b.js";

fn on_server(server: &MockDav, path: &str) -> Option<String> {
    server
        .file(path)
        .map(|bytes| String::from_utf8(bytes).unwrap())
}

fn held_paths(status: &Status) -> Vec<&str> {
    status
        .held
        .iter()
        .map(|conflict| conflict.path.as_str())
        .collect()
}

#[tokio::test]
async fn a_file_someone_else_wrote_since_is_neither_overwritten_nor_deleted() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("overwritten", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut retry = None;
    let mut held = Held::default();

    let mut pending = BTreeSet::from([
        checkout.save("app_x/cartridge/a.js", "mine"),
        checkout.save("app_x/cartridge/b.js", "mine"),
    ]);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine"));

    pending.insert(checkout.save("app_x/cartridge/a.js", "mine, second"));
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine, second"));
    assert_eq!(checkout.status().state, State::Synced);

    server.put(A_JS, "theirs");
    server.put(B_JS, "theirs");
    pending.insert(checkout.save("app_x/cartridge/a.js", "mine, third"));
    let b = ctx.config.cartridges_dir.join("app_x/cartridge/b.js");
    std::fs::remove_file(&b).unwrap();
    pending.insert(b);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;

    assert_eq!(on_server(&server, A_JS).as_deref(), Some("theirs"));
    assert_eq!(on_server(&server, B_JS).as_deref(), Some("theirs"));
    assert!(pending.is_empty() && retry.is_none(), "held, not retried");
    let status = checkout.status();
    assert_eq!(status.state, State::Held);
    assert!(
        status
            .detail
            .unwrap()
            .contains("2 file(s) changed on the sandbox")
    );

    let options = PushOptions {
        full: false,
        dry_run: false,
        show_progress: false,
        overwrite: Overwrite::Never,
    };
    let stats = push(ctx, options).await.unwrap();
    assert_eq!((stats.uploaded, stats.deleted, stats.held.len()), (0, 0, 2));
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("theirs"));

    let options = PushOptions {
        overwrite: Overwrite::Always,
        ..options
    };
    let stats = push(ctx, options).await.unwrap();
    assert_eq!((stats.uploaded, stats.deleted, stats.held.len()), (1, 1, 0));
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine, third"));
    assert_eq!(on_server(&server, B_JS), None);
}

#[tokio::test]
async fn a_held_file_stays_held_through_a_later_synced_batch() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("stays-held", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut retry = None;
    let mut held = Held::default();

    let mut pending = BTreeSet::from([checkout.save("app_x/cartridge/a.js", "mine")]);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;

    server.put(A_JS, "theirs");
    pending.insert(checkout.save("app_x/cartridge/a.js", "mine, second"));
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    let status = checkout.status();
    assert_eq!(status.state, State::Held);
    assert_eq!(held_paths(&status), vec!["app_x/cartridge/a.js"]);

    // An unrelated file goes up clean: the batch is synced, but a.js is still unresolved.
    pending.insert(checkout.save("app_x/cartridge/b.js", "unrelated"));
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(on_server(&server, B_JS).as_deref(), Some("unrelated"));
    let status = checkout.status();
    assert_eq!(
        status.state,
        State::Held,
        "the clean batch did not hide a.js"
    );
    assert_eq!(held_paths(&status), vec!["app_x/cartridge/a.js"]);

    // Overwriting a.js sends it, and only then does it leave the held set.
    let overwrite = Request {
        id: "resolve".to_string(),
        full: false,
        overwrite: true,
        resolve: Vec::new(),
        keep: false,
    };
    let mut backlog = Backlog::default();
    serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &overwrite,
    )
    .await;
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine, second"));
    assert_eq!(checkout.status().state, State::Synced);
}

#[tokio::test]
async fn resolving_a_held_file_overwrites_just_it_or_keeps_the_sandbox_copy() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("resolve", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut retry = None;
    let mut held = Held::default();
    let mut backlog = Backlog::default();

    let mut pending = BTreeSet::from([
        checkout.save("app_x/cartridge/a.js", "mine"),
        checkout.save("app_x/cartridge/b.js", "mine"),
    ]);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;

    // Another writer changes both; we edit both - both are held.
    server.put(A_JS, "theirs");
    server.put(B_JS, "theirs");
    pending.insert(checkout.save("app_x/cartridge/a.js", "mine, second"));
    pending.insert(checkout.save("app_x/cartridge/b.js", "mine, second"));
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(held_paths(&checkout.status()).len(), 2);

    // Overwrite just a.js: it goes up with our version, b.js stays held.
    let overwrite_a = Request {
        id: "o".into(),
        full: false,
        overwrite: true,
        resolve: vec!["app_x/cartridge/a.js".into()],
        keep: false,
    };
    serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &overwrite_a,
    )
    .await;
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine, second"));
    assert_eq!(on_server(&server, B_JS).as_deref(), Some("theirs"));
    assert_eq!(held_paths(&checkout.status()), vec!["app_x/cartridge/b.js"]);

    // Keep b.js: the sandbox copy stays, and it leaves the held set - synced again.
    let keep_b = Request {
        id: "k".into(),
        full: false,
        overwrite: false,
        resolve: vec!["app_x/cartridge/b.js".into()],
        keep: true,
    };
    serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &keep_b,
    )
    .await;
    assert_eq!(on_server(&server, B_JS).as_deref(), Some("theirs"));
    assert_eq!(checkout.status().state, State::Synced);
}

#[tokio::test]
async fn a_sandbox_write_that_matches_what_we_sent_is_not_a_conflict() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("same-bytes", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let mut retry = None;
    let mut held = Held::default();

    let mut pending = BTreeSet::from([checkout.save("app_x/cartridge/a.js", "mine")]);
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine"));

    // Another writer re-saves the very same bytes: the etag bumps, but nothing diverged.
    server.put(A_JS, "mine");

    pending.insert(checkout.save("app_x/cartridge/a.js", "mine, second"));
    attempt(
        ctx,
        &mut manifest,
        &mut pending,
        &mut held,
        None,
        &mut retry,
    )
    .await;

    assert_eq!(on_server(&server, A_JS).as_deref(), Some("mine, second"));
    assert_eq!(checkout.status().state, State::Synced, "not held");
}

fn handed(full: bool) -> Request {
    Request {
        id: "test".to_string(),
        full,
        overwrite: false,
        resolve: Vec::new(),
        keep: false,
    }
}

#[tokio::test]
async fn a_push_handed_over_while_the_sandbox_is_down_is_owed_and_done_when_it_is_back() {
    let server = MockDav::start().await;
    let checkout = Checkout::new("handed", &server);
    let ctx = &checkout.ctx;
    let mut manifest = Manifest::load(&ctx.manifest_path);
    let (mut pending, mut backlog, mut retry) = (BTreeSet::new(), Backlog::default(), None);
    let mut held = Held::default();
    checkout.save("app_x/cartridge/a.js", "one");

    server.set_down(true);
    let reply = serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &handed(false),
    )
    .await;
    assert!(matches!(reply, Reply::Down { ref reason } if reason.contains("HTTP 503")));
    assert!(backlog.push.is_some() && retry.is_some());
    let status = checkout.status();
    assert!(status.down.unwrap().contains("HTTP 503"));
    assert!(status.detail.unwrap().contains("the push is queued"));

    let requests = server.requests().len();
    let reply = serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &handed(false),
    )
    .await;
    assert!(matches!(reply, Reply::Down { .. }));
    assert_eq!(server.requests().len(), requests);
    assert!(retry.unwrap().at <= tokio::time::Instant::now());

    server.set_down(false);
    retry_queued(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        None,
        &mut retry,
    )
    .await;
    assert!(backlog.push.is_none() && retry.is_none());
    assert_eq!(on_server(&server, A_JS).as_deref(), Some("one"));
    let status = checkout.status();
    assert_eq!((status.state, status.down), (State::Synced, None));

    checkout.save("app_x/cartridge/b.js", "two");
    let reply = serve(
        ctx,
        &mut manifest,
        &mut pending,
        &mut backlog,
        &mut held,
        &mut retry,
        &handed(false),
    )
    .await;
    assert_eq!(
        reply,
        Reply::Done {
            uploaded: 1,
            deleted: 0,
            held: Vec::new()
        }
    );
    assert_eq!(on_server(&server, B_JS).as_deref(), Some("two"));
}
