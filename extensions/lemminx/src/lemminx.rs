use std::fs;

use zed_extension_api::{
    self as zed, serde_json::Value, settings::LspSettings, Architecture, GithubReleaseOptions,
    LanguageServerId, Os, Result,
};

const SERVER_ID: &str = "lemminx";
/// Red Hat publishes LemMinX as native binaries with each release of its VS Code client.
const SERVER_REPOSITORY: &str = "redhat-developer/vscode-xml";

struct LemminxExtension {
    cached_binary_path: Option<String>,
}

impl LemminxExtension {
    /// A configured binary, then one on `PATH`, win over the download.
    fn local_binary(worktree: &zed::Worktree) -> (Option<String>, Vec<String>) {
        let configured = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let (configured_path, args) = match configured {
            Some(binary) => (binary.path, binary.arguments.unwrap_or_default()),
            None => (None, Vec::new()),
        };

        let path = configured_path
            .or_else(|| worktree.which(SERVER_ID))
            .or_else(|| worktree.which(&format!("{SERVER_ID}.exe")));

        (path, args)
    }

    fn download_binary(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let release = zed::latest_github_release(
            SERVER_REPOSITORY,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, architecture) = zed::current_platform();
        let stem = binary_stem(platform, architecture)?;
        let asset_name = format!("{stem}.zip");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset named {asset_name} in release {}", release.version))?;

        let version_dir = format!("{SERVER_ID}-{}", release.version);
        let binary_path = match platform {
            Os::Windows => format!("{version_dir}/{stem}.exe"),
            Os::Mac | Os::Linux => format!("{version_dir}/{stem}"),
        };

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::Zip,
            )
            .map_err(|error| format!("failed to download {asset_name}: {error}"))?;
            zed::make_file_executable(&binary_path)?;

            remove_other_versions(&version_dir);
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }

    /// The `lsp.lemminx.settings` of the worktree, with project-relative paths made absolute.
    fn settings(worktree: &zed::Worktree) -> Value {
        let mut settings = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.settings)
            .unwrap_or_else(|| Value::Object(Default::default()));
        resolve_relative_paths(&mut settings, &worktree.root_path());
        settings
    }
}

/// The name of the release asset, without `.zip`, and of the binary inside it.
fn binary_stem(platform: Os, architecture: Architecture) -> Result<&'static str> {
    match (platform, architecture) {
        (Os::Windows, Architecture::X8664) => Ok("lemminx-win32"),
        (Os::Mac, Architecture::Aarch64) => Ok("lemminx-osx-aarch_64"),
        (Os::Mac, Architecture::X8664) => Ok("lemminx-osx-x86_64"),
        (Os::Linux, Architecture::Aarch64) => Ok("lemminx-linux-aarch_64"),
        (Os::Linux, Architecture::X8664) => Ok("lemminx-linux-x86_64"),
        _ => Err(
            "LemMinX publishes no native binary for this platform: set lsp.lemminx.binary.path"
                .into(),
        ),
    }
}

fn remove_other_versions(current: &str) {
    let Ok(entries) = fs::read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_str() != Some(current) {
            fs::remove_dir_all(entry.path()).ok();
        }
    }
}

/// `/x`, `\x`, `C:x` or a URI: anything LemMinX can open without knowing the project.
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("://")
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

fn absolute(path: &str, root: &str) -> String {
    if is_absolute(path) {
        return path.to_string();
    }
    let root = root.trim_end_matches(['/', '\\']);
    let relative = path.trim_start_matches("./");
    format!("{root}/{relative}")
}

/// LemMinX resolves catalogs and associated schemas against its own working directory, not
/// the project, so the VS Code client rewrites them first. This does the same, so a project's
/// settings can say `schemas/catalog.xml` instead of a path that only exists on one machine.
fn resolve_relative_paths(settings: &mut Value, root: &str) {
    let Some(xml) = settings.get_mut("xml") else {
        return;
    };
    if let Some(catalogs) = xml.get_mut("catalogs").and_then(Value::as_array_mut) {
        for catalog in catalogs.iter_mut() {
            if let Some(path) = catalog.as_str() {
                *catalog = Value::String(absolute(path, root));
            }
        }
    }
    if let Some(associations) = xml
        .get_mut("fileAssociations")
        .and_then(Value::as_array_mut)
    {
        for association in associations.iter_mut() {
            if let Some(system_id) = association.get_mut("systemId") {
                if let Some(path) = system_id.as_str() {
                    *system_id = Value::String(absolute(path, root));
                }
            }
        }
    }
}

/// LemMinX reads its settings at start-up from `initializationOptions.settings`.
fn initialization_options(user: Option<Value>, settings: Value) -> Value {
    let mut options = match user {
        Some(Value::Object(map)) => Value::Object(map),
        _ => Value::Object(Default::default()),
    };
    if let Value::Object(map) = &mut options {
        map.entry("settings").or_insert(settings);
    }
    options
}

impl zed::Extension for LemminxExtension {
    fn new() -> Self {
        LemminxExtension {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let (local, args) = Self::local_binary(worktree);
        let command = match local {
            Some(path) => path,
            None => self.download_binary(language_server_id)?,
        };

        Ok(zed::Command {
            command,
            args,
            env: Vec::new(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        let user = LspSettings::for_worktree(SERVER_ID, worktree)
            .ok()
            .and_then(|settings| settings.initialization_options);
        Ok(Some(initialization_options(user, Self::settings(worktree))))
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        Ok(Some(Self::settings(worktree)))
    }
}

zed::register_extension!(LemminxExtension);

#[cfg(test)]
mod tests {
    use super::*;
    use zed_extension_api::serde_json::json;

    #[test]
    fn makes_project_paths_absolute_and_leaves_the_rest() {
        let mut settings = json!({
            "xml": {
                "catalogs": ["schemas/catalog.xml", "./more.xml", "/etc/xml/catalog", "C:\\xml\\c.xml"],
                "fileAssociations": [
                    { "pattern": "**/*.xml", "systemId": "schemas/a.xsd" },
                    { "pattern": "**/*.svg", "systemId": "https://example.com/svg.xsd" }
                ],
                "validation": { "enabled": true }
            }
        });
        resolve_relative_paths(&mut settings, "/home/dev/project/");
        assert_eq!(
            settings["xml"]["catalogs"],
            json!([
                "/home/dev/project/schemas/catalog.xml",
                "/home/dev/project/more.xml",
                "/etc/xml/catalog",
                "C:\\xml\\c.xml"
            ])
        );
        assert_eq!(
            settings["xml"]["fileAssociations"][0]["systemId"],
            "/home/dev/project/schemas/a.xsd"
        );
        assert_eq!(
            settings["xml"]["fileAssociations"][1]["systemId"],
            "https://example.com/svg.xsd"
        );
        assert_eq!(settings["xml"]["validation"], json!({ "enabled": true }));
    }

    #[test]
    fn joins_a_windows_root() {
        assert_eq!(
            absolute("schemas/catalog.xml", "C:\\dev\\project"),
            "C:\\dev\\project/schemas/catalog.xml"
        );
    }

    #[test]
    fn leaves_settings_without_xml_alone() {
        let mut settings = json!({ "other": ["a.xml"] });
        resolve_relative_paths(&mut settings, "/root");
        assert_eq!(settings, json!({ "other": ["a.xml"] }));
    }

    #[test]
    fn passes_the_settings_in_the_initialization_options() {
        let settings = json!({ "xml": { "catalogs": ["/c.xml"] } });
        assert_eq!(
            initialization_options(None, settings.clone()),
            json!({ "settings": { "xml": { "catalogs": ["/c.xml"] } } })
        );
        assert_eq!(
            initialization_options(Some(json!({ "extendedClientCapabilities": {} })), settings),
            json!({ "extendedClientCapabilities": {}, "settings": { "xml": { "catalogs": ["/c.xml"] } } })
        );
    }

    #[test]
    fn keeps_settings_the_user_put_in_the_initialization_options() {
        let user = json!({ "settings": { "xml": { "catalogs": ["/mine.xml"] } } });
        assert_eq!(initialization_options(Some(user.clone()), json!({})), user);
    }

    #[test]
    fn names_an_asset_for_each_published_platform() {
        assert_eq!(
            binary_stem(Os::Windows, Architecture::X8664).unwrap(),
            "lemminx-win32"
        );
        assert_eq!(
            binary_stem(Os::Mac, Architecture::Aarch64).unwrap(),
            "lemminx-osx-aarch_64"
        );
        assert_eq!(
            binary_stem(Os::Linux, Architecture::X8664).unwrap(),
            "lemminx-linux-x86_64"
        );
        assert!(binary_stem(Os::Windows, Architecture::Aarch64).is_err());
    }
}
