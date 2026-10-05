//! `self-update`, and the line that says a newer release is out. Every binary comes from one
//! tagged release, and release.yml bakes that tag in: a build from source has none, so it is
//! never told, and never replaced unless asked.

use crate::logging;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sfcc_core::state;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPOSITORY: &str = "https://github.com/salva-sm/sfcc-tools";
/// Every tool a release ships. Only those already next to this one are replaced.
const TOOLS: [&str; 5] = [
    "sfcc-upload",
    "log-diff",
    "sfcc-tui",
    "isml-lsp",
    "sfcc-dap",
];
/// On Windows these ship as a bare .exe, the rest zipped; keep in sync with release.yml.
const BARE_ON_WINDOWS: [&str; 3] = ["sfcc-upload", "log-diff", "sfcc-tui"];
/// GitHub is asked at most this often; in between, the last answer is what is said.
const CHECK_EVERY_SECONDS: i64 = 24 * 60 * 60;
/// A run waits no longer than this for the answer before going on without it.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The release this binary was built for, when release.yml built it.
pub fn release() -> Option<&'static str> {
    option_env!("SFCC_TOOLS_RELEASE").filter(|tag| !tag.is_empty())
}

#[derive(Serialize, Deserialize)]
struct Checked {
    at: i64,
    latest: String,
}

fn checked_path() -> PathBuf {
    state::uploader_dir().join("release.json")
}

/// One line on stderr while a newer release is out.
pub async fn notice() {
    let Some(current) = release() else {
        return;
    };
    clear_leftovers();
    let path = checked_path();
    let latest = match state::read::<Checked>(&path) {
        Some(checked) if state::now_seconds() - checked.at < CHECK_EVERY_SECONDS => checked.latest,
        last => {
            // Offline, the last answer stands - and is not asked for again until tomorrow.
            let latest = latest_release(CHECK_TIMEOUT)
                .await
                .ok()
                .or(last.map(|checked| checked.latest))
                .unwrap_or_else(|| current.to_string());
            remember(&latest);
            latest
        }
    };
    if is_newer(&latest, current) {
        logging::notice(format!(
            "{latest} is out, this is {current} - `sfcc-upload self-update` installs it"
        ));
    }
}

fn remember(latest: &str) {
    let checked = Checked {
        at: state::now_seconds(),
        latest: latest.to_string(),
    };
    state::write(&checked_path(), &checked);
}

/// Replace every tool next to this one with the latest release's.
pub async fn run(check: bool, force: bool) -> Result<()> {
    clear_leftovers();
    let latest = latest_release(Duration::from_secs(30)).await?;
    remember(&latest);
    match release() {
        Some(current) if !force && !is_newer(&latest, current) => {
            logging::ok(format!("{current} is the latest release"));
            return Ok(());
        }
        Some(current) if check => {
            logging::info(format!("{latest} is out, this is {current}"));
            return Ok(());
        }
        None if check => {
            logging::info(format!(
                "{latest} is the latest release; this one was built from source"
            ));
            return Ok(());
        }
        None if !force => bail!(
            "this sfcc-upload was built from source, not taken from a release - `git pull` and \
             `cargo install` it again, or `--force` to replace it with {latest}'s binaries"
        ),
        _ => {}
    }

    let exe = std::env::current_exe().context("cannot tell where sfcc-upload is installed")?;
    let dir = exe.parent().context("sfcc-upload is not in a folder")?;
    let platform = platform()?;
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("cannot build the download client")?;

    for tool in TOOLS {
        let target = dir.join(binary_name(tool));
        if !target.is_file() {
            continue;
        }
        let asset = asset_name(tool, &platform);
        let url = format!("{REPOSITORY}/releases/download/{latest}/{asset}");
        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("cannot download {asset}"))?;
        if !response.status().is_success() {
            bail!("{latest} has no {asset} (HTTP {})", response.status());
        }
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("cannot download {asset}"))?;
        let binary = unpack(&asset, bytes.to_vec(), &binary_name(tool), dir)?;
        replace(&target, &binary)
            .with_context(|| format!("cannot replace {}", target.display()))?;
        logging::ok(format!("{tool} {latest}"));
    }

    logging::info(
        "what is already running keeps the old version until it restarts: `sfcc-upload stop \
         --all` and `start` the watchers again, rerun the editor task, restart Zed",
    );
    Ok(())
}

/// The tag `releases/latest` redirects to. Not the API: its unauthenticated limit is per
/// address, and a whole office can share one.
async fn latest_release(timeout: Duration) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("cannot build the release client")?;
    let response = client
        .get(format!("{REPOSITORY}/releases/latest"))
        .send()
        .await
        .context("cannot reach GitHub")?;
    response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|location| location.to_str().ok())
        .and_then(tag_of)
        .with_context(|| {
            format!(
                "GitHub did not say which release is the latest (HTTP {})",
                response.status()
            )
        })
}

fn tag_of(location: &str) -> Option<String> {
    let (_, tag) = location.rsplit_once("/tag/")?;
    version(tag).map(|_| tag.to_string())
}

/// `v0.18.0` as [0, 18, 0], so releases compare by number.
fn version(tag: &str) -> Option<Vec<u64>> {
    tag.strip_prefix('v')?
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

fn is_newer(latest: &str, current: &str) -> bool {
    match (version(latest), version(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// As release.yml names the assets: `x86_64-windows`, `aarch64-macos`.
fn platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "windows" | "macos" | "linux" => std::env::consts::OS,
        other => bail!("no release is built for {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" if os != "windows" => "aarch64",
        other => bail!("no release is built for {other} {os}"),
    };
    Ok(format!("{arch}-{os}"))
}

fn binary_name(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

fn asset_name(tool: &str, platform: &str) -> String {
    let extension = match platform.ends_with("windows") {
        true if BARE_ON_WINDOWS.contains(&tool) => "exe",
        true => "zip",
        false => "tar.gz",
    };
    format!("{tool}-{platform}.{extension}")
}

/// The binary out of what was downloaded. A tarball goes through `tar`, which every macOS
/// and Linux has.
fn unpack(asset: &str, bytes: Vec<u8>, binary: &str, dir: &Path) -> Result<Vec<u8>> {
    if asset.ends_with(".exe") {
        return Ok(bytes);
    }
    if asset.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .with_context(|| format!("{asset} is not a zip"))?;
        let mut file = archive
            .by_name(binary)
            .with_context(|| format!("{asset} has no {binary}"))?;
        let mut contents = Vec::new();
        file.read_to_end(&mut contents)
            .with_context(|| format!("cannot unpack {asset}"))?;
        return Ok(contents);
    }

    let staging = dir.join(format!(".{binary}.update"));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("cannot write in {}", dir.display()))?;
    let unpacked = (|| {
        let archive = staging.join(asset);
        std::fs::write(&archive, &bytes)?;
        let status = std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .status()
            .context("cannot run tar")?;
        if !status.success() {
            bail!("tar could not unpack {asset}");
        }
        std::fs::read(staging.join(binary)).with_context(|| format!("{asset} has no {binary}"))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    unpacked
}

/// Windows will not overwrite a running .exe, but lets it be renamed: the old one steps
/// aside as `<name>.<time>.old` and goes at a later run. Elsewhere a rename replaces it.
fn replace(target: &Path, binary: &[u8]) -> Result<()> {
    let fresh = suffixed(target, ".new");
    std::fs::write(&fresh, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
    }
    if cfg!(windows) {
        let aside = suffixed(target, &format!(".{}.old", state::now_seconds()));
        if let Err(error) = std::fs::rename(target, &aside) {
            let _ = std::fs::remove_file(&fresh);
            return Err(error.into());
        }
    }
    std::fs::rename(&fresh, target)?;
    Ok(())
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// What an earlier update set aside, once nothing runs it any more.
fn clear_leftovers() {
    if !cfg!(windows) {
        return;
    }
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = TOOLS
            .iter()
            .any(|tool| name.starts_with(&format!("{}.", binary_name(tool))));
        if ours && name.ends_with(".old") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
