//! A Debug Adapter Protocol adapter for server-side SFCC scripts.
//!
//! Zed, like any editor, speaks DAP; the instance speaks its own REST API and
//! has to be asked repeatedly whether anything stopped. This sits between
//! them, and needs nothing but the binary — no Node, and no CLI.

#![warn(missing_docs)]

pub mod adapter;
pub mod paths;
pub mod protocol;
pub mod sdapi;
pub mod variables;
