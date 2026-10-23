//! Files of a git ref, read without checking it out: a metadata mirror kept on a branch of its
//! own (`metadata_ref`) has no working copy in the workspace the editor opened.

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The `metadata_ref` init option, such as `origin/data/metadata`.
pub fn from_settings(settings: &Value) -> Option<String> {
    settings
        .get("metadata_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
        .map(str::to_string)
}

/// Path and text of every file of `reference` whose name `wanted` accepts, from the first root
/// that knows the ref. Nothing when git is missing, no root is a repository or the ref does not
/// exist yet: the ref is a fallback, never a reason to fail.
pub fn files(
    roots: &[PathBuf],
    reference: &str,
    wanted: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let Some(root) = roots.iter().find(|root| knows(root, reference)) else {
        return Vec::new();
    };
    let Some(listing) = git(root, &["ls-tree", "-r", "--name-only", "-z", reference]) else {
        return Vec::new();
    };
    listing
        .split('\0')
        .filter(|path| !path.is_empty() && wanted(file_name(path)))
        .filter_map(|path| {
            let text = git(root, &["show", &format!("{reference}:{path}")])?;
            Some((path.to_string(), text))
        })
        .collect()
}

/// The last segment of a path as git lists it, always with forward slashes.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The folder holding a file, as git lists it: a site archive's folder is the site id.
pub fn parent_name(path: &str) -> Option<&str> {
    let mut segments = path.rsplit('/');
    segments.next()?;
    segments.next()
}

fn knows(root: &Path, reference: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
    )
    .is_some()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
fn run(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A repository whose `mirror` branch holds `files`, and whose checkout holds none of them.
#[cfg(test)]
pub(crate) fn repository_with_mirror(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    run(&root, &["init", "-q", "-b", "main"]);
    run(&root, &["commit", "-q", "--allow-empty", "-m", "main"]);
    run(&root, &["checkout", "-q", "--orphan", "mirror"]);
    for (path, text) in files {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "mirror"]);
    run(&root, &["checkout", "-q", "main"]);
    run(&root, &["clean", "-q", "-fdx"]);
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_files_of_a_ref_that_is_not_checked_out() {
        let root = repository_with_mirror(
            "isml-lsp-gitref-read",
            &[
                ("metadata/baseline/sites/web/site.xml", "<site/>"),
                ("metadata/README.md", "x"),
            ],
        );
        assert!(!root.join("metadata").exists());
        let found = files(&[root], "mirror", |name| name == "site.xml");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "metadata/baseline/sites/web/site.xml");
        assert_eq!(found[0].1, "<site/>");
    }

    #[test]
    fn finds_nothing_for_a_ref_that_does_not_exist() {
        let root = repository_with_mirror("isml-lsp-gitref-missing", &[("a.xml", "<a/>")]);
        assert!(files(&[root], "origin/nowhere", |_| true).is_empty());
    }

    #[test]
    fn names_the_folder_of_a_listed_file() {
        assert_eq!(
            parent_name("metadata/baseline/sites/web/site.xml"),
            Some("web")
        );
        assert_eq!(parent_name("site.xml"), None);
        assert_eq!(file_name("a/b/site.xml"), "site.xml");
    }

    #[test]
    fn reads_the_ref_from_the_settings() {
        let settings = serde_json::json!({ "metadata_ref": " origin/data/metadata " });
        assert_eq!(
            from_settings(&settings).as_deref(),
            Some("origin/data/metadata")
        );
        assert_eq!(
            from_settings(&serde_json::json!({ "metadata_ref": "" })),
            None
        );
        assert_eq!(from_settings(&Value::Null), None);
    }
}
