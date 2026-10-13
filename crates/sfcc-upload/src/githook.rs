use anyhow::{Context, Result, bail};
use sfcc_core::config::Config;
use std::path::{Path, PathBuf};
use std::process::Command;

const HOOK_NAME: &str = "post-checkout";
const HOOK_MARKER: &str = "sfcc-upload";
/// What the hook said before the uploader was renamed, so it is still ours to replace.
const OLD_HOOK_MARKER: &str = "prost";
const HOOK_BODY: &str = r#"#!/bin/sh
# sfcc-upload: upload what the branch switch changed
[ "$3" = "1" ] || exit 0
command -v sfcc-upload >/dev/null 2>&1 || exit 0
sfcc-upload push --no-wait || true
"#;

pub fn install(config: &Config, force: bool) -> Result<PathBuf> {
    let hooks = hooks_dir(&config.cartridges_dir)
        .or_else(|| hooks_dir(&config.dw_json))
        .context("no git repository found around the cartridges")?;
    // Git would never run a hook written in .git/hooks, and the folder it runs is usually the
    // repository's own, versioned, which is not this tool's to write in.
    if let Some(elsewhere) = hooks_path(&config.cartridges_dir) {
        bail!(
            "git runs the hooks in {elsewhere} (core.hooksPath), not in {} - a hook written \
             there would never run. The watcher already uploads what a branch switch changes; \
             to push on checkout anyway, call `sfcc-upload push --no-wait` from \
             {elsewhere}/{HOOK_NAME}",
            hooks.display()
        );
    }

    std::fs::create_dir_all(&hooks)
        .with_context(|| format!("cannot create {}", hooks.display()))?;

    let hook = hooks.join(HOOK_NAME);
    if hook.exists() && !force {
        let existing = std::fs::read_to_string(&hook).unwrap_or_default();
        if !existing.contains(HOOK_MARKER) && !existing.contains(OLD_HOOK_MARKER) {
            bail!(
                "{} already exists - re-run with --force to replace it",
                hook.display()
            );
        }
    }

    std::fs::write(&hook, HOOK_BODY).with_context(|| format!("cannot write {}", hook.display()))?;
    make_executable(&hook);
    Ok(hook)
}

fn hooks_path(inside: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(inside)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !path.is_empty()).then_some(path)
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
        let home = std::env::temp_dir().join("sfcc-upload-test-hooks-plain");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".git")).unwrap();
        std::fs::create_dir_all(home.join("source").join("cartridges")).unwrap();

        let found = hooks_dir(&home.join("source").join("cartridges"));
        assert_eq!(found, Some(home.join(".git").join("hooks")));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_hooks_path_set_in_the_repository_is_found() {
        let home = std::env::temp_dir().join("sfcc-upload-test-hooks-path");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("cartridges")).unwrap();
        let git = |arguments: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&home)
                .args(arguments)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        assert_eq!(hooks_path(&home.join("cartridges")), None);

        git(&["config", "core.hooksPath", ".githooks"]);
        assert_eq!(
            hooks_path(&home.join("cartridges")).as_deref(),
            Some(".githooks")
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn follows_the_gitdir_pointer_of_a_worktree() {
        let home = std::env::temp_dir().join("sfcc-upload-test-hooks-worktree");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(".git"), "gitdir: ../real/.git/worktrees/one\n").unwrap();

        let found = hooks_dir(&home).expect("the pointer should be followed");
        assert!(found.ends_with("worktrees/one/hooks") || found.ends_with("worktrees\\one\\hooks"));

        let _ = std::fs::remove_dir_all(&home);
    }
}
