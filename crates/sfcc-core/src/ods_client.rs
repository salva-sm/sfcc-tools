//! The Sandbox API itself, authenticated with the API client of dw.json. That client needs
//! the Sandbox API User role on the realm in Account Manager; the OCAPI settings of the
//! instance do not come into it.

use super::{Operation, realm};
use crate::config::Config;
use crate::oauth::TokenCache;
use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use std::time::Duration;

const ODS_API: &str = "https://admin.dx.commercecloud.salesforce.com/api/v1";
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Sandbox {
    pub id: String,
    #[serde(default)]
    pub realm: String,
    #[serde(default)]
    pub instance: String,
    #[serde(default = "unknown")]
    pub state: String,
    #[serde(rename = "hostName", default)]
    pub host_name: String,
    /// When ODS deletes it, if it has a time to live.
    #[serde(default)]
    pub eol: Option<String>,
}

fn unknown() -> String {
    "unknown".to_string()
}

impl Sandbox {
    /// Whether it is the sandbox [`super::label`] named, by realm and instance or by its host.
    fn is(&self, label: &str) -> bool {
        let named = format!("{}-{}", self.realm, self.instance).to_lowercase();
        let by_host = self
            .host_name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_lowercase()
            .replace('_', "-");
        named == label || by_host == label
    }
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    data: Vec<Sandbox>,
}

pub struct Ods {
    client: Client,
    id: String,
    secret: String,
    token: TokenCache,
}

impl Ods {
    pub fn new(config: &Config) -> Result<Ods> {
        let api_client = config.api_client.clone().context(
            "the sandbox's state needs an API client - put \"client-id\" and \"client-secret\" \
             in dw.json, for a client with the Sandbox API User role on the realm",
        )?;
        let client = Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("cannot build the Sandbox API client")?;
        Ok(Ods {
            client,
            id: api_client.id,
            secret: api_client.secret,
            token: TokenCache::default(),
        })
    }

    /// The sandbox called `label`, from the realm's list. Never `GET /sandboxes/{id}`: ODS keeps
    /// a deleted sandbox's record under the same id, and that call can return it.
    pub async fn find(&self, label: &str) -> Result<Sandbox> {
        let realm = realm(label);
        let url = format!("{ODS_API}/sandboxes?filter_params=realm%3D{realm}");
        let response = self
            .client
            .get(&url)
            .bearer_auth(self.token().await?)
            .send()
            .await
            .context("cannot reach the Sandbox API")?;
        let body = accepted(response, realm).await?;
        matching(&body, label)?.with_context(|| {
            format!("the Sandbox API does not list {label} - deleted, or in another realm")
        })
    }

    pub async fn operate(&self, sandbox: &Sandbox, operation: Operation) -> Result<()> {
        let url = format!("{ODS_API}/sandboxes/{}/operations", sandbox.id);
        let response = self
            .client
            .post(&url)
            .bearer_auth(self.token().await?)
            .header("Content-Type", "application/json")
            .body(format!(r#"{{"operation":"{}"}}"#, operation.name()))
            .send()
            .await
            .context("cannot reach the Sandbox API")?;
        accepted(response, &sandbox.realm).await.map(|_| ())
    }

    async fn token(&self) -> Result<String> {
        self.token.get(&self.client, &self.id, &self.secret).await
    }
}

async fn accepted(response: Response, realm: &str) -> Result<String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    match status {
        status if status.is_success() => Ok(body),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => bail!(
            "the API client in dw.json may not use the Sandbox API - it needs the Sandbox API \
             User role on realm {realm} in Account Manager"
        ),
        status => match fault(&body) {
            Some(message) => bail!("the Sandbox API answered HTTP {status}: {message}"),
            None => bail!("the Sandbox API answered HTTP {status}"),
        },
    }
}

/// What ODS says went wrong, from its `{"error":{"message":...}}`.
fn fault(body: &str) -> Option<String> {
    let payload: serde_json::Value = serde_json::from_str(body).ok()?;
    payload["error"]["message"].as_str().map(str::to_string)
}

fn matching(body: &str, label: &str) -> Result<Option<Sandbox>> {
    let listing: Listing = serde_json::from_str(body).context("malformed sandbox list")?;
    Ok(listing
        .data
        .into_iter()
        .filter(|sandbox| sandbox.state != "deleted")
        .find(|sandbox| sandbox.is(label)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = r#"{"kind":"SandboxList","data":[
        {"id":"0a1b","realm":"zzzz","instance":"001","state":"deleted",
         "hostName":"zzzz-001.dx.commercecloud.salesforce.com"},
        {"id":"2c3d","realm":"zzzz","instance":"001","state":"stopped",
         "hostName":"zzzz-001.dx.commercecloud.salesforce.com","eol":"2026-12-01T00:00:00Z"},
        {"id":"4e5f","realm":"zzzz","instance":"002","state":"started",
         "hostName":"zzzz-002.dx.commercecloud.salesforce.com"}
    ]}"#;

    #[test]
    fn finds_the_sandbox_by_its_label_and_never_a_deleted_record() {
        let sandbox = matching(LISTING, "zzzz-001").unwrap().unwrap();
        assert_eq!(sandbox.id, "2c3d");
        assert_eq!(sandbox.state, "stopped");
        assert_eq!(sandbox.eol.as_deref(), Some("2026-12-01T00:00:00Z"));
    }

    #[test]
    fn a_sandbox_the_list_does_not_have_is_none() {
        assert_eq!(matching(LISTING, "zzzz-003").unwrap(), None);
        assert_eq!(matching(r#"{"data":[]}"#, "zzzz-001").unwrap(), None);
    }

    #[test]
    fn a_sandbox_without_realm_and_instance_is_found_by_its_host() {
        let body =
            r#"{"data":[{"id":"6a7b","hostName":"zzzz-004.dx.commercecloud.salesforce.com"}]}"#;
        let sandbox = matching(body, "zzzz-004").unwrap().unwrap();
        assert_eq!(sandbox.state, "unknown");
    }

    #[test]
    fn says_what_ods_reported_when_it_refuses() {
        let body = r#"{"kind":"Error","error":{"message":"Sandbox is already started"}}"#;
        assert_eq!(fault(body).as_deref(), Some("Sandbox is already started"));
        assert_eq!(fault("<html>"), None);
    }
}
