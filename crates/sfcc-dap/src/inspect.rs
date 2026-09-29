//! Values from the halted frame, for the evaluate request and for the language server's hover.
//!
//! Zed asks a debug adapter nothing on hover, but it does ask isml-lsp. So while attached, the
//! adapter answers on a localhost port and says where in one JSON file per adapter under its
//! own folder - the one layout the two programs have to agree on. One connection carries one
//! JSON line each way: `{"token", "expression"}` in, `{"value", "type", "members"}` out.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::sdapi::Session;
use crate::variables;

/// The frame the editor last opened, so a hover reads the one on screen.
pub type Selected = Arc<Mutex<Option<(u32, usize)>>>;

const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MEMBERS: usize = 60;

pub struct Found {
    pub value: String,
    pub type_: Option<String>,
    pub object: bool,
}

/// A plain name is read from its parent's members, so it carries the type and arrow the
/// Variables view shows; anything else goes to the evaluator. `Err` is the message to show.
pub fn read(
    session: &Session,
    thread: u32,
    frame: usize,
    expression: &str,
) -> Result<Found, String> {
    let path = variables::split_path(expression);
    if let Some((parent, leaf)) = path {
        let found = session
            .members(thread, frame, parent)
            .ok()
            .and_then(|members| members.into_iter().find(|variable| variable.name == leaf));
        if let Some(variable) = found {
            let value = variable.value.unwrap_or_default();
            let object = variables::looks_like_an_object(&value, variable.type_.as_deref());
            return Ok(Found {
                value,
                type_: variable.type_,
                object,
            });
        }
    }
    let value = session
        .evaluate(thread, frame, expression)
        .map_err(|error| format!("{error:#}"))?;
    if value.starts_with("ReferenceError") || value.starts_with("TypeError") {
        return Err(value);
    }
    // Globals such as `pdict` are no frame's members; `object_path` still opens them by name.
    let object = path.is_some()
        && session
            .evaluate(thread, frame, &format!("typeof ({expression})"))
            .is_ok_and(|kind| kind.trim() == "object");
    Ok(Found {
        value,
        type_: None,
        object,
    })
}

pub struct Published {
    file: PathBuf,
}

impl Drop for Published {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

pub fn publish(
    session: Arc<Session>,
    cartridges: &Path,
    selected: Selected,
    raw: bool,
) -> Option<Published> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let token = format!("{:016x}", RandomState::new().build_hasher().finish());

    let dir = sessions_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = dir.join(format!("{}.json", std::process::id()));
    let record = json!({
        "cartridges": cartridges.to_string_lossy(),
        "port": port,
        "token": token,
    });
    std::fs::write(&file, record.to_string()).ok()?;

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = answer(&session, &selected, &token, raw, stream);
        }
    });
    Some(Published { file })
}

fn answer(
    session: &Session,
    selected: &Selected,
    token: &str,
    raw: bool,
    mut stream: TcpStream,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let asked: Value = serde_json::from_str(&line).unwrap_or_default();
    let reply = match asked.get("token").and_then(Value::as_str) == Some(token) {
        true => inspect(
            session,
            selected,
            asked
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim(),
            raw,
        ),
        false => json!({ "error": "wrong token" }),
    };
    writeln!(stream, "{reply}")
}

/// Nothing halted is an empty answer, not an error: the hover then shows only the docs.
fn inspect(session: &Session, selected: &Selected, expression: &str, raw: bool) -> Value {
    let Ok(Some(thread)) = session.halted() else {
        return json!({});
    };
    let frame = selected
        .lock()
        .ok()
        .and_then(|selected| *selected)
        .filter(|(on, frame)| *on == thread.id && *frame < thread.call_stack.len())
        .map_or(0, |(_, frame)| frame);

    let found = match read(session, thread.id, frame, expression) {
        Ok(found) => found,
        Err(message) => return json!({ "error": message }),
    };
    let members: Vec<Value> = match found.object {
        true => session
            .members(thread.id, frame, Some(expression))
            .unwrap_or_default()
            .into_iter()
            .filter(|variable| {
                raw || variables::worth_showing(&variable.name, variable.type_.as_deref(), true)
            })
            .take(MEMBERS)
            .map(|variable| {
                json!({
                    "name": variable.name,
                    "type": variable.type_,
                    "value": variable.value.as_deref().map(variables::one_line),
                })
            })
            .collect(),
        false => Vec::new(),
    };
    json!({ "value": found.value, "type": found.type_, "members": members })
}

/// sfcc-dap's folder; isml-lsp derives the same one.
fn sessions_dir() -> Option<PathBuf> {
    if cfg!(windows)
        && let Ok(appdata) = std::env::var("APPDATA")
    {
        return Some(PathBuf::from(appdata).join("sfcc-dap").join("sessions"));
    }
    let root = match std::env::var("XDG_CONFIG_HOME") {
        Ok(config) if !config.is_empty() => PathBuf::from(config),
        _ => PathBuf::from(std::env::var("HOME").ok()?).join(".config"),
    };
    Some(root.join("sfcc-dap").join("sessions"))
}
