//! A language server for Salesforce B2C Commerce (SFCC) projects.
//!
//! An SFCC checkout gives a general-purpose editor nothing to work with: no
//! type definitions for the `dw.*` API, no way to resolve a cartridge-relative
//! `require`, no record of which of the four cartridges declaring a route is
//! the one that runs. This crate answers those questions from the checkout
//! alone — no instance, no network, no `node_modules`.
//!
//! # What it answers
//!
//! | Request | For |
//! | ------- | --- |
//! | [`textDocument/definition`](server) | template paths, `require` paths, resource keys, routes |
//! | [`textDocument/completion`](complete) | ISML tags, the [`dw.*` API](api), [custom attributes](metadata), template paths, route names |
//! | [`textDocument/hover`](hover) | API signatures, and the [override chain](routes) of a route |
//! | [`textDocument/publishDiagnostics`](diagnose) | unknown custom attributes, and [configuration](validate) that does not resolve |
//!
//! # How it finds things
//!
//! [`workspace`] discovers the cartridges in the open folder and indexes what
//! the other modules need lazily — templates, controllers, resource keys — so
//! a session only pays for the questions it asks. [`cartridgepath`] reads the
//! override order from `dw.json` or a site archive; without it the server
//! still answers, but says the order is unknown rather than guessing.
//!
//! Every check is silent when the evidence for it is absent. No metadata in
//! the folder means no attribute diagnostics at all, because an unknown
//! attribute and an unknown instance are indistinguishable.

#![warn(missing_docs)]

pub mod api;
pub mod cartridgepath;
pub mod complete;
pub mod custom;
pub mod diagnose;
pub mod errors;
pub mod hover;
pub mod isml;
pub mod metadata;
pub mod reference;
pub mod resolve;
pub mod routes;
pub mod server;
pub mod sync;
pub mod validate;
pub mod workspace;
