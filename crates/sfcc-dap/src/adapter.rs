//! The adapter: what the editor asks for, turned into API calls.
//!
//! Two things run at once. The main loop answers requests; a background
//! thread polls the instance, because nothing there tells us a breakpoint
//! was hit — see [`sdapi`](crate::sdapi).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::logs::Logs;
use crate::paths::Paths;
use crate::protocol::{Request, Writer};
use crate::sdapi::{Breakpoint, Session, Variable};
use crate::variables;

/// How often to ask the instance whether anything halted.
const POLL: Duration = Duration::from_millis(600);
/// Frame ids have to be one number; this packs a thread and a frame index.
const FRAME_STRIDE: i64 = 1000;

/// What a `variablesReference` stands for.
#[derive(Debug, Clone)]
struct Handle {
    thread: u32,
    frame: usize,
    /// `None` for the frame's own variables, otherwise the object to expand.
    object: Option<String>,
    /// The synthetic scope holding the platform globals.
    globals: bool,
}

/// One editor session.
pub struct Adapter {
    session: Arc<Session>,
    paths: Paths,
    writer: Writer,
    handles: Mutex<BTreeMap<i64, Handle>>,
    next_handle: Mutex<i64>,
    /// Breakpoints by source file, so one file's edit replaces only its own.
    by_source: Mutex<BTreeMap<String, Vec<Breakpoint>>>,
    raw_variables: AtomicBool,
    stop: Arc<AtomicBool>,
    /// The `dw.json` this session was given, for the log follower.
    config: PathBuf,
    /// Held for as long as the session lasts; dropping it stops the follower.
    logs: Mutex<Option<Logs>>,
}

impl Adapter {
    /// Attach to an instance the configuration already resolved.
    pub fn new(session: Session, cartridges: PathBuf, config: PathBuf, writer: Writer) -> Adapter {
        Adapter {
            session: Arc::new(session),
            paths: Paths::new(cartridges),
            config,
            logs: Mutex::new(None),
            writer,
            handles: Mutex::new(BTreeMap::new()),
            next_handle: Mutex::new(1),
            by_source: Mutex::new(BTreeMap::new()),
            raw_variables: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Answer one request. `false` means the session is over.
    pub fn handle(&self, request: &Request) -> bool {
        match request.command.as_str() {
            "initialize" => self.initialize(request),
            "attach" | "launch" => self.attach(request),
            "configurationDone" => self.writer.respond(request, json!({})),
            "setBreakpoints" => self.set_breakpoints(request),
            "threads" => self.threads(request),
            "stackTrace" => self.stack_trace(request),
            "scopes" => self.scopes(request),
            "variables" => self.variables(request),
            "evaluate" => self.evaluate(request),
            "continue" => self.resume(request),
            "next" => self.step(request, "over"),
            "stepIn" => self.step(request, "into"),
            "stepOut" => self.step(request, "out"),
            "pause" => self.writer.fail(
                request,
                "the script debugger cannot pause a running request; it only halts on breakpoints",
            ),
            "disconnect" | "terminate" => {
                self.writer.respond(request, json!({}));
                self.shut_down();
                return false;
            }
            _ => self.writer.respond(request, json!({})),
        }
        true
    }

    fn initialize(&self, request: &Request) {
        self.writer.respond(
            request,
            json!({
                "supportsConfigurationDoneRequest": true,
                "supportsConditionalBreakpoints": true,
                "supportsEvaluateForHovers": true,
                "supportsTerminateRequest": true,
            }),
        );
    }

    fn attach(&self, request: &Request) {
        if request.argument("raw_variables") == &Value::Bool(true) {
            self.raw_variables.store(true, Ordering::Relaxed);
        }
        self.follow_logs(request);
        self.writer.respond(request, json!({}));
        self.writer.event("initialized", json!({}));
        self.watch();
    }

    /// The error that did *not* stop at a breakpoint shows up in the sandbox
    /// log, so it belongs in the same window.
    fn follow_logs(&self, request: &Request) {
        if request.argument("logs") == &Value::Bool(false) {
            return;
        }
        let levels = request.argument("log_level").as_str().map(str::to_string);
        let following = Logs::follow(&self.config, levels.as_deref(), &self.writer);
        if let Ok(mut logs) = self.logs.lock() {
            *logs = Some(following);
        }
    }

    /// Poll for a halted thread and announce it once, until disconnect.
    fn watch(&self) {
        let session = Arc::clone(&self.session);
        let writer = self.writer.clone();
        let stop = Arc::clone(&self.stop);
        std::thread::spawn(move || {
            let mut announced: Option<u32> = None;
            while !stop.load(Ordering::Relaxed) {
                match session.halted() {
                    Ok(Some(thread)) if announced != Some(thread.id) => {
                        announced = Some(thread.id);
                        writer.event(
                            "thread",
                            json!({ "reason": "started", "threadId": thread.id }),
                        );
                        writer.event(
                            "stopped",
                            json!({
                                "reason": "breakpoint",
                                "threadId": thread.id,
                                "allThreadsStopped": false,
                            }),
                        );
                    }
                    Ok(None) => announced = None,
                    Ok(Some(_)) => {}
                    Err(error) => {
                        writer.log(format!("the instance stopped answering: {error:#}"));
                        return;
                    }
                }
                std::thread::sleep(POLL);
            }
        });
    }

    fn set_breakpoints(&self, request: &Request) {
        let source = request.argument("source");
        let Some(local) = source.get("path").and_then(Value::as_str) else {
            return self
                .writer
                .fail(request, "the editor sent no file to break in");
        };
        let Some(script) = self.paths.to_script(&PathBuf::from(local)) else {
            return self.writer.fail(
                request,
                format!("{local} is not under the cartridges directory this session was given"),
            );
        };

        let wanted: Vec<Breakpoint> = request
            .argument("breakpoints")
            .as_array()
            .map(|points| {
                points
                    .iter()
                    .filter_map(|point| {
                        Some(Breakpoint {
                            id: None,
                            script_path: script.clone(),
                            line_number: point.get("line")?.as_u64()? as u32,
                            condition: point
                                .get("condition")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        // The API replaces the whole set at once, so every file's points go
        // up together or the others would be dropped.
        let all = {
            let Ok(mut by_source) = self.by_source.lock() else {
                return self.writer.fail(request, "internal state is poisoned");
            };
            by_source.insert(script.clone(), wanted.clone());
            by_source.values().flatten().cloned().collect::<Vec<_>>()
        };

        let bound = match self.session.set_breakpoints(&all) {
            Ok(bound) => bound,
            Err(error) => return self.writer.fail(request, format!("{error:#}")),
        };

        let answer: Vec<Value> = wanted
            .iter()
            .map(|point| {
                let hit = bound.iter().find(|candidate| {
                    candidate.script_path == point.script_path
                        && candidate.line_number == point.line_number
                });
                match hit {
                    Some(hit) => json!({
                        "id": hit.id, "verified": true, "line": hit.line_number, "source": source,
                    }),
                    None => json!({
                        "verified": false, "line": point.line_number, "source": source,
                        "message": "the instance did not bind this line",
                    }),
                }
            })
            .collect();
        self.writer
            .respond(request, json!({ "breakpoints": answer }));
    }

    fn threads(&self, request: &Request) {
        let threads = self.session.threads().unwrap_or_default();
        let listed: Vec<Value> = threads
            .iter()
            .map(|thread| {
                json!({
                    "id": thread.id,
                    "name": format!("Request thread {} ({})", thread.id, thread.status),
                })
            })
            .collect();
        self.writer.respond(request, json!({ "threads": listed }));
    }

    fn stack_trace(&self, request: &Request) {
        let Some(id) = request.number("threadId") else {
            return self.writer.respond(request, json!({ "stackFrames": [] }));
        };
        let thread = self
            .session
            .threads()
            .unwrap_or_default()
            .into_iter()
            .find(|thread| i64::from(thread.id) == id);
        let Some(thread) = thread else {
            return self.writer.respond(request, json!({ "stackFrames": [] }));
        };

        let frames: Vec<Value> = thread
            .call_stack
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                let script = &frame.location.script_path;
                let local = self.paths.to_local(script);
                json!({
                    "id": i64::from(thread.id) * FRAME_STRIDE + index as i64,
                    "name": self.frame_name(frame.location.function_name.as_deref(), script),
                    "line": frame.location.line_number,
                    "column": 1,
                    "source": {
                        "name": local.file_name().map(|name| name.to_string_lossy().into_owned()),
                        "path": local.to_string_lossy(),
                    },
                })
            })
            .collect();
        self.writer.respond(
            request,
            json!({ "stackFrames": frames, "totalFrames": frames.len() }),
        );
    }

    /// A stack crossing four cartridges reads as four unrelated files unless
    /// each frame says which one it is in, and which others hold the same.
    fn frame_name(&self, function: Option<&str>, script: &str) -> String {
        let name = function.unwrap_or("(anonymous)");
        let Some(cartridge) = Paths::cartridge_of(script) else {
            return name.to_string();
        };
        match self.paths.also_in(script).as_slice() {
            [] => format!("{name}  ·  {cartridge}"),
            others => format!("{name}  ·  {cartridge}, also in {}", others.join(", ")),
        }
    }

    fn scopes(&self, request: &Request) {
        let Some((thread, frame)) = request.number("frameId").map(split_frame) else {
            return self.writer.respond(request, json!({ "scopes": [] }));
        };
        // SFCC first: `pdict`, `request` and `session` are platform globals,
        // in neither the local nor the closure scope, so without this they
        // mean typing into the watch box at every breakpoint.
        self.writer.respond(
            request,
            json!({
                "scopes": [
                    { "name": "SFCC", "variablesReference": self.handle_for(thread, frame, None, true), "expensive": false },
                    { "name": "Locals", "variablesReference": self.handle_for(thread, frame, None, false), "expensive": false },
                ]
            }),
        );
    }

    fn variables(&self, request: &Request) {
        let reference = request.number("variablesReference").unwrap_or_default();
        let Some(handle) = self.resolve(reference) else {
            return self.writer.respond(request, json!({ "variables": [] }));
        };

        let listed = match handle.globals {
            true => self.globals(&handle),
            false => self.members(&handle),
        };
        self.writer.respond(request, json!({ "variables": listed }));
    }

    fn members(&self, handle: &Handle) -> Vec<Value> {
        let found = self
            .session
            .members(handle.thread, handle.frame, handle.object.as_deref())
            .unwrap_or_default();
        let raw = self.raw_variables.load(Ordering::Relaxed);
        found
            .iter()
            .filter(|variable| {
                raw || variables::worth_showing(
                    &variable.name,
                    variable.type_.as_deref(),
                    handle.object.is_some(),
                )
            })
            .map(|variable| self.describe(handle, variable))
            .collect()
    }

    /// The platform globals, probed one by one: a frame that does not have
    /// one simply does not list it.
    fn globals(&self, handle: &Handle) -> Vec<Value> {
        variables::SFCC_GLOBALS
            .iter()
            .filter_map(|name| {
                let kind = self
                    .session
                    .evaluate(handle.thread, handle.frame, &format!("typeof {name}"))
                    .ok()?;
                let kind = kind.trim();
                if kind.is_empty() || kind == "undefined" || kind.contains("Error") {
                    return None;
                }
                let value = self
                    .session
                    .evaluate(handle.thread, handle.frame, &format!("String({name})"))
                    .unwrap_or_default();
                Some(json!({
                    "name": name,
                    "value": variables::one_line(&value),
                    "type": kind,
                    "evaluateName": name,
                    "variablesReference": match kind == "object" {
                        true => self.handle_for(handle.thread, handle.frame, Some((*name).to_string()), false),
                        false => 0,
                    },
                }))
            })
            .collect()
    }

    fn describe(&self, handle: &Handle, variable: &Variable) -> Value {
        let path = match &handle.object {
            Some(object) => format!("{object}.{}", variable.name),
            None => variable.name.clone(),
        };
        let value = variable.value.clone().unwrap_or_default();
        let expandable = variables::looks_like_an_object(&value, variable.type_.as_deref());
        json!({
            "name": variable.name,
            "value": variables::one_line(&value),
            "type": variable.type_,
            "evaluateName": path,
            "variablesReference": match expandable {
                true => self.handle_for(handle.thread, handle.frame, Some(path), false),
                false => 0,
            },
        })
    }

    fn evaluate(&self, request: &Request) {
        let expression = request
            .argument("expression")
            .as_str()
            .unwrap_or_default()
            .to_string();
        let Some((thread, frame)) = request.number("frameId").map(split_frame) else {
            return self
                .writer
                .fail(request, "nothing is halted to evaluate in");
        };
        match self.session.evaluate(thread, frame, &expression) {
            Ok(value) if value.starts_with("ReferenceError") || value.starts_with("TypeError") => {
                self.writer.fail(request, value)
            }
            Ok(value) => self
                .writer
                .respond(request, json!({ "result": value, "variablesReference": 0 })),
            Err(error) => self.writer.fail(request, format!("{error:#}")),
        }
    }

    fn resume(&self, request: &Request) {
        let Some(thread) = request.number("threadId") else {
            return self.writer.fail(request, "no thread to resume");
        };
        self.forget_handles();
        match self.session.resume(thread as u32) {
            Ok(()) => self
                .writer
                .respond(request, json!({ "allThreadsContinued": false })),
            Err(error) => self.writer.fail(request, format!("{error:#}")),
        }
    }

    fn step(&self, request: &Request, kind: &str) {
        let Some(thread) = request.number("threadId") else {
            return self.writer.fail(request, "no thread to step");
        };
        self.forget_handles();
        match self.session.step(thread as u32, kind) {
            Ok(()) => self.writer.respond(request, json!({})),
            Err(error) => self.writer.fail(request, format!("{error:#}")),
        }
    }

    fn shut_down(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut logs) = self.logs.lock() {
            *logs = None;
        }
        self.session.close();
        self.writer.event("terminated", json!({}));
    }

    fn handle_for(&self, thread: u32, frame: usize, object: Option<String>, globals: bool) -> i64 {
        let (Ok(mut handles), Ok(mut next)) = (self.handles.lock(), self.next_handle.lock()) else {
            return 0;
        };
        let reference = *next;
        *next += 1;
        handles.insert(
            reference,
            Handle {
                thread,
                frame,
                object,
                globals,
            },
        );
        reference
    }

    fn resolve(&self, reference: i64) -> Option<Handle> {
        self.handles.lock().ok()?.get(&reference).cloned()
    }

    /// Once execution moves, every reference into the old stack is stale.
    fn forget_handles(&self) {
        if let Ok(mut handles) = self.handles.lock() {
            handles.clear();
        }
    }
}

/// The inverse of the packing [`Adapter::stack_trace`] does.
fn split_frame(id: i64) -> (u32, usize) {
    ((id / FRAME_STRIDE) as u32, (id % FRAME_STRIDE) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_a_thread_and_a_frame_into_one_id() {
        assert_eq!(split_frame(7 * FRAME_STRIDE + 3), (7, 3));
        assert_eq!(split_frame(0), (0, 0));
    }
}
