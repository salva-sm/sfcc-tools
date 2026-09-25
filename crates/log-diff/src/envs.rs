//! Several environments' ledgers at once - `dev=ledgers/dev.json` - for the
//! commands that look across them: the summary and the dashboard.

use crate::ledger::{Ledger, fetch};
use anyhow::{Context, Result};
use std::path::Path;

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
