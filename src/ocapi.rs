use crate::config::Config;
use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, StatusCode};
use std::time::Duration;

const TOKEN_URL: &str = "https://account.demandware.com/dwsso/oauth2/access_token";
const DATA_API_VERSION: &str = "v23_2";

pub struct Ocapi {
    client: Client,
    hostname: String,
    id: String,
    secret: String,
}

impl Ocapi {
    pub fn new(config: &Config) -> Result<Ocapi> {
        let api_client = config.api_client.clone().context(
            "activating a code version needs an API client - put \"client-id\" and \
             \"client-secret\" (or the custom-sfcc-ci block) in dw.json",
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
        })
    }

    pub async fn activate(&self, code_version: &str) -> Result<()> {
        let url = format!(
            "https://{}/s/-/dw/data/{DATA_API_VERSION}/code_versions/{code_version}",
            self.hostname
        );

        let response = self
            .client
            .request(Method::PATCH, &url)
            .bearer_auth(self.token().await?)
            .header("Content-Type", "application/json")
            .body(r#"{"active":true}"#)
            .send()
            .await
            .with_context(|| format!("cannot reach the Data API on {}", self.hostname))?;

        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND => bail!("code version {code_version} does not exist on the sandbox"),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => bail!(
                "the API client is not allowed to touch code versions on {} - add \
                 /code_versions to its OCAPI Data settings",
                self.hostname
            ),
            status => bail!("activating {code_version} failed with HTTP {status}"),
        }
    }

    async fn token(&self) -> Result<String> {
        let response = self
            .client
            .post(TOKEN_URL)
            .basic_auth(&self.id, Some(&self.secret))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .send()
            .await
            .context("cannot reach Account Manager for an access token")?;

        if !response.status().is_success() {
            bail!("Account Manager rejected the API client (HTTP {})", response.status());
        }

        let body = response.text().await.context("cannot read the token response")?;
        let payload: serde_json::Value =
            serde_json::from_str(&body).context("malformed token response")?;
        Ok(payload["access_token"]
            .as_str()
            .context("token response without access_token")?
            .to_string())
    }
}
