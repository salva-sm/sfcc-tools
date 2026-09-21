use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const HOOK_NAME: &str = "post-checkout";
const HOOK_MARKER: &str = "prost";
const HOOK_BODY: &str = r#"#!/bin/sh
# prost: upload what the branch switch changed
[ "$3" = "1" ] || exit 0
command -v prost >/dev/null 2>&1 || exit 0
prost push || true
"#;

pub fn install(config: &Config, force: bool) -> Result<PathBuf> {
    let hooks = hooks_dir(&config.cartridges_dir)
        .or_else(|| hooks_dir(&config.dw_json))
        .context("no git repository found around the cartridges")?;

    std::fs::create_dir_all(&hooks)
        .with_context(|| format!("cannot create {}", hooks.display()))?;

    let hook = hooks.join(HOOK_NAME);
    if hook.exists() && !force {
        let existing = std::fs::read_to_string(&hook).unwrap_or_default();
        if !existing.contains(HOOK_MARKER) {
            bail!("{} already exists - re-run with --force to replace it", hook.display());
        }
    }

    std::fs::write(&hook, HOOK_BODY).with_context(|| format!("cannot write {}", hook.display()))?;
    make_executable(&hook);
    Ok(hook)
}

fn hooks_dir(start: &Path) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let git = directory.join(".git");
        if git.is_dir() {
            return Some(git.join("hooks"));
        }
        if git.is_file() {
            let pointer = std::fs::read_to_string(&git).ok()?;
            let target = pointer.trim().strip_prefix("gitdir:")?.trim();
            return Some(directory.join(target).join("hooks"));
        }
    }
    None
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(windows)]
fn make_executable(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_hooks_directory_of_a_plain_repository() {
        let home = std::env::temp_dir().join("prost-test-hooks-plain");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".git")).unwrap();
        std::fs::create_dir_all(home.join("source").join("cartridges")).unwrap();

        let found = hooks_dir(&home.join("source").join("cartridges"));
        assert_eq!(found, Some(home.join(".git").join("hooks")));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn follows_the_gitdir_pointer_of_a_worktree() {
        let home = std::env::temp_dir().join("prost-test-hooks-worktree");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".git"), "gitdir: ../real/.git/worktrees/one\n").unwrap();

        let found = hooks_dir(&home).expect("the pointer should be followed");
        assert!(found.ends_with("worktrees/one/hooks") || found.ends_with("worktrees\\one\\hooks"));

        let _ = std::fs::remove_dir_all(&home);
    }
}
