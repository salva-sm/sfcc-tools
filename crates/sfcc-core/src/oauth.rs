//! The Account Manager token, for whichever client holds an API client id and secret.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const OAUTH_URL: &str = "https://account.demandware.com/dwsso/oauth2/access_token";

struct Token {
    value: String,
    expires_at: Instant,
}

/// One token, asked for again a minute before it expires.
#[derive(Default)]
pub(crate) struct TokenCache {
    token: RwLock<Option<Token>>,
}

impl TokenCache {
    pub(crate) async fn get(&self, client: &Client, id: &str, secret: &str) -> Result<String> {
        if let Some(token) = self.token.read().await.as_ref()
            && token.expires_at > Instant::now()
        {
            return Ok(token.value.clone());
        }

        let response = client
            .post(OAUTH_URL)
            .basic_auth(id, Some(secret))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .send()
            .await
            .context("cannot reach Account Manager for an access token")?;

        if !response.status().is_success() {
            bail!(
                "Account Manager rejected the client credentials (HTTP {})",
                response.status()
            );
        }

        let body = response
            .text()
            .await
            .context("cannot read the token response")?;
        let payload: serde_json::Value =
            serde_json::from_str(&body).context("malformed token response")?;
        let value = payload["access_token"]
            .as_str()
            .context("token response without access_token")?
            .to_string();
        let lifetime = payload["expires_in"]
            .as_u64()
            .unwrap_or(1800)
            .saturating_sub(60);

        *self.token.write().await = Some(Token {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(lifetime),
        });
        Ok(value)
    }
}
