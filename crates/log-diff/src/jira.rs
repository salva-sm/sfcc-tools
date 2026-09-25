//! A Jira ticket for a signature, with what the ledger knows about it, so the
//! person picking it up starts from the failure rather than from a link.
//!
//! Jira Cloud's REST API, authenticated with an account's email and an API
//! token: `JIRA_URL`, `JIRA_EMAIL` and `JIRA_API_TOKEN`.

use crate::ledger::Known;
use crate::notify::headline;
use crate::team::Ticket;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// Where the ticket goes.
pub struct Target {
    /// `https://acme.atlassian.net`.
    pub url: String,
    /// The account creating it.
    pub email: String,
    /// Its API token.
    pub token: String,
    /// The project key.
    pub project: String,
    /// The issue type: Bug, Task...
    pub kind: String,
}

impl Target {
    /// The site and credentials from the environment; the project and type as given.
    pub fn from_env(project: String, kind: String) -> Result<Target> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .with_context(|| format!("{name} is not set"))
        };
        Ok(Target {
            url: var("JIRA_URL")?.trim_end_matches('/').to_string(),
            email: var("JIRA_EMAIL")?,
            token: var("JIRA_API_TOKEN")?,
            project,
            kind,
        })
    }
}

/// The issue to create: a summary line, and a description in Atlassian
/// Document Format - the failure, where, how often, and the scrubbed example.
pub fn issue(
    target: &Target,
    id: &str,
    environment: &str,
    known: &Known,
    link: Option<&str>,
) -> Value {
    let what = headline(
        &known.label,
        known.exception_class.as_deref(),
        known.location.as_deref(),
    );
    let paragraph = |text: String| json!({ "type": "paragraph", "content": [{ "type": "text", "text": text }] });
    let mut content = vec![
        paragraph(format!(
            "Logged {} times on {} between {} and {} ({}).",
            known.count, environment, known.first_seen, known.last_seen, known.label
        )),
        paragraph(format!("Signature {id}, from log-diff.")),
    ];
    if let Some(sha) = &known.first_deploy_sha {
        content.push(paragraph(format!("First seen after deploy {sha}.")));
    }
    if let Some(link) = link {
        content.push(json!({ "type": "paragraph", "content": [{
            "type": "text", "text": "Code", "marks": [{ "type": "link", "attrs": { "href": link } }]
        }]}));
    }
    content.push(
        json!({ "type": "codeBlock", "content": [{ "type": "text", "text": known.example }] }),
    );

    json!({ "fields": {
        "project": { "key": target.project },
        "issuetype": { "name": target.kind },
        "summary": format!("[{}] {}", environment.to_uppercase(), what.chars().take(200).collect::<String>()),
        "labels": ["log-diff", environment],
        "description": { "type": "doc", "version": 1, "content": content },
    }})
}

/// Create the issue, and return what to remember of it.
pub async fn create(target: &Target, issue: &Value) -> Result<Ticket> {
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .post(format!("{}/rest/api/3/issue", target.url))
        .basic_auth(&target.email, Some(&target.token))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(issue.to_string())
        .send()
        .await
        .context("cannot reach Jira")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "Jira answered HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        );
    }
    let created: Value =
        serde_json::from_str(&body).context("Jira answered something unreadable")?;
    let key = created["key"]
        .as_str()
        .context("Jira's answer has no issue key")?
        .to_string();
    Ok(Ticket {
        url: format!("{}/browse/{key}", target.url),
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_issue_carries_the_failure_and_where_it_happened() {
        let target = Target {
            url: "https://acme.atlassian.net".into(),
            email: String::new(),
            token: String::new(),
            project: "SHOP".into(),
            kind: "Bug".into(),
        };
        let known = Known {
            label: "error".into(),
            exception_class: Some("TypeError".into()),
            location: Some("app_x/cartridge/a.js:9".into()),
            example: "TypeError: boom".into(),
            first_seen: "2026-09-22T10:00:00Z".into(),
            last_seen: "2026-09-23T10:00:00Z".into(),
            count: 42,
            first_deploy_sha: Some("9451cff".into()),
            pending: false,
            back: false,
            resolved_at: None,
            muted: false,
            spiked_on: None,
        };
        let issue = issue(&target, "abc", "prd", &known, None);

        assert_eq!(issue["fields"]["project"]["key"], "SHOP");
        assert_eq!(
            issue["fields"]["summary"],
            "[PRD] TypeError at app_x/cartridge/a.js:9"
        );
        assert_eq!(issue["fields"]["labels"][1], "prd");
        let blocks = issue["fields"]["description"]["content"]
            .as_array()
            .unwrap();
        assert_eq!(blocks.last().unwrap()["type"], "codeBlock");
    }
}
