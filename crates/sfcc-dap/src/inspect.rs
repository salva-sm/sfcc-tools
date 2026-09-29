//! Values from the halted frame, for `evaluate` and for isml-lsp's hover, which asks on a
//! localhost port: one JSON line each way, `{"token", "expression"}` in, `{"value", "type", "members"}` out.

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
use sfcc_core::state::{self, sessions};

/// The frame the editor last opened, so a hover reads the one on screen.
pub type Selected = Arc<Mutex<Option<(u32, usize)>>>;

const READ_TIMEOUT: Duration = Duration::from_secs(2);
const MEMBERS: usize = 60;

pub struct Found {
    pub value: String,
    pub type_: Option<String>,
    pub object: bool,
}

/// A plain name is read from its parent's members, which carry its type; the rest is evaluated.
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

    let file = sessions::path(std::process::id());
    let record = sessions::Session {
        cartridges: cartridges.to_string_lossy().into_owned(),
        port,
        token: token.clone(),
    };
    state::write(&file, &record);
    if !file.is_file() {
        return None;
    }

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
