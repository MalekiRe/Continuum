//! Continuum: a model inhabiting a persistent Lisp world.

use anyhow::{Context, Result};
use persistent_lisp_harness::{
    EvalInterruptHandle, Executor, ExecutorConfig, Kernel, ModelInterruptHandle, OpenRouterModel,
    OutputSink, Scheduler, TurnOutcome,
};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

static LOG_BUF: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(1000)));
static CHAT_HISTORY: LazyLock<Mutex<VecDeque<ChatEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

struct ChatEntry {
    role: String,
    message: String,
    timestamp: String,
}

fn slog(message: impl AsRef<str>) {
    let message = message.as_ref();
    println!("{}", message);
    let mut log = LOG_BUF.lock().unwrap();
    if log.len() == 1000 {
        log.pop_front();
    }
    log.push_back(message.to_string());
}

fn load_kernel() -> Result<Kernel> {
    match Kernel::recover_from_latest() {
        Ok(kernel) => {
            slog("[kernel] recovered latest valid snapshot");
            Ok(kernel)
        }
        Err(persistent_lisp_harness::SnapshotError::NotFound) => {
            slog("[kernel] fresh start");
            Ok(Kernel::new())
        }
        Err(error) => Err(error).context("continuity violation: snapshots exist but none recover"),
    }
}

fn root_instructions() -> String {
    r#"You are Continuum, a persistent agent inhabiting a live Lisp machine.
Choose exactly one useful Lisp action per turn. Its evaluated result returns in your next context.
Continue taking useful actions indefinitely, including after replying to a human; do not wait for another prompt.
Use definitions to build reusable namespaced tools. Inspect before guessing.
Use (bash "command") only as a top-level form. It runs in your fixed agent workspace.
Use (model/call "prompt") only as a top-level form for a focused model subtask.
Use (agent/call "name" "task") only as a top-level form to create a child agent.
A child finishes with top-level (agent/return value).
Respond to a pending human message with top-level (message/reply "message-id" "text").
Do not emit prose or Markdown: emit one Lisp form."#
        .into()
}

fn add_chat(role: &str, message: impl Into<String>) {
    let mut history = CHAT_HISTORY.lock().unwrap();
    if history.len() == 1_000 {
        history.pop_front();
    }
    history.push_back(ChatEntry {
        role: role.into(),
        message: message.into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    });
}

fn deliver_human(kernel: &mut Kernel, text: String) {
    add_chat("user", text.clone());
    match kernel.human_message(&text) {
        Ok(id) => slog(format!("[human:{}] queued for active frames", id)),
        Err(error) => slog(format!("[human] rejected: {}", error)),
    }
}

#[derive(Clone)]
struct HumanIntervention {
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    executor: Executor,
    model: ModelInterruptHandle,
    eval: EvalInterruptHandle,
    gate: Arc<Mutex<()>>,
}

impl HumanIntervention {
    fn submit(&self, message: String) -> bool {
        let _intervention = self.gate.lock().unwrap();
        self.eval.request_interrupt();
        self.model.request_interrupt();
        if let Err(error) = self.executor.cancel() {
            slog(format!("[executor] cancellation failed: {error}"));
        }
        self.tx.send(message).is_ok()
    }

    fn acknowledge(&self) {
        let _intervention = self.gate.lock().unwrap();
        self.model.clear_pending();
        self.eval.clear_pending();
    }
}

fn start_input_thread(intervention: HumanIntervention) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let line = line.trim().to_string();
            if !line.is_empty() && !intervention.submit(line) {
                break;
            }
        }
    });
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn chat_html() -> String {
    CHAT_HISTORY
        .lock()
        .unwrap()
        .iter()
        .map(|entry| {
            let class = if entry.role == "user" {
                "msg user"
            } else {
                "msg agent"
            };
            format!(
                r#"<div class="{}"><div>{}</div><div class="meta">{}</div></div>"#,
                class,
                escape_html(&entry.message),
                escape_html(&entry.timestamp)
            )
        })
        .collect()
}

fn content_type(value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes("Content-Type", value).unwrap()
}

fn start_http(intervention: HumanIntervention) {
    thread::spawn(move || {
        let address =
            std::env::var("CONTINUUM_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let server = tiny_http::Server::http(&address).expect("HTTP listen failed");
        slog(format!("[http] listening on http://{address}"));
        let thoughts = include_str!("../web/thoughts.html");
        let chat = include_str!("../web/chat.html");
        for mut request in server.incoming_requests() {
            let method = request.method().as_str();
            let url = request.url();
            let response = match (method, url) {
                ("GET", "/" | "/thoughts") => tiny_http::Response::from_string(thoughts)
                    .with_header(content_type("text/html; charset=utf-8")),
                ("GET", "/chat") => tiny_http::Response::from_string(chat)
                    .with_header(content_type("text/html; charset=utf-8")),
                ("GET", "/thoughts.json") => tiny_http::Response::from_string(
                    serde_json::to_string(&*LOG_BUF.lock().unwrap()).unwrap(),
                )
                .with_header(content_type("application/json")),
                ("GET", "/chat/history") => tiny_http::Response::from_string(chat_html())
                    .with_header(content_type("text/html")),
                ("POST", "/chat/send") => {
                    let mut body = String::new();
                    let _ = request
                        .as_reader()
                        .take(64 * 1024)
                        .read_to_string(&mut body);
                    let message = url::form_urlencoded::parse(body.as_bytes())
                        .find_map(|(name, value)| (name == "message").then(|| value.into_owned()))
                        .unwrap_or_default();
                    if !message.is_empty() {
                        intervention.submit(message);
                    }
                    tiny_http::Response::from_string(chat_html())
                        .with_header(content_type("text/html"))
                }
                _ => tiny_http::Response::from_string("not found").with_status_code(404),
            };
            let _ = request.respond(response);
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    slog("Continuum v0.2 — persistent context → model Lisp action → result");
    let mut kernel = load_kernel()?;
    let eval_interrupt = kernel.eval_interrupt_handle();
    kernel.set_output_sink(OutputSink::new(|message| {
        slog(message.trim_end_matches('\n'))
    }));
    kernel.set_root_instructions_if_empty(root_instructions());

    let workspace = std::env::var_os("CONTINUUM_AGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("data/workspace").to_path_buf());
    let executor = Executor::new(ExecutorConfig::with_working_directory(workspace))
        .context("initialize Bash executor")?;
    let scheduler = Scheduler::new(OpenRouterModel::default(), executor.clone());
    let model_interrupt = scheduler.model_interrupt_handle();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let intervention = HumanIntervention {
        tx,
        executor,
        model: model_interrupt,
        eval: eval_interrupt,
        gate: Arc::new(Mutex::new(())),
    };
    start_input_thread(intervention.clone());
    start_http(intervention.clone());

    let mut snapshot_timer = Instant::now();
    loop {
        if let Err(error) = kernel.check_wake_timers() {
            slog(format!("[wake timers] {error}"));
        }
        if snapshot_timer.elapsed() >= Duration::from_secs(3600) {
            if let Err(error) = kernel.snapshot() {
                slog(format!("[snapshot] {}", error));
            }
            snapshot_timer = Instant::now();
        }

        let outcome = tokio::select! {
            biased;
            message = rx.recv() => {
                let Some(message) = message else { return Ok(()) };
                // The selected turn future is dropped before its trap and sticky
                // interruption signals are discarded.
                kernel.discard_pending_operation();
                intervention.acknowledge();
                if matches!(message.as_str(), "!!exit" | "!!quit") {
                    if let Err(error) = kernel.snapshot() {
                        eprintln!("[snapshot] {}", error);
                    }
                    return Ok(());
                }
                deliver_human(&mut kernel, message);
                continue;
            }
            outcome = scheduler.run_turn(&mut kernel) => outcome,
        };
        match outcome {
            Ok(TurnOutcome::Evaluated { source, result, .. }) => {
                slog(format!("[lisp] {} => {}", source, result));
            }
            Ok(TurnOutcome::ToolCompleted { source, result, .. }) => {
                slog(format!("[tool] {} => {}", source, result));
            }
            Ok(TurnOutcome::Spawned { child_id, .. }) => {
                slog(format!("[agent] spawned {}", child_id));
            }
            Ok(TurnOutcome::Returned { result, .. }) => {
                slog(format!("[agent] child returned {}", result));
            }
            Ok(TurnOutcome::Replied { text, .. }) => {
                add_chat("agent", text.clone());
                slog(format!("[agent] {}", text));
            }
            Ok(TurnOutcome::Idle) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(error) => {
                slog(format!("[turn] {}", error));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
