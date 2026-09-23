//! What every tool in this repository needs before it can talk to a sandbox.
//!
//! One `dw.json` reader, shared. The uploader, the debug adapter and anything
//! that comes later all take the same credentials from the same file and obey
//! the same rule about which instances may be written to — and they should not
//! each have their own opinion about what that file means.
//!
//! With the `webdav` feature it also carries the WebDAV client and the log
//! reader, so the uploader and the log differ read the instance log the same
//! way without one of them running the other.

#![warn(missing_docs)]

/// Tab completion for the command-line tools.
#[cfg(feature = "completions")]
pub mod completions;
/// The sandbox credentials in `dw.json`, and what may be written to.
pub mod config;
/// The instance log: records, a mark, and what was written since.
#[cfg(feature = "webdav")]
pub mod logs;
/// WebDAV against the instance.
#[cfg(feature = "webdav")]
pub mod webdav;
