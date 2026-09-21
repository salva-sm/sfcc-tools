//! What every tool in this repository needs before it can talk to a sandbox.
//!
//! One `dw.json` reader, shared. The uploader, the debug adapter and anything
//! that comes later all take the same credentials from the same file and obey
//! the same rule about which instances may be written to — and they should not
//! each have their own opinion about what that file means.

#![warn(missing_docs)]

/// The sandbox credentials in `dw.json`, and what may be written to.
pub mod config;
