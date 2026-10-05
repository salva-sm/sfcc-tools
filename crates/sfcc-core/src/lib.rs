//! The `dw.json` reader, the state layout, the WebDAV and OCAPI clients and the log reader every tool here shares.

#[cfg(feature = "completions")]
pub mod completions;
pub mod config;
#[cfg(feature = "daemon")]
pub mod daemon;
#[cfg(feature = "webdav")]
pub mod logs;
#[cfg(any(feature = "webdav", feature = "ocapi"))]
mod oauth;
#[cfg(feature = "ocapi")]
pub mod ocapi;
pub mod state;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(feature = "webdav")]
pub mod webdav;
