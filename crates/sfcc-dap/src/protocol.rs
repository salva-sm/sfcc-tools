//! The Debug Adapter Protocol on the wire.
//!
//! Same framing as LSP — `Content-Length` header, blank line, JSON body —
//! but its own message shapes: requests carry a `command`, responses quote
//! the `request_seq` they answer, and events arrive unprompted.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

/// A request from the editor.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// The editor's sequence number, quoted back in the response.
    pub seq: i64,
    /// What is being asked for: `initialize`, `stackTrace`, and the rest.
    pub command: String,
    /// The command's arguments, absent for those that take none.
    #[serde(default)]
    pub arguments: Value,
}

impl Request {
    /// One argument, or `Value::Null` when the editor sent none.
    pub fn argument(&self, name: &str) -> &Value {
        self.arguments.get(name).unwrap_or(&Value::Null)
    }

    /// One argument as a number, which is how the protocol carries every id.
    pub fn number(&self, name: &str) -> Option<i64> {
        self.argument(name).as_i64()
    }
}

/// Writes messages to the editor, from whichever thread has something to say.
#[derive(Clone)]
pub struct Writer {
    out: std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    seq: std::sync::Arc<AtomicI64>,
}

impl Writer {
    /// Wrap a stream — stdout in a real session.
    pub fn new(out: Box<dyn Write + Send>) -> Writer {
        Writer {
            out: std::sync::Arc::new(std::sync::Mutex::new(out)),
            seq: std::sync::Arc::new(AtomicI64::new(0)),
        }
    }

    /// Answer a request.
    pub fn respond(&self, request: &Request, body: Value) {
        self.send(json!({
            "type": "response",
            "request_seq": request.seq,
            "success": true,
            "command": request.command,
            "body": body,
        }));
    }

    /// Refuse a request, with a message the editor shows.
    pub fn fail(&self, request: &Request, message: impl AsRef<str>) {
        self.send(json!({
            "type": "response",
            "request_seq": request.seq,
            "success": false,
            "command": request.command,
            "message": message.as_ref(),
        }));
    }

    /// Tell the editor something it did not ask about.
    pub fn event(&self, name: &str, body: Value) {
        self.send(json!({ "type": "event", "event": name, "body": body }));
    }

    /// Write a line to the debug console.
    pub fn log(&self, text: impl AsRef<str>) {
        self.event(
            "output",
            json!({ "category": "console", "output": format!("{}\n", text.as_ref()) }),
        );
    }

    fn send(&self, mut message: Value) {
        message["seq"] = json!(self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        let body = message.to_string();
        let Ok(mut out) = self.out.lock() else {
            return;
        };
        let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = out.flush();
    }
}

/// Read one message. `None` at end of stream.
pub fn read(input: &mut impl BufRead) -> Result<Option<Request>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed
            .strip_prefix("Content-Length:")
            .or_else(|| trimmed.strip_prefix("content-length:"))
        {
            length = value.trim().parse::<usize>().ok();
        }
    }

    let Some(length) = length else {
        return Ok(None);
    };
    let mut body = vec![0; length];
    input.read_exact(&mut body).context("truncated message")?;
    let text = String::from_utf8(body).context("message is not UTF-8")?;
    Ok(serde_json::from_str(&text).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn reads_a_framed_request() {
        let body = r#"{"seq":3,"type":"request","command":"threads"}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let request = read(&mut BufReader::new(raw.as_bytes())).unwrap().unwrap();
        assert_eq!(request.seq, 3);
        assert_eq!(request.command, "threads");
    }

    #[test]
    fn reads_arguments_by_name() {
        let body = r#"{"seq":1,"command":"continue","arguments":{"threadId":7}}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let request = read(&mut BufReader::new(raw.as_bytes())).unwrap().unwrap();
        assert_eq!(request.number("threadId"), Some(7));
        assert_eq!(request.number("missing"), None);
    }

    #[test]
    fn stops_at_the_end_of_the_stream() {
        assert!(read(&mut BufReader::new(&b""[..])).unwrap().is_none());
    }

    #[test]
    fn frames_what_it_writes_and_numbers_it() {
        let sink = Shared::default();
        let writer = Writer::new(Box::new(sink.clone()));
        writer.event("initialized", json!({}));
        writer.event("terminated", json!({}));

        let written = sink.text();
        assert!(written.contains("Content-Length:"));
        assert!(written.contains(r#""seq":1"#));
        assert!(written.contains(r#""seq":2"#));
    }

    #[derive(Clone, Default)]
    struct Shared(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
