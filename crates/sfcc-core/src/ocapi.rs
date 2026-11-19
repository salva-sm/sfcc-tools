//! OCAPI's Data API, with the API client in `dw.json` and its token.
//!
//! What a request means, and what its errors mean, is the caller's.

use crate::config::Config;
use crate::oauth::TokenCache;
use anyhow::{Context, Result};
use reqwest::{Client, Method, RequestBuilder};
use std::time::Duration;

pub const DATA_API_VERSION: &str = "v23_2";

pub struct Ocapi {
    client: Client,
    hostname: String,
    id: String,
    secret: String,
    token: TokenCache,
}

impl Ocapi {
    pub fn new(config: &Config) -> Result<Ocapi> {
        let api_client = config.api_client.clone().context(
            "OCAPI needs an API client - put \"client-id\" and \"client-secret\" (or the \
             custom-sfcc-ci block) in dw.json",
        )?;

        let client = crate::http::client(Duration::from_secs(60), config.accept_invalid_certs)?;

        Ok(Ocapi {
            client,
            hostname: config.hostname.clone(),
            id: api_client.id,
            secret: api_client.secret,
            token: TokenCache::default(),
        })
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// `path` is what follows `/dw/data/<version>/`, as in `code_versions`.
    pub async fn data(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        let url = format!(
            "https://{}/s/-/dw/data/{DATA_API_VERSION}/{path}",
            self.hostname
        );
        let token = self.token.get(&self.client, &self.id, &self.secret).await?;
        Ok(self.client.request(method, url).bearer_auth(token))
    }
}
