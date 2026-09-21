use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT_CODE_VERSION: &str = "version1";

#[derive(Debug, Deserialize)]
struct DwJson {
    hostname: Option<String>,
    username: Option<String>,
    password: Option<String>,
    #[serde(rename = "code-version")]
    code_version: Option<String>,
    #[serde(rename = "codeVersion")]
    code_version_alt: Option<String>,
    cartridge: Option<Vec<String>>,
    #[serde(rename = "cartridgesDir")]
    cartridges_dir: Option<String>,
    #[serde(rename = "client-id")]
    client_id: Option<String>,
    #[serde(rename = "client-secret")]
    client_secret: Option<String>,
    #[serde(rename = "custom-sfcc-ci")]
    sfcc_ci: Option<SfccCi>,
    #[serde(rename = "self-signed")]
    self_signed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SfccCi {
    #[serde(rename = "sfcc-oauth-client-id")]
    client_id: Option<String>,
    #[serde(rename = "sfcc-oauth-client-secret")]
    client_secret: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
/// What kind of instance a hostname belongs to, which decides whether a
/// tool may write to it.
pub enum Instance {
    /// A developer sandbox. The only kind written to without asking.
    Sandbox,
    /// A shared development instance.
    Development,
    /// Staging. Never written to.
    Staging,
    /// Production. Never written to.
    Production,
    /// A host that matches none of the known shapes.
    Unknown,
}

#[derive(Debug, Clone)]
/// How a request to the instance authenticates.
pub enum Credentials {
    /// A Business Manager user, or a WebDAV access key.
    Basic {
        /// The Business Manager user name.
        username: String,
        /// Its password, or the access key.
        password: String,
    },
    /// An Account Manager API client, exchanged for a bearer token.
    OAuth {
        /// The client id.
        client_id: String,
        /// Its secret.
        client_secret: String,
    },
}

#[derive(Debug, Clone)]
/// Everything a tool needs from `dw.json`, resolved.
pub struct Config {
    /// The file this came from.
    pub dw_json: PathBuf,
    /// The instance host, without scheme or trailing slash.
    pub hostname: String,
    /// How to authenticate against it.
    pub credentials: Credentials,
    /// The code version directory being worked on.
    pub code_version: String,
    /// The directory holding the cartridges, absolute.
    pub cartridges_dir: PathBuf,
    /// The cartridges to act on, or `None` for all of them.
    pub cartridge_filter: Option<Vec<String>>,
    /// Whether a self-signed certificate is acceptable.
    pub accept_invalid_certs: bool,
    /// An Account Manager client, for the APIs that need one.
    pub api_client: Option<ApiClient>,
}

#[derive(Debug, Clone)]
/// An Account Manager API client.
pub struct ApiClient {
    /// The client id.
    pub id: String,
    /// Its secret.
    pub secret: String,
}

impl Config {
    /// Read the nearest `dw.json`, walking up from the working directory
    /// unless a path is given.
    pub fn load(
        explicit_path: Option<PathBuf>,
        code_version_override: Option<String>,
    ) -> Result<Config> {
        let dw_json = match explicit_path {
            Some(path) => path
                .canonicalize()
                .with_context(|| format!("dw.json not found at {}", path.display()))?,
            None => discover_dw_json()?,
        };

        let raw = std::fs::read_to_string(&dw_json)
            .with_context(|| format!("cannot read {}", dw_json.display()))?;
        let parsed: DwJson = serde_json::from_str(&raw)
            .with_context(|| format!("cannot parse {} as JSON", dw_json.display()))?;

        let hostname = parsed
            .hostname
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("dw.json has no \"hostname\"")?;

        let credentials = resolve_credentials(&parsed).context(
            "dw.json needs \"username\"/\"password\" or \"client-id\"/\"client-secret\"",
        )?;

        let code_version = code_version_override
            .or(parsed.code_version.clone())
            .or(parsed.code_version_alt.clone())
            .unwrap_or_else(|| DEFAULT_CODE_VERSION.to_string());

        let root = dw_json
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let cartridges_dir = resolve_cartridges_dir(&root, parsed.cartridges_dir.as_deref())?;

        let api_client = resolve_api_client(&parsed);
        let cartridge_filter = parsed
            .cartridge
            .filter(|names| !names.is_empty())
            .map(|names| names.iter().map(|name| leaf_name(name)).collect());

        Ok(Config {
            dw_json,
            hostname: hostname.trim().trim_end_matches('/').to_string(),
            credentials,
            code_version,
            cartridges_dir,
            cartridge_filter,
            accept_invalid_certs: parsed.self_signed.unwrap_or(false),
            api_client,
        })
    }

    /// Where the cartridges of this code version live over WebDAV.
    pub fn webdav_root(&self) -> String {
        format!(
            "https://{}/on/demandware.servlet/webdav/Sites/Cartridges",
            self.hostname
        )
    }

    /// Where the instance writes its logs.
    pub fn logs_url(&self) -> String {
        format!(
            "https://{}/on/demandware.servlet/webdav/Sites/Logs",
            self.hostname
        )
    }

    /// The Data API resource for this code version.
    pub fn code_version_url(&self) -> String {
        format!("{}/{}", self.webdav_root(), self.code_version)
    }

    /// What kind of instance this points at.
    pub fn instance(&self) -> Instance {
        classify_host(&self.hostname)
    }

    /// Refuse to write anywhere that is not a developer sandbox. Staging
    /// and production are refused outright, with no override.
    pub fn ensure_writable(&self, allow_shared: bool) -> Result<()> {
        match self.instance() {
            Instance::Sandbox => Ok(()),
            Instance::Production | Instance::Staging => bail!(
                "refusing to write to {} - this looks like a staging or production \
                 instance, and these tools only write to developer sandboxes",
                self.hostname
            ),
            _ if allow_shared => Ok(()),
            _ => bail!(
                "{} is not a developer sandbox - re-run with --allow-shared-instance if you \
                 really mean to deploy there",
                self.hostname
            ),
        }
    }

    /// A filename-safe name for this host and code version, for the state
    /// a tool keeps per sandbox.
    pub fn identity(&self) -> String {
        let raw = format!("{}__{}", self.hostname, self.code_version);
        raw.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    }
}

/// What kind of instance a hostname is, by its shape alone.
pub fn classify_host(hostname: &str) -> Instance {
    let host = hostname.to_lowercase();
    let words: Vec<&str> = host
        .split('.')
        .next()
        .unwrap_or_default()
        .split(['-', '_'])
        .collect();

    if words
        .iter()
        .any(|word| matches!(*word, "production" | "prod" | "prd"))
    {
        return Instance::Production;
    }
    if words
        .iter()
        .any(|word| matches!(*word, "staging" | "stg" | "stage"))
    {
        return Instance::Staging;
    }
    if words
        .iter()
        .any(|word| matches!(*word, "development" | "dev"))
    {
        return Instance::Development;
    }
    if host.ends_with(".my.commercecloud.salesforce.com")
        || host.ends_with(".dx.commercecloud.salesforce.com")
    {
        return Instance::Sandbox;
    }
    Instance::Unknown
}

fn resolve_api_client(parsed: &DwJson) -> Option<ApiClient> {
    let from_block = parsed
        .sfcc_ci
        .as_ref()
        .and_then(|block| Some((block.client_id.clone()?, block.client_secret.clone()?)));
    let (id, secret) =
        from_block.or_else(|| Some((parsed.client_id.clone()?, parsed.client_secret.clone()?)))?;

    if id.trim().is_empty() || secret.trim().is_empty() {
        return None;
    }
    Some(ApiClient { id, secret })
}

fn resolve_credentials(parsed: &DwJson) -> Option<Credentials> {
    let username = parsed
        .username
        .clone()
        .filter(|value| !value.trim().is_empty());
    let password = parsed
        .password
        .clone()
        .filter(|value| !value.trim().is_empty());
    if let (Some(username), Some(password)) = (username, password) {
        return Some(Credentials::Basic { username, password });
    }

    let client_id = parsed
        .client_id
        .clone()
        .filter(|value| !value.trim().is_empty());
    let client_secret = parsed
        .client_secret
        .clone()
        .filter(|value| !value.trim().is_empty());
    match (client_id, client_secret) {
        (Some(client_id), Some(client_secret)) => Some(Credentials::OAuth {
            client_id,
            client_secret,
        }),
        _ => None,
    }
}

fn resolve_cartridges_dir(root: &Path, configured: Option<&str>) -> Result<PathBuf> {
    if let Some(relative) = configured {
        // The value is relative to dw.json. When the file sits in a
        // subdirectory it is routinely written relative to the repository
        // root instead, which resolves against the parent and nowhere else.
        let from_root = root.join(relative);
        let from_parent = root.parent().map(|parent| parent.join(relative));
        for candidate in [Some(from_root.clone()), from_parent].into_iter().flatten() {
            if candidate.is_dir() {
                return Ok(normalize(&candidate));
            }
        }
        bail!(
            "\"cartridgesDir\": {relative:?} points to {}, which does not exist - the path is \
             relative to dw.json",
            from_root.display()
        );
    }

    let candidates = [
        root.join("cartridges"),
        root.join("source").join("cartridges"),
        root.join("..").join("cartridges"),
        root.join("..").join("source").join("cartridges"),
    ];
    for candidate in candidates {
        if candidate.is_dir() {
            return Ok(normalize(&candidate));
        }
    }
    bail!(
        "no cartridges directory found next to {} - set \"cartridgesDir\" in dw.json",
        root.display()
    )
}

fn discover_dw_json() -> Result<PathBuf> {
    let start = std::env::current_dir().context("cannot read the current directory")?;
    for directory in start.ancestors() {
        for candidate in [
            directory.join("dw.json"),
            directory.join("source").join("dw.json"),
        ] {
            if candidate.is_file() {
                return Ok(normalize(&candidate));
            }
        }
    }
    bail!(
        "no dw.json found from {} upwards - pass --config <path>",
        start.display()
    )
}

fn normalize(path: &Path) -> PathBuf {
    let Ok(canonical) = path.canonicalize() else {
        return path.to_path_buf();
    };
    match canonical
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
    {
        Some(stripped) => PathBuf::from(stripped),
        None => canonical,
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

fn leaf_name(entry: &str) -> String {
    entry
        .replace('\\', "/")
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(entry)
        .to_string()
}
