//! Registers the SFCC script debugger with Zed.
//!
//! Zed only speaks to debuggers it knows about, and B2C Commerce is not one
//! of them. This tells it how to start `sfcc-dap`, which does the work: the
//! Debug Adapter Protocol on the editor's side, the instance's own
//! `dw/debugger/v2_0` API on the other. Nothing else is needed — no Node,
//! and no CLI.

use std::fs;

use serde::Deserialize;
use zed_extension_api::{
    self as zed, Architecture, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario,
    DebugTaskDefinition, GithubReleaseOptions, Os, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, Worktree,
};

const ADAPTER_BINARY: &str = "sfcc-dap";
const ADAPTER_REPOSITORY: &str = "salva-sm/sfcc-tools";
const CARTRIDGE_CANDIDATES: [&str; 2] = ["source/cartridges", "cartridges"];

/// What a `.zed/debug.json` entry may say.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Settings {
    /// An explicit path to the adapter, when it is not on `PATH`.
    binary: Option<String>,
    /// The cartridges directory, absolute or relative to the worktree.
    cartridge_path: Option<String>,
    /// A `dw.json` other than the one beside the cartridges.
    config: Option<String>,
    /// The client id reported to the instance, when two people share one.
    client_id: Option<String>,
}

struct B2cDebugExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for B2cDebugExtension {
    fn new() -> Self {
        B2cDebugExtension {
            cached_binary_path: None,
        }
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        definition: DebugTaskDefinition,
        user_installed_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary> {
        let settings: Settings = serde_json::from_str(&definition.config)
            .map_err(|error| format!("cannot read the debug configuration: {error}"))?;

        // A configured or locally built adapter always wins; the download is
        // only for a machine with no Rust toolchain on it.
        let command = match settings
            .binary
            .clone()
            .or(user_installed_path)
            .or_else(|| worktree.which(ADAPTER_BINARY))
            .or_else(|| worktree.which(&format!("{ADAPTER_BINARY}.exe")))
        {
            Some(path) => path,
            None => self.download_binary()?,
        };

        let root = worktree.root_path();
        let cartridges = cartridge_path(&settings, worktree)?;
        let config = match &settings.config {
            Some(configured) => absolute(configured, &root),
            None => format!("{}/dw.json", parent_of(&cartridges)),
        };

        let mut arguments = vec![
            "--cartridge-path".to_string(),
            cartridges,
            "--config".to_string(),
            config,
        ];
        if let Some(client_id) = settings.client_id {
            arguments.push("--client-id".to_string());
            arguments.push(client_id);
        }

        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments,
            envs: worktree.shell_env(),
            cwd: Some(root),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: definition.config,
                request: StartDebuggingRequestArgumentsRequest::Attach,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        _config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest> {
        Ok(StartDebuggingRequestArgumentsRequest::Attach)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario> {
        if let DebugRequest::Launch(_) = config.request {
            return Err(
                "the B2C script debugger attaches to a running instance; it cannot launch one"
                    .to_string(),
            );
        }

        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: "{}".to_string(),
            tcp_connection: None,
        })
    }
}

impl B2cDebugExtension {
    /// Fetch the adapter from the repository's latest release.
    fn download_binary(&mut self) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        let release = zed::latest_github_release(
            ADAPTER_REPOSITORY,
            GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (platform, architecture) = zed::current_platform();
        let asset_name = format!(
            "{ADAPTER_BINARY}-{arch}-{os}.{extension}",
            arch = match architecture {
                Architecture::Aarch64 => "aarch64",
                Architecture::X8664 => "x86_64",
                _ => return Err("only x86_64 and aarch64 are published".to_string()),
            },
            os = match platform {
                Os::Windows => "windows",
                Os::Mac => "macos",
                Os::Linux => "linux",
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

        let version_dir = format!("{ADAPTER_BINARY}-{}", release.version);
        let binary_path = match platform {
            Os::Windows => format!("{version_dir}/{ADAPTER_BINARY}.exe"),
            Os::Mac | Os::Linux => format!("{version_dir}/{ADAPTER_BINARY}"),
        };

        if !fs::metadata(&binary_path).is_ok_and(|stat| stat.is_file()) {
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

fn cartridge_path(settings: &Settings, worktree: &Worktree) -> Result<String> {
    let root = worktree.root_path();

    if let Some(configured) = &settings.cartridge_path {
        return Ok(absolute(configured, &root));
    }

    for candidate in CARTRIDGE_CANDIDATES {
        let marker = format!("{candidate}/modules/server/route.js");
        if worktree.read_text_file(&marker).is_ok() {
            return Ok(absolute(candidate, &root));
        }
    }

    Err(format!(
        "no cartridges directory found under {root} - set \"cartridge_path\" in the debug configuration"
    ))
}

fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => ".".to_string(),
    }
}

fn absolute(path: &str, root: &str) -> String {
    let looks_absolute = path.starts_with('/') || path.chars().nth(1) == Some(':');
    match looks_absolute {
        true => path.to_string(),
        false => format!("{root}/{path}"),
    }
}

zed::register_extension!(B2cDebugExtension);
