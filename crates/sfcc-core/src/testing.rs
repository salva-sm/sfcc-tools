//! An in-memory WebDAV server shaped like an instance. Paths are under `Sites/`:
//! `Logs/error-blade1-20260922.log`, `Cartridges/b12_x`.

use crate::config::{Config, Credentials};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SITES: &str = "/on/demandware.servlet/webdav/Sites/";
const MODIFIED: &str = "Tue, 22 Sep 2026 08:00:00 GMT";

#[derive(Default)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    modified: BTreeMap<String, String>,
    requests: Vec<String>,
    /// 503 to everything, the way a stopped sandbox answers.
    down: bool,
}

/// Stops when the test's runtime does.
#[derive(Clone)]
pub struct MockDav {
    address: String,
    tree: Arc<Mutex<Tree>>,
}

impl MockDav {
    pub async fn start() -> MockDav {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free port on the loopback interface");
        let address = listener.local_addr().expect("a bound address").to_string();
        let tree = Arc::new(Mutex::new(Tree::default()));
        let served = tree.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tree = served.clone();
                tokio::spawn(async move {
                    let _ = answer(stream, &tree).await;
                });
            }
        });
        MockDav { address, tree }
    }

    pub fn config(&self) -> Config {
        Config {
            dw_json: PathBuf::from("dw.json"),
            hostname: self.address.clone(),
            credentials: Credentials::Basic {
                username: "tester".to_string(),
                password: "secret".to_string(),
            },
            code_version: "version1".to_string(),
            cartridges_dir: PathBuf::from("cartridges"),
            cartridge_filter: None,
            accept_invalid_certs: false,
            api_client: None,
            plain_http: true,
        }
    }

    pub fn put(&self, path: &str, contents: impl Into<Vec<u8>>) {
        let mut tree = self.tree.lock().unwrap();
        tree.files.insert(path.to_string(), contents.into());
    }

    pub fn append(&self, path: &str, contents: &str) {
        let mut tree = self.tree.lock().unwrap();
        tree.files
            .entry(path.to_string())
            .or_default()
            .extend_from_slice(contents.as_bytes());
    }

    /// A folder with a last-modified date: a code version.
    pub fn folder(&self, path: &str, modified: &str) {
        let mut tree = self.tree.lock().unwrap();
        let path = path.trim_end_matches('/');
        tree.files.insert(format!("{path}/.keep"), Vec::new());
        tree.modified.insert(path.to_string(), modified.to_string());
    }

    pub fn set_down(&self, down: bool) {
        self.tree.lock().unwrap().down = down;
    }

    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.tree.lock().unwrap().files.get(path).cloned()
    }

    pub fn requests(&self) -> Vec<String> {
        self.tree.lock().unwrap().requests.clone()
    }
}

async fn answer(mut stream: TcpStream, tree: &Mutex<Tree>) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_string();
    let target = first.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_string()))
        .collect();
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };

    let length: usize = header("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = request[head_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }

    let path = decode(target.strip_prefix(SITES).unwrap_or(&target));
    let path = path.trim_end_matches('/').to_string();
    let (status, extra, payload) = {
        let mut tree = tree.lock().unwrap();
        tree.requests.push(format!("{method} {path}"));
        let exists = |tree: &Tree, path: &str| {
            let prefix = format!("{path}/");
            tree.files
                .keys()
                .any(|file| file == path || file.starts_with(&prefix))
        };
        match (header("authorization").is_some(), method.as_str()) {
            _ if tree.down => ("503 Service Unavailable", String::new(), Vec::new()),
            (false, _) => ("401 Unauthorized", String::new(), Vec::new()),
            (true, "PUT") => {
                tree.files.insert(path.clone(), body);
                ("201 Created", String::new(), Vec::new())
            }
            (true, "MKCOL") if exists(&tree, &path) => {
                ("405 Method Not Allowed", String::new(), Vec::new())
            }
            (true, "MKCOL") => {
                tree.files.insert(format!("{path}/.keep"), Vec::new());
                ("201 Created", String::new(), Vec::new())
            }
            (true, "DELETE") if exists(&tree, &path) => {
                let prefix = format!("{path}/");
                tree.files
                    .retain(|file, _| file != &path && !file.starts_with(&prefix));
                ("204 No Content", String::new(), Vec::new())
            }
            (true, "DELETE") => ("404 Not Found", String::new(), Vec::new()),
            (true, "PROPFIND") => match listing(&tree, &path) {
                Some(xml) => ("207 Multi-Status", String::new(), xml.into_bytes()),
                None => ("404 Not Found", String::new(), Vec::new()),
            },
            (true, "GET") => match tree.files.get(&path) {
                None => ("404 Not Found", String::new(), Vec::new()),
                Some(contents) => {
                    let from = header("range")
                        .and_then(|range| {
                            range
                                .strip_prefix("bytes=")?
                                .trim_end_matches('-')
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap_or(0);
                    match (from, contents.len()) {
                        (0, _) => ("200 OK", String::new(), contents.clone()),
                        (from, size) if from >= size => (
                            "416 Range Not Satisfiable",
                            format!("Content-Range: bytes */{size}\r\n"),
                            Vec::new(),
                        ),
                        (from, size) => (
                            "206 Partial Content",
                            format!("Content-Range: bytes {from}-{}/{size}\r\n", size - 1),
                            contents[from..].to_vec(),
                        ),
                    }
                }
            },
            _ => ("405 Method Not Allowed", String::new(), Vec::new()),
        }
    };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.shutdown().await
}

/// The folder itself first, then its direct members.
fn listing(tree: &Tree, folder: &str) -> Option<String> {
    let prefix = format!("{folder}/");
    let mut members: BTreeMap<String, Option<usize>> = BTreeMap::new();
    for (path, contents) in &tree.files {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once('/') {
            Some((child, _)) => {
                members.insert(child.to_string(), None);
            }
            None if rest != ".keep" => {
                members.insert(rest.to_string(), Some(contents.len()));
            }
            None => {}
        }
    }
    if members.is_empty() && !tree.files.keys().any(|path| path.starts_with(&prefix)) {
        return None;
    }

    let modified = |path: &str| {
        tree.modified
            .get(path)
            .cloned()
            .unwrap_or_else(|| MODIFIED.to_string())
    };
    let mut xml =
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
    xml.push_str(&response(
        &format!("{SITES}{folder}/"),
        None,
        &modified(folder),
    ));
    for (name, size) in members {
        let path = format!("{folder}/{name}");
        let href = match size {
            Some(_) => format!("{SITES}{}", crate::webdav::encode_path(&path)),
            None => format!("{SITES}{}/", crate::webdav::encode_path(&path)),
        };
        xml.push_str(&response(&href, size, &modified(&path)));
    }
    xml.push_str("</D:multistatus>");
    Some(xml)
}

fn response(href: &str, size: Option<usize>, modified: &str) -> String {
    let kind = match size {
        Some(size) => format!("<D:resourcetype/><D:getcontentlength>{size}</D:getcontentlength>"),
        None => "<D:resourcetype><D:collection/></D:resourcetype>".to_string(),
    };
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop>{kind}<D:getlastmodified>{modified}</D:getlastmodified></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
    )
}

fn decode(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(value) = encoded
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            decoded.push(value);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}
