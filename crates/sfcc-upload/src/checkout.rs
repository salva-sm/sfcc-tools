//! A code version belongs to the checkout that uploads to it; another checkout (an agent's
//! worktree, a second clone) gets a code version of its own.

use std::path::Path;
use std::process::Command;

use sfcc_core::config::Config;
use sfcc_core::state;

use crate::manifest::Manifest;

const MAX_VERSION: usize = 50;

pub fn same_folder(left: &Path, right: &Path) -> bool {
    state::is_within(left, right) && state::is_within(right, left)
}

pub fn other(config: &Config, manifest: &Manifest) -> Option<String> {
    let recorded = manifest.cartridges.as_deref()?;
    (!same_folder(Path::new(recorded), &config.cartridges_dir)).then(|| recorded.to_string())
}

pub fn claim(config: &Config, manifest: &mut Manifest) -> bool {
    let mine = config.cartridges_dir.to_string_lossy().into_owned();
    let changed = manifest.cartridges.as_deref() != Some(mine.as_str());
    manifest.cartridges = Some(mine);
    changed
}

pub fn refusal(config: &Config, owner: &str) -> String {
    format!(
        "{} on {} is {owner}'s - uploading {} to it would replace its files",
        config.code_version,
        config.hostname,
        config.cartridges_dir.display()
    )
}

pub fn branch_version(config: &Config) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(&config.cartridges_dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Detached, the branch is `HEAD`, which names nothing.
    (branch != "HEAD")
        .then(|| version_name(&branch))
        .flatten()
        .filter(|name| *name != config.code_version)
}

fn version_name(branch: &str) -> Option<String> {
    let mut name = String::new();
    for character in branch.chars() {
        match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => name.push(character),
            _ if !name.ends_with('-') => name.push('-'),
            _ => {}
        }
    }
    let name: String = name.trim_matches('-').chars().take(MAX_VERSION).collect();
    let name = name.trim_end_matches('-').to_string();
    (!name.is_empty()).then_some(name)
}

pub fn way_out(config: &Config) -> String {
    let name = branch_version(config).unwrap_or_else(|| "<name>".to_string());
    format!("give it a code version of its own: `sfcc-upload push --code-version {name}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_becomes_a_code_version_name() {
        assert_eq!(
            version_name("feature/ABC-123-cart-fix").as_deref(),
            Some("feature-ABC-123-cart-fix")
        );
        assert_eq!(version_name("a//b..c").as_deref(), Some("a-b-c"));
        assert_eq!(version_name("/").as_deref(), None);
        assert_eq!(
            version_name(&"x".repeat(80)).map(|it| it.len()),
            Some(MAX_VERSION)
        );
    }

    #[tokio::test]
    async fn the_checkout_is_recorded_and_another_one_found() {
        let mut config = sfcc_core::testing::MockDav::start().await.config();
        config.cartridges_dir = "/work/shop/cartridges".into();
        let mut manifest = Manifest::default();
        assert_eq!(other(&config, &manifest), None);
        assert!(claim(&config, &mut manifest));
        assert!(!claim(&config, &mut manifest));
        assert_eq!(other(&config, &manifest), None);

        config.cartridges_dir = "/work/shop-agent/cartridges".into();
        assert_eq!(
            other(&config, &manifest).as_deref(),
            Some("/work/shop/cartridges")
        );
    }
}
