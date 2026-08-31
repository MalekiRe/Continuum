//! Continuum: a model inhabiting a persistent Lisp world.

use anyhow::{Context, Result};
use persistent_lisp_harness::{
    ControlReply, ControlTrigger, Executor, ExecutorConfig, Kernel, LocalModel, OutputSink,
    Scheduler, StateLock, TurnOutcome,
};
use std::collections::VecDeque;
use std::io::{self, BufRead};
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

fn load_kernel(snapshot_directory: &Path) -> Result<Kernel> {
    match Kernel::recover_from_dir(snapshot_directory) {
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
Use definitions to build reusable namespaced tools. Inspect before guessing.
Use (bash "command") as the final action when you want to block until completion. Use bash/start only when you intentionally want a background job, then inspect it with bash/status or wake.
Use (model/call "prompt") only as the final action for a focused model subtask.
Use (agent/call "name" "task") only as the final action to create a child agent.
A child finishes with (agent/return value) as its final action.
Respond to a pending human message with (message/reply "message-id" "text") as the final action.
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

#[derive(Clone)]
struct HumanIntervention {
    tx: tokio::sync::mpsc::UnboundedSender<ControlTrigger>,
    gate: Arc<Mutex<()>>,
}

impl HumanIntervention {
    fn submit(&self, message: String) -> bool {
        let _intervention = self.gate.lock().unwrap();
        add_chat("user", message.clone());
        let trigger = if matches!(message.as_str(), "!!exit" | "!!quit") {
            ControlTrigger::Shutdown
        } else {
            ControlTrigger::Human(message)
        };
        self.tx.send(trigger).is_ok()
    }

    fn shutdown(&self) {
        let _intervention = self.gate.lock().unwrap();
        let _ = self.tx.send(ControlTrigger::Shutdown);
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
                    let _ = request.as_reader().read_to_string(&mut body);
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
    slog("Continuum — persistent context → model Lisp action → result");
    let state_root = std::env::var_os("CONTINUUM_STATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("data").to_path_buf());
    let _state_lock = StateLock::acquire(&state_root).context("acquire Continuum state")?;
    let snapshot_directory = state_root.join("snapshots");
    let mut kernel = load_kernel(&snapshot_directory)?;
    kernel.set_snapshot_directory(&snapshot_directory);
    kernel.set_output_sink(OutputSink::new(|message| {
        slog(message.trim_end_matches('\n'))
    }));
    kernel.set_root_instructions_if_empty(root_instructions());

    let workspace = std::env::var_os("CONTINUUM_AGENT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_root.join("workspace"));
    let executor = Executor::new(ExecutorConfig::with_working_directory(workspace))
        .context("initialize Bash executor")?;
    let scheduler = Scheduler::new(LocalModel::default(), executor);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<ControlReply>();
    let intervention = HumanIntervention {
        tx,
        gate: Arc::new(Mutex::new(())),
    };
    start_input_thread(intervention.clone());
    start_http(intervention.clone());
    let shutdown = intervention.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        shutdown.shutdown();
    });
    tokio::spawn(async move {
        while let Some(reply) = reply_rx.recv().await {
            add_chat("agent", reply.text.clone());
            slog(format!("[control] {}", reply.text));
        }
    });

    let mut next_snapshot = Instant::now() + Duration::from_secs(3600);
    loop {
        if let Err(error) = kernel.check_wake_timers() {
            slog(format!("[wake timers] {error}"));
        }
        if Instant::now() >= next_snapshot {
            match kernel.snapshot() {
                Ok(_) => next_snapshot = Instant::now() + Duration::from_secs(3600),
                Err(error) => {
                    slog(format!("[snapshot] {error}"));
                    next_snapshot = Instant::now() + Duration::from_secs(60);
                }
            }
        }

        let (returned, outcome) = scheduler
            .run_supervised_turn(kernel, &mut rx, &reply_tx)
            .await;
        kernel = returned;
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
            Ok(TurnOutcome::Cancelled { reason, .. }) => {
                slog(format!("[control] cancelled current work: {reason}"));
            }
            Ok(TurnOutcome::Shutdown) => {
                scheduler
                    .cancel_background_jobs()
                    .context("cancel background jobs")?;
                kernel.snapshot().context("final snapshot")?;
                return Ok(());
            }
            Ok(TurnOutcome::Idle) => tokio::time::sleep(Duration::from_millis(50)).await,
            Err(error) => {
                slog(format!("[turn] {}", error));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
