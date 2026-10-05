//! The code versions, through the Data API client in sfcc-core.

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use sfcc_core::config::Config;

pub struct Ocapi {
    api: sfcc_core::ocapi::Ocapi,
}

impl Ocapi {
    pub fn new(config: &Config) -> Result<Ocapi> {
        if config.api_client.is_none() {
            bail!(
                "activating a code version needs an API client - put \"client-id\" and \
                 \"client-secret\" (or the custom-sfcc-ci block) in dw.json"
            );
        }
        Ok(Ocapi {
            api: sfcc_core::ocapi::Ocapi::new(config)?,
        })
    }

    pub async fn activate(&self, code_version: &str) -> Result<()> {
        let response = self
            .api
            .data(Method::PATCH, &format!("code_versions/{code_version}"))
            .await?
            .header("Content-Type", "application/json")
            .body(r#"{"active":true}"#)
            .send()
            .await
            .with_context(|| format!("cannot reach the Data API on {}", self.api.hostname()))?;

        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND => {
                bail!("code version {code_version} does not exist on the sandbox")
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => bail!(
                "the API client is not allowed to touch code versions on {} - add \
                 /code_versions to its OCAPI Data settings",
                self.api.hostname()
            ),
            status => bail!("activating {code_version} failed with HTTP {status}"),
        }
    }

    /// The code version the sandbox serves, if any is active.
    pub async fn active_code_version(&self) -> Result<Option<String>> {
        let response = self
            .api
            .data(Method::GET, "code_versions")
            .await?
            .send()
            .await
            .with_context(|| format!("cannot reach the Data API on {}", self.api.hostname()))?;

        match response.status() {
            status if status.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => bail!(
                "the API client is not allowed to read code versions on {} - add \
                 /code_versions to its OCAPI Data settings",
                self.api.hostname()
            ),
            status => bail!("listing the code versions failed with HTTP {status}"),
        }

        let body = response
            .text()
            .await
            .context("cannot read the code version list")?;
        active_in(&body)
    }
}

fn active_in(body: &str) -> Result<Option<String>> {
    let payload: serde_json::Value =
        serde_json::from_str(body).context("malformed code version list")?;
    let versions = payload["data"]
        .as_array()
        .context("code version list without data")?;
    Ok(versions
        .iter()
        .find(|version| version["active"].as_bool() == Some(true))
        .and_then(|version| version["id"].as_str())
        .map(str::to_string))
}

#[cfg(test)]
#[path = "ocapi_tests.rs"]
mod tests;
