//! The instance's Script Debugger API, as a client.
//!
//! `https://<host>/s/-/dw/debugger/v2_0` is a plain REST API: create a client,
//! set breakpoints, poll for a halted thread, read its frames. There is no
//! push — nothing tells you a breakpoint was hit, so [`Session::halted`] is
//! polled.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sfcc_core::config::{Config, Credentials};

const TIMEOUT: Duration = Duration::from_secs(30);

/// A breakpoint, as the instance records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    /// Assigned by the instance once the line is bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    /// Cartridge-relative path, which is what the instance knows files by.
    pub script_path: String,
    /// One-based line.
    pub line_number: u32,
    /// Only halt when this expression is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// A script thread the instance is running, or has halted.
#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    /// The instance's id for it.
    pub id: u32,
    /// `running`, `halted` or `done`.
    pub status: String,
    /// Innermost frame first; only present once halted.
    #[serde(default)]
    pub call_stack: Vec<Frame>,
}

impl Thread {
    /// Whether the thread is sitting on a breakpoint.
    pub fn is_halted(&self) -> bool {
        self.status.eq_ignore_ascii_case("halted")
    }
}

/// One frame of a halted thread's call stack.
#[derive(Debug, Clone, Deserialize)]
pub struct Frame {
    /// Where execution is, in this frame.
    pub location: Location,
}

/// A place in a script, as the instance names it.
#[derive(Debug, Clone, Deserialize)]
pub struct Location {
    /// The enclosing function, when it has a name.
    #[serde(default)]
    pub function_name: Option<String>,
    /// One-based line.
    pub line_number: u32,
    /// Cartridge-relative path.
    pub script_path: String,
}

/// A variable in a frame, or a member of one.
#[derive(Debug, Clone, Deserialize)]
pub struct Variable {
    /// The identifier.
    pub name: String,
    /// The declared or inferred type. `type` is a keyword here, so the
    /// field has to be renamed — and without the rename serde silently
    /// looks for `type_` in the answer and never finds it.
    #[serde(rename = "type", default)]
    pub type_: Option<String>,
    /// The value, already rendered by the instance.
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Deserialize)]
struct Breakpoints {
    #[serde(default)]
    breakpoints: Vec<Breakpoint>,
}

#[derive(Deserialize)]
struct Threads {
    #[serde(default)]
    script_threads: Vec<Thread>,
}

#[derive(Deserialize)]
struct Variables {
    #[serde(default)]
    object_members: Vec<Variable>,
}

#[derive(Deserialize)]
struct Evaluation {
    #[serde(default)]
    result: Option<String>,
}

/// One debugging session against one instance.
pub struct Session {
    base: String,
    authorization: String,
    client_id: String,
    agent: ureq::Agent,
    /// The instance pins a session to one app server with `dwsid`. Without
    /// replaying it, a later request can land on a server that has never
    /// heard of this debugger.
    cookies: Mutex<BTreeMap<String, String>>,
}

impl Session {
    /// Open a session. The instance allows one debugger client at a time, so
    /// this fails while Prophet or the VS Code extension is attached.
    pub fn open(config: &Config, client_id: &str) -> Result<Session> {
        let Credentials::Basic { username, password } = &config.credentials else {
            bail!(
                "the script debugger needs a Business Manager user in dw.json - \
                 \"username\" and \"password\", or a WebDAV access key. An API \
                 client on its own cannot attach."
            );
        };

        let session = Session {
            base: format!("https://{}/s/-/dw/debugger/v2_0", config.hostname),
            authorization: format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            ),
            client_id: client_id.to_string(),
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(TIMEOUT))
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .provider(ureq::tls::TlsProvider::NativeTls)
                        // Only ever true for a sandbox with a self-signed
                        // certificate, which `dw.json` has to opt into.
                        .disable_verification(config.accept_invalid_certs)
                        .build(),
                )
                .build()
                .into(),
            cookies: Mutex::new(BTreeMap::new()),
        };
        session.send("POST", "/client", None)?;
        Ok(session)
    }

    /// Replace every breakpoint with this set, and report what bound.
    pub fn set_breakpoints(&self, wanted: &[Breakpoint]) -> Result<Vec<Breakpoint>> {
        if wanted.is_empty() {
            self.send("DELETE", "/breakpoints", None)?;
            return Ok(Vec::new());
        }
        let body = serde_json::json!({ "breakpoints": wanted });
        let answer = self.send("POST", "/breakpoints", Some(body))?;
        Ok(parse::<Breakpoints>(answer)?.breakpoints)
    }

    /// Every thread the instance is tracking.
    pub fn threads(&self) -> Result<Vec<Thread>> {
        let answer = self.send("GET", "/threads", None)?;
        Ok(parse::<Threads>(answer)?.script_threads)
    }

    /// The first thread sitting on a breakpoint, if any.
    pub fn halted(&self) -> Result<Option<Thread>> {
        Ok(self.threads()?.into_iter().find(Thread::is_halted))
    }

    /// Let a halted thread continue.
    pub fn resume(&self, thread: u32) -> Result<()> {
        self.send("POST", &format!("/threads/{thread}/resume"), None)?;
        Ok(())
    }

    /// Step, by one of `over`, `into` or `out`.
    pub fn step(&self, thread: u32, kind: &str) -> Result<()> {
        self.send("POST", &format!("/threads/{thread}/{kind}"), None)?;
        Ok(())
    }

    /// The variables of a frame, or the members of one object within it.
    pub fn members(&self, thread: u32, frame: usize, path: Option<&str>) -> Result<Vec<Variable>> {
        let route = match path {
            Some(path) => format!(
                "/threads/{thread}/frames/{frame}/members?object_path={}",
                encode(path)
            ),
            None => format!("/threads/{thread}/frames/{frame}/members"),
        };
        let answer = self.send("GET", &route, None)?;
        Ok(parse::<Variables>(answer)?.object_members)
    }

    /// Evaluate an expression in the context of a frame.
    pub fn evaluate(&self, thread: u32, frame: usize, expression: &str) -> Result<String> {
        let route = format!(
            "/threads/{thread}/frames/{frame}/eval?expr={}",
            encode(expression)
        );
        let answer = self.send("GET", &route, None)?;
        Ok(parse::<Evaluation>(answer)?.result.unwrap_or_default())
    }

    /// End the session, freeing the instance's single debugger slot.
    pub fn close(&self) {
        let _ = self.send("DELETE", "/client", None);
    }

    fn send(&self, method: &str, route: &str, body: Option<serde_json::Value>) -> Result<String> {
        let url = format!("{}{route}", self.base);
        let mut request = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("Authorization", &self.authorization)
            .header("x-dw-client-id", &self.client_id)
            .header("Content-Type", "application/json");
        if let Some(cookie) = self.cookie_header() {
            request = request.header("Cookie", cookie);
        }

        let sent = match body {
            Some(value) => {
                let payload = serde_json::to_string(&value)?;
                self.agent.run(request.body(payload)?)
            }
            None => self.agent.run(request.body(())?),
        };
        let mut response = match sent {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) => {
                bail!("{method} {route} failed: HTTP {status}")
            }
            Err(error) => bail!("{method} {route} failed: {error}"),
        };

        self.remember_cookies(&response);
        response
            .body_mut()
            .read_to_string()
            .with_context(|| format!("cannot read the answer to {method} {route}"))
    }

    fn cookie_header(&self) -> Option<String> {
        let cookies = self.cookies.lock().ok()?;
        (!cookies.is_empty()).then(|| {
            cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    fn remember_cookies(&self, response: &ureq::http::Response<ureq::Body>) {
        let Ok(mut cookies) = self.cookies.lock() else {
            return;
        };
        for header in response.headers().get_all("set-cookie") {
            let Ok(value) = header.to_str() else {
                continue;
            };
            if let Some((name, value)) = value.split(';').next().and_then(|it| it.split_once('=')) {
                cookies.insert(name.trim().to_string(), value.trim().to_string());
            }
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: String) -> Result<T> {
    // An empty body is a legitimate 204; treat it as the empty shape.
    let body = match body.trim().is_empty() {
        true => "{}".to_string(),
        false => body,
    };
    serde_json::from_str(&body).with_context(|| format!("unexpected answer: {body}"))
}

/// Percent-encode what goes in a query string. Only the characters that
/// actually break a URL, since an expression is easier to read in a log with
/// its brackets intact.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_only_halted_as_halted() {
        let halted = Thread {
            id: 1,
            status: "halted".into(),
            call_stack: Vec::new(),
        };
        assert!(halted.is_halted());
        assert!(
            !Thread {
                status: "running".into(),
                ..halted
            }
            .is_halted()
        );
    }

    #[test]
    fn encodes_what_would_break_a_query_string() {
        assert_eq!(encode("session.custom.x"), "session.custom.x");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("x&y=1"), "x%26y%3D1");
    }

    #[test]
    fn reads_the_shape_the_instance_answers_with() {
        let body = r#"{"script_threads":[{"id":7,"status":"halted","call_stack":[
            {"location":{"function_name":"show","line_number":42,
             "script_path":"/app_brand/cartridge/controllers/Account.js"}}]}]}"#;
        let threads = parse::<Threads>(body.into()).unwrap().script_threads;
        assert_eq!(threads.len(), 1);
        assert!(threads[0].is_halted());
        assert_eq!(threads[0].call_stack[0].location.line_number, 42);
    }

    #[test]
    fn reads_the_type_the_answer_actually_carries() {
        let body =
            r#"{"object_members":[{"name":"currency","type":"dw.util.Currency","value":"EUR"}]}"#;
        let members = parse::<Variables>(body.into()).unwrap().object_members;
        assert_eq!(members[0].type_.as_deref(), Some("dw.util.Currency"));
    }

    #[test]
    fn takes_an_empty_body_for_the_empty_shape() {
        assert!(
            parse::<Threads>(String::new())
                .unwrap()
                .script_threads
                .is_empty()
        );
    }
}
