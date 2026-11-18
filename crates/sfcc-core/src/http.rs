//! The one HTTP client the SFCC-instance calls share, with the sandbox's self-signed
//! certificate honored only when `dw.json` allows it.

use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_IDLE_PER_HOST: usize = 16;

/// `accept_invalid_certs` is the sandbox's alone; the Salesforce-run APIs present a valid
/// certificate and pass `false`.
pub fn client(timeout: Duration, accept_invalid_certs: bool) -> Result<Client> {
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .pool_max_idle_per_host(POOL_IDLE_PER_HOST)
        .danger_accept_invalid_certs(accept_invalid_certs)
        .build()
        .context("cannot build the HTTP client")
}
