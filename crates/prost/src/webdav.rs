use crate::config::{Config, Credentials};
use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const MAX_ATTEMPTS: u32 = 4;
const OAUTH_URL: &str = "https://account.demandware.com/dwsso/oauth2/access_token";
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    Ready,
    MissingCodeVersion,
    Unauthorized,
    Unavailable(String),
}

#[derive(Debug, Clone)]
pub struct DavEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: String,
}

struct Token {
    value: String,
    expires_at: Instant,
}

pub struct Dav {
    client: Client,
    root: String,
    base: String,
    credentials: Credentials,
    token: RwLock<Option<Token>>,
}

impl Dav {
    pub fn new(config: &Config) -> Result<Dav> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(16)
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .build()
            .context("cannot build the HTTP client")?;

        Ok(Dav {
            client,
            root: config.webdav_root(),
            base: config.code_version_url(),
            credentials: config.credentials.clone(),
            token: RwLock::new(None),
        })
    }

    pub fn file_url(&self, relative_path: &str) -> String {
        format!("{}/{}", self.base, encode_path(relative_path))
    }

    pub fn root_url(&self) -> &str {
        &self.root
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn availability(&self) -> Availability {
        let response = self
            .send(|| {
                self.client
                    .request(propfind(), &self.base)
                    .header("Depth", "0")
                    .header("Content-Type", "text/xml; charset=utf-8")
                    .timeout(Duration::from_secs(15))
                    .body(PROPFIND_BODY)
            })
            .await;

        match response {
            Ok(response)
                if response.status() == StatusCode::MULTI_STATUS || response.status().is_success() =>
            {
                Availability::Ready
            }
            Ok(response) if response.status() == StatusCode::NOT_FOUND => Availability::MissingCodeVersion,
            Ok(response)
                if response.status() == StatusCode::UNAUTHORIZED
                    || response.status() == StatusCode::FORBIDDEN =>
            {
                Availability::Unauthorized
            }
            Ok(response) => Availability::Unavailable(format!("HTTP {}", response.status())),
            Err(error) => Availability::Unavailable(root_cause(&error)),
        }
    }

    pub async fn wait_until_ready(&self, max_wait: Option<Duration>) -> Result<()> {
        let started = Instant::now();
        let mut announced = false;
        loop {
            match self.availability().await {
                Availability::Ready => {
                    if announced {
                        crate::logging::ok("sandbox is back online");
                    }
                    return Ok(());
                }
                Availability::MissingCodeVersion => {
                    self.mkcol(&self.base).await?;
                    return Ok(());
                }
                Availability::Unauthorized => {
                    bail!("the sandbox rejected the credentials from dw.json (HTTP 401/403)")
                }
                Availability::Unavailable(reason) => {
                    if let Some(limit) = max_wait {
                        if started.elapsed() >= limit {
                            bail!("sandbox unreachable after {}s: {reason}", limit.as_secs());
                        }
                    }
                    if !announced {
                        crate::logging::warn(format!("sandbox unavailable ({reason}) - waiting"));
                        announced = true;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    pub async fn mkcol(&self, url: &str) -> Result<()> {
        let response = self.send(|| self.client.request(mkcol(), url)).await?;
        let status = response.status();
        if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::CONFLICT {
            return Ok(());
        }
        bail!("MKCOL {url} failed with HTTP {status}")
    }

    pub async fn ensure_directory(&self, relative_path: &str) -> Result<()> {
        let mut walked = String::new();
        for segment in relative_path.split('/').filter(|segment| !segment.is_empty()) {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(segment);
            self.mkcol(&self.file_url(&walked)).await?;
        }
        Ok(())
    }

    pub async fn put(&self, relative_path: &str, body: Vec<u8>) -> Result<()> {
        let url = self.file_url(relative_path);
        let response = self
            .send(|| {
                self.client
                    .put(&url)
                    .header("Content-Type", "application/octet-stream")
                    .body(body.clone())
            })
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        bail!("PUT {relative_path} failed with HTTP {status}")
    }

    pub async fn delete(&self, relative_path: &str) -> Result<bool> {
        let url = self.file_url(relative_path);
        let response = self.send(|| self.client.delete(&url)).await?;
        let status = response.status();
        if status.is_success() {
            return Ok(true);
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        bail!("DELETE {relative_path} failed with HTTP {status}")
    }

    pub async fn delete_code_version(&self) -> Result<bool> {
        self.delete("").await
    }

    pub async fn unzip(&self, relative_path: &str) -> Result<()> {
        let url = self.file_url(relative_path);
        let response = self
            .send(|| {
                self.client
                    .post(&url)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body("method=UNZIP")
            })
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        bail!("remote unzip of {relative_path} failed with HTTP {status}")
    }

    pub async fn read_from(&self, url: &str, offset: u64) -> Result<String> {
        let response = self
            .send(|| self.client.get(url).header("Range", format!("bytes={offset}-")))
            .await?;

        let status = response.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE || status == StatusCode::NOT_FOUND {
            return Ok(String::new());
        }
        if !status.is_success() {
            bail!("GET {url} failed with HTTP {status}");
        }

        let body = response.text().await.unwrap_or_default();
        if status == StatusCode::PARTIAL_CONTENT || offset == 0 {
            return Ok(body);
        }
        Ok(body.get(offset as usize..).unwrap_or_default().to_string())
    }

    pub async fn list(&self, url: &str) -> Result<Vec<DavEntry>> {
        let response = self
            .send(|| {
                self.client
                    .request(propfind(), url)
                    .header("Depth", "1")
                    .header("Content-Type", "text/xml; charset=utf-8")
                    .body(PROPFIND_BODY)
            })
            .await?;

        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if !(status.is_success() || status == StatusCode::MULTI_STATUS) {
            bail!("PROPFIND {url} failed with HTTP {status}");
        }

        let body = response.text().await.context("cannot read the PROPFIND response")?;
        Ok(parse_multistatus(&body, url))
    }

    async fn send<F>(&self, build: F) -> Result<Response>
    where
        F: Fn() -> RequestBuilder,
    {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let request = self.authorize(build()).await?;
            match request.send().await {
                Ok(response) => {
                    if response.status().is_server_error() && attempt < MAX_ATTEMPTS {
                        backoff(attempt).await;
                        continue;
                    }
                    return Ok(response);
                }
                Err(error) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(error).context("the request to the sandbox failed");
                    }
                    backoff(attempt).await;
                }
            }
        }
    }

    async fn authorize(&self, builder: RequestBuilder) -> Result<RequestBuilder> {
        match &self.credentials {
            Credentials::Basic { username, password } => Ok(builder.basic_auth(username, Some(password))),
            Credentials::OAuth { .. } => Ok(builder.bearer_auth(self.access_token().await?)),
        }
    }

    async fn access_token(&self) -> Result<String> {
        if let Some(token) = self.token.read().await.as_ref() {
            if token.expires_at > Instant::now() {
                return Ok(token.value.clone());
            }
        }

        let Credentials::OAuth { client_id, client_secret } = &self.credentials else {
            bail!("no OAuth credentials configured");
        };

        let response = self
            .client
            .post(OAUTH_URL)
            .basic_auth(client_id, Some(client_secret))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials")
            .send()
            .await
            .context("cannot reach Account Manager for an access token")?;

        if !response.status().is_success() {
            bail!("Account Manager rejected the client credentials (HTTP {})", response.status());
        }

        let body = response.text().await.context("cannot read the token response")?;
        let payload: serde_json::Value =
            serde_json::from_str(&body).context("malformed token response")?;
        let value = payload["access_token"]
            .as_str()
            .context("token response without access_token")?
            .to_string();
        let lifetime = payload["expires_in"].as_u64().unwrap_or(1800).saturating_sub(60);

        *self.token.write().await = Some(Token {
            value: value.clone(),
            expires_at: Instant::now() + Duration::from_secs(lifetime),
        });
        Ok(value)
    }
}

async fn backoff(attempt: u32) {
    tokio::time::sleep(Duration::from_millis(400_u64 << attempt.min(5))).await;
}

fn propfind() -> Method {
    Method::from_bytes(b"PROPFIND").expect("PROPFIND is a valid method name")
}

fn mkcol() -> Method {
    Method::from_bytes(b"MKCOL").expect("MKCOL is a valid method name")
}

fn root_cause(error: &anyhow::Error) -> String {
    error
        .chain()
        .last()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| error.to_string())
}

pub fn encode_path(relative_path: &str) -> String {
    let mut encoded = String::with_capacity(relative_path.len());
    for byte in relative_path.replace('\\', "/").bytes() {
        let character = byte as char;
        let is_safe = character.is_ascii_alphanumeric()
            || matches!(character, '-' | '_' | '.' | '~' | '/' | '(' | ')' | '$' | '@' | '+' | ',' | '=' | ':');
        if is_safe {
            encoded.push(character);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn decode_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
            if let Ok(value) = u8::from_str_radix(hex, 16) {
                decoded.push(value);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn parse_multistatus(body: &str, requested_url: &str) -> Vec<DavEntry> {
    let requested_name = decode_path(requested_url.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
    let mut entries = Vec::new();

    for block in body.split("response>").skip(1) {
        let Some(href) = inner_text(block, "href") else {
            continue;
        };
        let name = decode_path(href.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
        if name.is_empty() || name == requested_name {
            continue;
        }
        entries.push(DavEntry {
            name,
            is_dir: block.contains("collection/>") || block.contains("collection />"),
            size: inner_text(block, "getcontentlength")
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0),
            modified: inner_text(block, "getlastmodified").unwrap_or_default().trim().to_string(),
        });
    }
    entries
}

fn inner_text(block: &str, local_name: &str) -> Option<String> {
    let open = format!("{local_name}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find('<')? + start;
    Some(block[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "<D:multistatus xmlns:D=\"DAV:\">",
        "<D:response><D:href>/webdav/Sites/Cartridges/version1/</D:href>",
        "<D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response>",
        "<D:response><D:href>/webdav/Sites/Cartridges/version1/app%20one/</D:href>",
        "<D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype>",
        "<D:getlastmodified>Fri, 05 Sep 2026 10:00:00 GMT</D:getlastmodified></D:prop></D:propstat></D:response>",
        "<D:response><D:href>/webdav/Sites/Cartridges/version1/readme.txt</D:href>",
        "<D:propstat><D:prop><D:resourcetype/><D:getcontentlength>42</D:getcontentlength></D:prop></D:propstat></D:response>",
        "</D:multistatus>"
    );

    #[test]
    fn encodes_unsafe_characters_but_keeps_separators() {
        assert_eq!(encode_path("app/cartridge/a b.js"), "app/cartridge/a%20b.js");
        assert_eq!(encode_path(r"app\cartridge\x.js"), "app/cartridge/x.js");
        assert_eq!(encode_path("app_common-eu/x.min.js"), "app_common-eu/x.min.js");
    }

    #[test]
    fn parses_entries_and_skips_the_requested_collection() {
        let entries = parse_multistatus(SAMPLE, "https://host/webdav/Sites/Cartridges/version1");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["app one", "readme.txt"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
    }
}
