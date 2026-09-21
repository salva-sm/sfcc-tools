use std::fs;

use zed_extension_api::{
    self as zed, settings::LspSettings, Architecture, GithubReleaseOptions, LanguageServerId, Os,
    Result,
};

const SERVER_BINARY: &str = "isml-lsp";
const SERVER_REPOSITORY: &str = "salva-sm/sfcc-zed-isml";

struct IsmlExtension {
    cached_binary_path: Option<String>,
}

impl IsmlExtension {
    /// A configured or locally built server always wins: on the machine the
    /// extension is developed on, `isml-lsp` is on `PATH` and rebuilt often.
    fn local_binary(worktree: &zed::Worktree) -> (Option<String>, Vec<String>) {
        let configured = LspSettings::for_worktree(SERVER_BINARY, worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let (configured_path, args) = match configured {
            Some(binary) => (binary.path, binary.arguments.unwrap_or_default()),
            None => (None, Vec::new()),
        };

        let path = configured_path
            .or_else(|| worktree.which(SERVER_BINARY))
            .or_else(|| worktree.which(&format!("{SERVER_BINARY}.exe")));

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
        let asset_name = format!(
            "{SERVER_BINARY}-{arch}-{os}.{extension}",
            arch = match architecture {
                Architecture::Aarch64 => "aarch64",
                Architecture::X8664 => "x86_64",
                Architecture::X86 => return Err("32-bit x86 is not supported".into()),
            },
            os = match platform {
                Os::Mac => "macos",
                Os::Linux => "linux",
                Os::Windows => "windows",
            },
            extension = match platform {
                Os::Windows => "zip",
                Os::Mac | Os::Linux => "tar.gz",
            },
        );
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("no asset named {asset_name} in release {}", release.version))?;

        let version_dir = format!("{SERVER_BINARY}-{}", release.version);
        let binary_path = match platform {
            Os::Windows => format!("{version_dir}/{SERVER_BINARY}.exe"),
            Os::Mac | Os::Linux => format!("{version_dir}/{SERVER_BINARY}"),
        };

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(
                &asset.download_url,
                &version_dir,
                match platform {
                    Os::Windows => zed::DownloadedFileType::Zip,
                    Os::Mac | Os::Linux => zed::DownloadedFileType::GzipTar,
                },
            )
            .map_err(|error| format!("failed to download {asset_name}: {error}"))?;
            zed::make_file_executable(&binary_path)?;

            remove_other_versions(&version_dir);
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
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

impl zed::Extension for IsmlExtension {
    fn new() -> Self {
        IsmlExtension {
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
}

zed::register_extension!(IsmlExtension);
