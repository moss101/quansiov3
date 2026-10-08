//! One cell of a composition script, run in a fresh embedded interpreter (ADP-009).
//!
//! A cell is the unit of isolation. Each one gets its own QuickJS runtime — a heap limit, a stack limit and an
//! interrupt handler — and a context that holds the language's built-ins and nothing the host did not hand over:
//! no module loader, no `console`, no timers, no `eval`, no `Function` constructors, and three native functions the
//! prelude wraps into `call`, `log` and `image`. Nothing survives the cell except the `state` object, which is
//! serialized to JSON and loaded into the next cell's fresh interpreter, so no function, closure or handle ever
//! crosses a cell.
//!
//! The host call is the only way out and it is synchronous from the script's side: the worker thread that runs the
//! interpreter blocks on the governed dispatch of the tool the script named. That makes the order of effects the order
//! of the script, and it means a script cannot have two calls in flight.
//!
//! **A terminal condition is not an exception the script owns.** When a host call reports one (the person took the
//! computer, a permission was revoked, the run was stopped, a budget is spent, the runtime fenced the call, an
//! approval is pending, an outcome is unknown, the deadline or the call budget passed), the worker records it, the call
//! throws, and from then on: every further host call throws the same condition without reaching the dispatcher, the
//! interrupt handler aborts the interpreter at its next poll, and the cell's result is the terminal condition **whatever
//! the script returns or catches**. A `try/catch` can swallow the throw; it can not keep the cell going, make another
//! effect, or change what the cell reports.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rquickjs::context::intrinsic;
use rquickjs::{Ctx, Exception, Function, Object, Promise, Runtime, Value};
use serde_json::{json, Value as Json};
use tokio::runtime::Handle;

use super::config::{ComposeConfig, ComposeLimits};
use super::{HostCaller, HostResult};

/// The JavaScript that builds the script's API and removes `eval` and the `Function` constructors.
const PRELUDE: &str = include_str!("prelude.js");

/// Why a cell ended before its script did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// What kind of condition: `host_error`, `approval_required`, `outcome_unknown`, `host_call_budget` or `deadline`.
    pub condition: &'static str,
    /// The host error code, when a host call reported one.
    pub code: String,
    /// What the host said, bounded.
    pub detail: String,
}

/// How a cell failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellFailure {
    /// The script threw, or did not compile, or did not settle.
    Script {
        /// The exception's message, bounded.
        message: String,
    },
    /// The interpreter hit one of its limits.
    Limit {
        /// `deadline`, `memory`, `stack`, `script_size`, `output` or `state`.
        limit: &'static str,
        /// What was exceeded.
        detail: String,
    },
    /// A terminal condition ended the cell.
    Terminated(Terminal),
}

/// What one cell produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellOutput {
    /// The script's return value as JSON, when it returned and nothing ended the cell.
    pub result_json: Option<String>,
    /// The `log` text, cut at the output bound.
    pub logs: String,
    /// Whether the text was cut.
    pub logs_truncated: bool,
    /// The `image` references, at most the bound.
    pub images: Vec<String>,
    /// Host calls the script made (the ones that reached the dispatcher, and the one that spent the budget).
    pub host_calls: u32,
    /// How the cell failed, when it did.
    pub failure: Option<CellFailure>,
    /// The `state` after the cell, when the cell succeeded and the state fits.
    pub state_json: Option<String>,
}

/// What the cell is asked to do.
#[derive(Debug, Clone)]
pub struct CellRequest {
    /// The script (the body of an async function).
    pub script: String,
    /// The session's state going in.
    pub state_json: Option<String>,
    /// Wall-clock time.
    pub deadline: Duration,
    /// Host calls allowed.
    pub host_call_budget: u32,
}

struct Shared {
    config: Arc<ComposeConfig>,
    caller: Arc<dyn HostCaller>,
    handle: Handle,
    deadline: Instant,
    budget: u32,
    calls: AtomicU32,
    terminal: Mutex<Option<Terminal>>,
    aborted: AtomicBool,
    logs: Mutex<(String, bool)>,
    images: Mutex<Vec<String>>,
}

impl Shared {
    fn terminal(&self) -> Option<Terminal> {
        self.terminal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Record the first terminal condition; later ones change nothing.
    fn end(&self, condition: &'static str, code: &str, detail: &str) -> Terminal {
        let mut slot = self.terminal.lock().unwrap_or_else(PoisonError::into_inner);
        let terminal = slot.get_or_insert_with(|| Terminal {
            condition,
            code: code.chars().take(64).collect(),
            detail: detail.chars().take(512).collect(),
        });
        self.aborted.store(true, Ordering::SeqCst);
        terminal.clone()
    }

    fn interrupted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst) || Instant::now() >= self.deadline
    }

    /// One host call: the budget, the deadline, the dispatcher, and what the answer means for the cell. The error is
    /// the terminal condition the call throws.
    fn host_call(&self, tool: &str, args_json: &str) -> Result<String, Terminal> {
        if let Some(terminal) = self.terminal() {
            return Err(terminal);
        }
        if Instant::now() >= self.deadline {
            return Err(self.end("deadline", "", "the cell's deadline passed before the call"));
        }
        let made = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if made > self.budget {
            return Err(self.end(
                "host_call_budget",
                "",
                &format!("the cell's budget of {} host calls is spent", self.budget),
            ));
        }
        if args_json.len() > self.config.limits.max_args_bytes {
            return Ok(failed(
                "COMPOSE_LIMIT_EXCEEDED",
                "the call's arguments are over the limit",
                false,
                None,
            ));
        }
        let Ok(args) = serde_json::from_str::<Json>(args_json) else {
            return Ok(failed(
                "VALIDATION_SCHEMA",
                "the call's arguments are not JSON",
                false,
                None,
            ));
        };
        let result = self.handle.block_on(self.caller.call(tool, args));
        match result {
            HostResult::Ok(value) => {
                let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string());
                if text.len() > self.config.limits.max_host_result_bytes {
                    return Ok(failed(
                        "TOOL_OUTPUT_TRUNCATED",
                        "the result is over the limit the script is handed",
                        false,
                        None,
                    ));
                }
                Ok(json!({ "ok": true, "value": value }).to_string())
            }
            HostResult::Failed {
                code,
                message,
                retryable,
                details,
            } => {
                if self.config.is_terminal(&code) {
                    return Err(self.end("host_error", &code, &message));
                }
                Ok(failed(&code, &message, retryable, details))
            }
            HostResult::Parked { detail } => {
                Err(self.end("approval_required", "APPROVAL_REQUIRED", &detail))
            }
            HostResult::Unknown { detail } => {
                Err(self.end("outcome_unknown", "EFFECT_UNKNOWN", &detail))
            }
        }
    }

    fn log(&self, line: &str) {
        let cap = self.config.limits.output_text_bytes;
        let mut slot = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.1 {
            return;
        }
        let room = cap.saturating_sub(slot.0.len());
        let needed = line.len() + 1;
        if needed <= room {
            slot.0.push_str(line);
            slot.0.push('\n');
        } else {
            let mut cut = room.min(line.len());
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            slot.0.push_str(&line[..cut]);
            slot.1 = true;
        }
    }

    fn image(&self, reference: &str) {
        let limits = &self.config.limits;
        let mut images = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        if images.len() < limits.output_images && reference.len() <= limits.image_ref_bytes {
            images.push(reference.to_string());
        }
    }
}

fn failed(code: &str, message: &str, retryable: bool, details: Option<Json>) -> String {
    let mut error = json!({ "code": code, "message": message, "retryable": retryable });
    if let Some(details) = details {
        error["details"] = details;
    }
    json!({ "ok": false, "error": error }).to_string()
}

/// Run one cell. Blocks the calling thread, so call it from `spawn_blocking`.
pub fn run_cell(
    config: &Arc<ComposeConfig>,
    caller: Arc<dyn HostCaller>,
    handle: Handle,
    request: &CellRequest,
) -> CellOutput {
    let limits: &ComposeLimits = &config.limits;
    if request.script.len() > limits.max_script_bytes {
        return CellOutput {
            failure: Some(CellFailure::Limit {
                limit: "script_size",
                detail: format!("the script is over {} bytes", limits.max_script_bytes),
            }),
            ..CellOutput::default()
        };
    }
    let shared = Arc::new(Shared {
        config: Arc::clone(config),
        caller,
        handle,
        deadline: Instant::now() + request.deadline,
        budget: request.host_call_budget,
        calls: AtomicU32::new(0),
        terminal: Mutex::new(None),
        aborted: AtomicBool::new(false),
        logs: Mutex::new((String::new(), false)),
        images: Mutex::new(Vec::new()),
    });
    let outcome = interpret(&shared, request);
    let (logs, logs_truncated) = shared
        .logs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let images = shared
        .images
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let host_calls = shared.calls.load(Ordering::SeqCst).min(shared.budget);
    // A terminal condition decides the cell, whatever the script did with the exception it was thrown.
    if let Some(terminal) = shared.terminal() {
        return CellOutput {
            logs,
            logs_truncated,
            images,
            host_calls,
            failure: Some(CellFailure::Terminated(terminal)),
            ..CellOutput::default()
        };
    }
    match outcome {
        Ok((result_json, state_json)) => {
            let mut output = CellOutput {
                logs,
                logs_truncated,
                images,
                host_calls,
                ..CellOutput::default()
            };
            let cap = limits.output_text_bytes;
            match state_json {
                Some(state) if state.len() <= limits.state_max_bytes => {
                    output.state_json = Some(state)
                }
                Some(state) => {
                    output.failure = Some(CellFailure::Limit {
                        limit: "state",
                        detail: format!(
                            "the state is {} bytes, over the {}-byte limit",
                            state.len(),
                            limits.state_max_bytes
                        ),
                    });
                    return output;
                }
                None => {
                    output.failure = Some(CellFailure::Script {
                        message: "the state can not be serialized as JSON".to_string(),
                    });
                    return output;
                }
            }
            output.result_json = Some(match result_json {
                Some(text) if text.len() > cap => {
                    output.logs_truncated = true;
                    let mut cut = cap;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text[..cut].to_string()
                }
                Some(text) => text,
                None => "null".to_string(),
            });
            output
        }
        Err(failure) => CellOutput {
            logs,
            logs_truncated,
            images,
            host_calls,
            failure: Some(failure),
            ..CellOutput::default()
        },
    }
}

/// The interpreter: returns the result JSON and the state JSON, or the failure.
fn interpret(
    shared: &Arc<Shared>,
    request: &CellRequest,
) -> Result<(Option<String>, Option<String>), CellFailure> {
    let limits = &shared.config.limits;
    let script_error = |message: String| CellFailure::Script { message };
    let runtime = Runtime::new()
        .map_err(|error| script_error(format!("the interpreter could not start: {error}")))?;
    runtime.set_memory_limit(limits.memory_bytes);
    runtime.set_max_stack_size(limits.stack_bytes);
    let for_interrupt = Arc::clone(shared);
    runtime.set_interrupt_handler(Some(Box::new(move || for_interrupt.interrupted())));
    // The built-ins a script needs and none that reach outside: no `Proxy` (nothing here needs interception), no
    // `WeakRef` and no `Performance` (a clock and a collector's behaviour are things a script should not observe).
    let context = rquickjs::Context::custom::<(
        intrinsic::Date,
        intrinsic::Eval,
        intrinsic::RegExpCompiler,
        intrinsic::RegExp,
        intrinsic::Json,
        intrinsic::MapSet,
        intrinsic::TypedArrays,
        intrinsic::Promise,
    )>(&runtime)
    .map_err(|error| script_error(format!("the interpreter could not start: {error}")))?;

    let state_in = request.state_json.clone().unwrap_or_default();
    let wrapped = format!(
        "(async function () {{ \"use strict\";\n{}\n}})()",
        request.script
    );
    context.with(
        |ctx| -> Result<(Option<String>, Option<String>), CellFailure> {
            let classify = |ctx: &Ctx<'_>, error: rquickjs::Error| -> CellFailure {
                classify_error(shared, ctx, &error)
            };
            let api = install(shared, &ctx).map_err(|error| classify(&ctx, error))?;
            let begin: Function = api.get("begin").map_err(|error| classify(&ctx, error))?;
            let finish: Function = api.get("finish").map_err(|error| classify(&ctx, error))?;
            begin
                .call::<_, ()>((state_in,))
                .map_err(|error| classify(&ctx, error))?;

            let promise: Promise = ctx.eval(wrapped).map_err(|error| classify(&ctx, error))?;
            let value: Value = promise.finish().map_err(|error| classify(&ctx, error))?;
            let result = if value.is_undefined() {
                None
            } else {
                ctx.json_stringify(value)
                    .map_err(|error| classify(&ctx, error))?
                    .map(|text| text.to_string())
                    .transpose()
                    .map_err(|error| classify(&ctx, error))?
            };
            let state: Option<String> = finish.call(()).map_err(|error| classify(&ctx, error))?;
            Ok((result, state))
        },
    )
}

/// Hand the prelude its three native functions and take back the API it returns.
fn install<'js>(shared: &Arc<Shared>, ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
    let natives = Object::new(ctx.clone())?;
    let for_call = Arc::clone(shared);
    natives.set(
        "hostCall",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, tool: String, args: String| -> rquickjs::Result<String> {
                for_call.host_call(&tool, &args).map_err(|terminal| {
                    Exception::throw_message(
                        &ctx,
                        &format!("terminal condition: {}", terminal.condition),
                    )
                })
            },
        )?,
    )?;
    let for_log = Arc::clone(shared);
    natives.set(
        "hostLog",
        Function::new(ctx.clone(), move |line: String| {
            for_log.log(&line);
        })?,
    )?;
    let for_image = Arc::clone(shared);
    natives.set(
        "hostImage",
        Function::new(ctx.clone(), move |reference: String| {
            for_image.image(&reference);
        })?,
    )?;
    let factory: Function = ctx.eval(PRELUDE)?;
    factory.call((natives,))
}

/// What a failed evaluation means. A terminal condition, the deadline and the interpreter's own limits are told apart
/// from the script's mistakes by what the worker recorded, then by the exception's own words.
fn classify_error(shared: &Arc<Shared>, ctx: &Ctx<'_>, error: &rquickjs::Error) -> CellFailure {
    if let Some(terminal) = shared.terminal() {
        return CellFailure::Terminated(terminal);
    }
    let message = match error {
        rquickjs::Error::Exception => describe_exception(ctx),
        rquickjs::Error::WouldBlock => return CellFailure::Script {
            message:
                "the script did not finish: it is waiting for something that nothing can resolve"
                    .to_string(),
        },
        other => other.to_string(),
    };
    let lower = message.to_lowercase();
    if Instant::now() >= shared.deadline || lower.contains("interrupted") {
        return CellFailure::Limit {
            limit: "deadline",
            detail: "the cell ran past its deadline".to_string(),
        };
    }
    if lower.contains("out of memory")
        || lower.contains("string too long")
        || lower.contains("invalid string length")
        || lower.contains("allocation failed")
    {
        return CellFailure::Limit {
            limit: "memory",
            detail: format!(
                "the script used more than {} bytes",
                shared.config.limits.memory_bytes
            ),
        };
    }
    if lower.contains("stack overflow") || lower.contains("maximum call stack") {
        return CellFailure::Limit {
            limit: "stack",
            detail: "the script nested deeper than the stack allows".to_string(),
        };
    }
    CellFailure::Script { message }
}

fn describe_exception(ctx: &Ctx<'_>) -> String {
    let caught = ctx.catch();
    let text = if let Some(object) = caught.as_object() {
        let name: String = object.get("name").unwrap_or_default();
        let message: String = object.get("message").unwrap_or_default();
        if name.is_empty() {
            message
        } else {
            format!("{name}: {message}")
        }
    } else if let Some(text) = caught.as_string() {
        text.to_string().unwrap_or_default()
    } else if let Some(number) = caught.as_number() {
        format!("the script threw the number {number}")
    } else if let Some(flag) = caught.as_bool() {
        format!("the script threw {flag}")
    } else if caught.is_null() || caught.is_undefined() {
        // QuickJS raises no error object when it runs out of memory while building one: the exception carries no value.
        // That is, in practice, the heap limit (or a script that threw a bare null, which this reports the same way).
        "the interpreter ran out of memory while raising an error".to_string()
    } else {
        "the script threw a value that is not an error".to_string()
    };
    text.chars().take(1024).collect()
}
