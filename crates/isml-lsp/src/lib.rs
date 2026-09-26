//! A language server for SFCC checkouts, answering from the checkout alone: no instance, no network.

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
