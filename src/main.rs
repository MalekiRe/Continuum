//! Continuum: the Snowflake bytecode harness and its small local shell.

use anyhow::{Context, Result};
use persistent_lisp_harness::snowflake::image::{ImageError, ImageStore};
use persistent_lisp_harness::snowflake::runtime::{Command, Runtime, RuntimeHandle};
use persistent_lisp_harness::snowflake::world::{Agent, TranscriptEntry, World};
use std::collections::VecDeque;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::sync::{Condvar, LazyLock, Mutex};
use std::thread;

static LOGS: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(1_000)));
static CHAT: LazyLock<Mutex<VecDeque<ChatEntry>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(1_000)));
static EVENTS: LazyLock<(Mutex<u64>, Condvar)> = LazyLock::new(|| (Mutex::new(0), Condvar::new()));

struct ChatEntry {
    role: &'static str,
    text: String,
    timestamp: String,
}

fn bounded_push<T>(queue: &mut VecDeque<T>, value: T) {
    if queue.len() == 1_000 {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn publish() {
    let (generation, changed) = &*EVENTS;
    let mut generation = generation.lock().unwrap();
    *generation = generation.wrapping_add(1);
    changed.notify_all();
}

fn log(message: impl Into<String>) {
    let message = message.into();
    println!("{message}");
    bounded_push(&mut LOGS.lock().unwrap(), message);
    publish();
}

fn chat(role: &'static str, text: impl Into<String>) {
    bounded_push(
        &mut CHAT.lock().unwrap(),
        ChatEntry {
            role,
            text: text.into(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    );
    publish();
}

fn observe(agent: &str, entry: &TranscriptEntry) {
    log(format!("[{agent}] {} => {}", entry.source, entry.result));
    if entry.source.contains("(reply ") && !entry.result.starts_with("error:") {
        chat("agent", entry.result.clone());
    }
}

fn seed_history(world: &World) {
    for message in world.state.messages.values() {
        chat("user", message.text.clone());
    }
    for agent in &world.state.agents {
        for entry in &agent.transcript {
            observe(&agent.name, entry);
        }
    }
}

fn root_instructions() -> String {
    "You are Continuum, a persistent autonomous agent inhabiting a Lisp bytecode machine. \
     Work indefinitely, inspect before guessing, build useful definitions, and answer every pending \
     human message with (reply ID TEXT). Effects may be nested inside ordinary Lisp expressions."
        .into()
}

fn load_world(directory: &PathBuf) -> Result<World> {
    match ImageStore::new(directory).load() {
        Ok(world) => {
            log("[image] recovered latest Snowflake image");
            Ok(world)
        }
        Err(ImageError::NotFound) => {
            log("[image] starting a fresh Snowflake world");
            Ok(World::default())
        }
        Err(error) => Err(error).context("Snowflake continuity violation"),
    }
}

#[derive(Clone)]
struct Intervention(RuntimeHandle);

impl Intervention {
    fn submit(&self, message: String) -> bool {
        if matches!(message.as_str(), "!!exit" | "!!quit") {
            return self.0.send(Command::Shutdown).is_ok();
        }
        if self.0.send(Command::HumanMessage(message.clone())).is_err() {
            return false;
        }
        chat("user", message);
        let _ = self.0.send(Command::Snapshot);
        true
    }
}

fn start_input(intervention: Intervention) {
    thread::spawn(move || {
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            let message = line.trim().to_owned();
            if !message.is_empty() && !intervention.submit(message) {
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
    CHAT.lock()
        .unwrap()
        .iter()
        .map(|entry| {
            format!(
                r#"<div class="msg {}"><div>{}</div><div class="meta">{}</div></div>"#,
                entry.role,
                escape_html(&entry.text),
                escape_html(&entry.timestamp)
            )
        })
        .collect()
}

fn content_type(value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes("Content-Type", value).unwrap()
}

fn control(handle: &RuntimeHandle, name: &str) -> bool {
    let command = match name {
        "/control/snapshot" => Command::Snapshot,
        "/control/cancel-lisp" => Command::CancelLisp,
        "/control/cancel-external" => Command::CancelExternal,
        "/control/shutdown" => Command::Shutdown,
        _ => return false,
    };
    handle.send(command).is_ok()
}

fn start_http(intervention: Intervention) {
    thread::spawn(move || {
        let address =
            std::env::var("CONTINUUM_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let server = tiny_http::Server::http(&address).expect("HTTP listen failed");
        log(format!("[http] listening on http://{address}"));
        let thoughts = include_str!("../web/thoughts.html");
        let chat_page = include_str!("../web/chat.html");
        for mut request in server.incoming_requests() {
            let method = request.method().as_str();
            let url = request.url().to_owned();
            if method == "GET" && url.starts_with("/events") {
                let seen = url
                    .split_once("since=")
                    .and_then(|(_, value)| value.parse().ok())
                    .unwrap_or(0);
                thread::spawn(move || {
                    let (generation, changed) = &*EVENTS;
                    let mut generation = generation.lock().unwrap();
                    while *generation <= seen {
                        generation = changed.wait(generation).unwrap();
                    }
                    let response = tiny_http::Response::from_string(generation.to_string())
                        .with_header(content_type("text/plain"));
                    let _ = request.respond(response);
                });
                continue;
            }
            let response = match (method, url.as_str()) {
                ("GET", "/" | "/thoughts") => tiny_http::Response::from_string(thoughts)
                    .with_header(content_type("text/html; charset=utf-8")),
                ("GET", "/chat") => tiny_http::Response::from_string(chat_page)
                    .with_header(content_type("text/html; charset=utf-8")),
                ("GET", "/thoughts.json") => tiny_http::Response::from_string(
                    serde_json::to_string(&*LOGS.lock().unwrap()).unwrap(),
                )
                .with_header(content_type("application/json")),
                ("GET", "/chat/history") => tiny_http::Response::from_string(chat_html())
                    .with_header(content_type("text/html; charset=utf-8")),
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
                        .with_header(content_type("text/html; charset=utf-8"))
                }
                ("POST", path) if control(&intervention.0, path) => {
                    tiny_http::Response::from_string("ok")
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
    let directory = std::env::var_os("CONTINUUM_SNOWFLAKE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/snowflake"));
    let mut world = load_world(&directory)?;
    if world.state.agents.is_empty() {
        world
            .state
            .agents
            .push(Agent::new("Continuum".into(), root_instructions()));
    } else if world.state.agents[0].instructions.is_empty() {
        world.state.agents[0].instructions = root_instructions();
    }
    seed_history(&world);

    let mut runtime = Runtime::new(world, ImageStore::new(&directory));
    runtime.observe(observe);
    let intervention = Intervention(runtime.handle());
    start_input(intervention.clone());
    start_http(intervention);
    log("Continuum Snowflake ABI 2 — bytecode runtime online");
    runtime.run().await.context("Snowflake runtime stopped")
}
