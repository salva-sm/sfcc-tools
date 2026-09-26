//! A save while the sandbox is down, against a WebDAV server in memory.

use super::*;
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

    server.set_down(true);
    let mut pending = BTreeSet::from([checkout.save("app_x/cartridge/a.js", "one")]);
    attempt(ctx, &mut manifest, &mut pending, None, &mut retry).await;

    assert_eq!(pending.len(), 1, "kept for the retry");
    assert_eq!(retry.map(|retry| retry.delay), Some(RETRY_FIRST));
    let status = checkout.status();
    assert_eq!(status.state, State::Failed);
    assert!(status.detail.unwrap().contains("1 change(s) queued"));

    // Still down: probed, not uploaded, and the wait grows.
    pending.insert(checkout.save("app_x/cartridge/b.js", "two"));
    retry_queued(ctx, &mut manifest, &mut pending, None, &mut retry).await;
    assert_eq!(retry.map(|retry| retry.delay), Some(RETRY_FIRST * 2));
    assert!(
        checkout
            .status()
            .detail
            .unwrap()
            .contains("sandbox unavailable (HTTP 503")
    );

    server.set_down(false);
    retry_queued(ctx, &mut manifest, &mut pending, None, &mut retry).await;
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
