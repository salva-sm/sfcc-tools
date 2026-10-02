//! OCAPI, with the API client in `dw.json`: the Data API with its token, the Shop API with its id.
//!
//! What a request means, and what its errors mean, is the caller's.

use crate::config::Config;
use crate::oauth::TokenCache;
use anyhow::{Context, Result};
use reqwest::{Client, Method, RequestBuilder};
use std::time::Duration;

pub const DATA_API_VERSION: &str = "v23_2";
pub const SHOP_API_VERSION: &str = "v23_2";

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

        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .build()
            .context("cannot build the OCAPI client")?;

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

    /// `path` is what follows `/dw/shop/<version>/`, as in `product_search`. The Shop API
    /// only takes the client's id, which has to be in the site's OCAPI Shop settings.
    pub fn shop(&self, site: &str, path: &str) -> RequestBuilder {
        let url = format!(
            "https://{}/s/{site}/dw/shop/{SHOP_API_VERSION}/{path}",
            self.hostname
        );
        self.client.get(url).header("x-dw-client-id", &self.id)
    }
}
