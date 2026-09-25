//! Several environments' ledgers at once - `dev=ledgers/dev.json` - for the
//! commands that look across them: the summary and the dashboard. Or all of
//! them straight from the ledger repository on GitHub, so nothing generated
//! from them ever has to be kept there.

use crate::ledger::{Ledger, fetch};
use crate::team::Team;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Duration;

/// The environments a ledger repository keeps, in the order they are shown.
pub const ENVIRONMENTS: [&str; 3] = ["dev", "stg", "prd"];

/// One environment and what its ledger knows.
pub struct Environment {
    /// Its name, as given: dev, stg, prd.
    pub name: String,
    /// Its ledger.
    pub ledger: Ledger,
}

/// Read `name=path` or `name=url` specs; a bare path is named after its file.
/// A ledger not written yet is an environment with nothing in it, not an error:
/// a repository set up for three environments can start with one.
pub async fn load(specs: &[String]) -> Result<Vec<Environment>> {
    let mut environments = Vec::new();
    for spec in specs {
        let (name, source) = match spec.split_once('=') {
            Some((name, source)) if !name.contains(['/', '\\']) => (name.to_string(), source),
            _ => (
                Path::new(spec)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| spec.clone()),
                spec.as_str(),
            ),
        };
        let ledger = match source.starts_with("http://") || source.starts_with("https://") {
            true => Ledger::parse(
                &fetch(source)
                    .await
                    .with_context(|| format!("cannot fetch {source}"))?,
            )?,
            false => Ledger::load(Path::new(source))?,
        };
        environments.push(Environment { name, ledger });
    }
    Ok(environments)
}

/// Every environment's ledger and the team file, read from a ledger
/// repository on GitHub - `owner/repo`, or `owner/repo@branch` - through the
/// API, with the token of the environment or of the `gh` login.
pub async fn from_repository(spec: &str) -> Result<(Vec<Environment>, Team)> {
    let (slug, branch) = match spec.split_once('@') {
        Some((slug, branch)) => (slug, branch),
        None => (spec, "main"),
    };
    if slug.split('/').count() != 2 {
        bail!("{spec:?} is not a repository: owner/repo, or owner/repo@branch");
    }
    let token = github_token().context(
        "reading a private repository takes a token: set GITHUB_TOKEN, or log in with `gh auth login`",
    )?;
    // A missing file is an environment not read yet; a missing repository is
    // a mistake, and GitHub says 404 for one the token cannot see as well.
    let status = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(format!("https://api.github.com/repos/{slug}"))
        .header("User-Agent", "log-diff")
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("cannot reach GitHub for {slug}"))?
        .status();
    if !status.is_success() {
        bail!("{slug} is not a repository this token can read (HTTP {status})");
    }

    let mut environments = Vec::new();
    for name in ENVIRONMENTS {
        let ledger =
            match github_file(slug, branch, &format!("ledgers/{name}.json"), &token).await? {
                Some(raw) => Ledger::parse(&raw)
                    .with_context(|| format!("cannot read ledgers/{name}.json"))?,
                None => Ledger::default(),
            };
        environments.push(Environment {
            name: name.to_string(),
            ledger,
        });
    }
    let team = match github_file(slug, branch, "team.json", &token).await? {
        Some(raw) => serde_json::from_str(&raw).context("cannot read team.json")?,
        None => Team::default(),
    };
    Ok((environments, team))
}

/// A file of a repository, or `None` when it is not there.
async fn github_file(slug: &str, branch: &str, path: &str, token: &str) -> Result<Option<String>> {
    let url = format!("https://api.github.com/repos/{slug}/contents/{path}?ref={branch}");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?
        .get(&url)
        .header("User-Agent", "log-diff")
        .header("Accept", "application/vnd.github.raw")
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("cannot reach GitHub for {slug}"))?;
    match response.status().as_u16() {
        200 => Ok(Some(response.text().await?)),
        404 => Ok(None),
        401 | 403 => bail!(
            "GitHub refused {slug}/{path} (HTTP {}) - does the token reach that repository?",
            response.status()
        ),
        _ => bail!(
            "GitHub answered HTTP {} for {slug}/{path}",
            response.status()
        ),
    }
}

/// `LOG_DIFF_TOKEN`, `GITHUB_TOKEN` or `GH_TOKEN`, or the token `gh` is
/// logged in with - which is most developers' already.
pub fn github_token() -> Option<String> {
    for name in ["LOG_DIFF_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = std::env::var(name)
            && !token.trim().is_empty()
        {
            return Some(token.trim().to_string());
        }
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !token.is_empty()).then_some(token)
}
