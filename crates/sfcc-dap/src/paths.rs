//! Turning an editor's file into one the instance knows, and back.
//!
//! The editor deals in absolute local paths; the instance deals in paths
//! relative to the cartridges directory. Getting this wrong is the usual
//! reason a breakpoint never binds, so it is kept in one place.

use std::path::{Path, PathBuf};

/// The two views of the same tree.
pub struct Paths {
    cartridges: PathBuf,
}

impl Paths {
    /// Anchored on the directory that holds the cartridges.
    pub fn new(cartridges: impl Into<PathBuf>) -> Paths {
        Paths {
            cartridges: cartridges.into(),
        }
    }

    /// What the instance calls a local file: a leading slash, then the
    /// cartridge and the path within it, with forward slashes.
    pub fn to_script(&self, local: &Path) -> Option<String> {
        let relative = local.strip_prefix(&self.cartridges).ok()?;
        let joined = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        (!joined.is_empty()).then(|| format!("/{joined}"))
    }

    /// The local file behind a script path, whether or not it exists.
    pub fn to_local(&self, script: &str) -> PathBuf {
        let relative = script.trim_start_matches('/');
        self.cartridges
            .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
    }

    /// The cartridge a script path belongs to.
    pub fn cartridge_of(script: &str) -> Option<&str> {
        script
            .trim_start_matches('/')
            .split('/')
            .next()
            .filter(|name| !name.is_empty())
    }

    /// Every other cartridge holding the same path inside `cartridge/`.
    ///
    /// The usual reason a breakpoint never binds is that the copy being
    /// edited is not the copy being loaded, and naming the others is enough
    /// to see it — which of them wins needs the cartridge path, which the
    /// debugger API does not carry.
    pub fn also_in(&self, script: &str) -> Vec<String> {
        let Some(cartridge) = Self::cartridge_of(script) else {
            return Vec::new();
        };
        let within = script
            .trim_start_matches('/')
            .strip_prefix(cartridge)
            .unwrap_or_default()
            .trim_start_matches('/');
        if within.is_empty() {
            return Vec::new();
        }

        let Ok(entries) = std::fs::read_dir(&self.cartridges) else {
            return Vec::new();
        };
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != cartridge)
            .filter(|name| {
                self.cartridges
                    .join(name)
                    .join(within.replace('/', std::path::MAIN_SEPARATOR_STR))
                    .is_file()
            })
            .collect();
        found.sort();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::new(PathBuf::from("C:/repo/source/cartridges"))
    }

    #[test]
    fn names_a_local_file_the_way_the_instance_does() {
        let local =
            PathBuf::from("C:/repo/source/cartridges/app_brand/cartridge/controllers/Account.js");
        assert_eq!(
            paths().to_script(&local).as_deref(),
            Some("/app_brand/cartridge/controllers/Account.js")
        );
    }

    #[test]
    fn refuses_a_file_outside_the_cartridges() {
        assert!(paths().to_script(Path::new("C:/elsewhere/x.js")).is_none());
    }

    #[test]
    fn finds_the_local_file_behind_a_script_path() {
        let local = paths().to_local("/app_brand/cartridge/controllers/Account.js");
        assert!(
            local.ends_with(
                "app_brand/cartridge/controllers/Account.js"
                    .replace('/', std::path::MAIN_SEPARATOR_STR)
            )
        );
        assert!(local.starts_with("C:/repo/source/cartridges"));
    }

    #[test]
    fn reads_the_cartridge_off_a_script_path() {
        assert_eq!(
            Paths::cartridge_of("/app_brand/cartridge/controllers/Account.js"),
            Some("app_brand")
        );
        assert_eq!(Paths::cartridge_of("/"), None);
    }

    #[test]
    fn lists_the_other_cartridges_holding_the_same_file() {
        let root = std::env::temp_dir().join("sfcc-dap-paths");
        let _ = std::fs::remove_dir_all(&root);
        for cartridge in ["app_brand", "app_storefront_base", "int_payment"] {
            let directory = root.join(cartridge).join("cartridge").join("controllers");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("Account.js"), "//").unwrap();
        }
        std::fs::create_dir_all(root.join("int_search")).unwrap();

        let found = Paths::new(&root).also_in("/app_brand/cartridge/controllers/Account.js");
        assert_eq!(found, ["app_storefront_base", "int_payment"]);
    }
}
