//! Continuum: the Snowflake bytecode harness and its small local shell.

use anyhow::{Context, Result};
use persistent_lisp_harness::snowflake::effects;
use persistent_lisp_harness::snowflake::image::{ImageError, ImageStore};
use persistent_lisp_harness::snowflake::runtime::{Command, Runtime, RuntimeHandle};
use persistent_lisp_harness::snowflake::world::{Agent, TranscriptEntry, World};
use std::collections::VecDeque;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

static LOGS: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(1_000)));
static CHAT: LazyLock<Mutex<Vec<ChatEntry>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static ACTIVITY: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new("Starting…".into()));
static REASONING: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
static GENERATION: AtomicU64 = AtomicU64::new(1);
static CHANGED: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static HTTP_TOKEN: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().to_string());

struct ChatEntry {
    role: &'static str,
    text: String,
    timestamp: String,
}

fn publish() {
    GENERATION.fetch_add(1, Ordering::Release);
    CHANGED.notify_waiters();
}

fn log(message: impl Into<String>) {
    let message = message.into();
    println!("{message}");
    let mut logs = LOGS.lock().unwrap();
    if logs.len() == 1_000 {
        logs.pop_front();
    }
    logs.push_back(message);
    publish();
}

fn chat_at(role: &'static str, text: impl Into<String>, timestamp: String) {
    CHAT.lock().unwrap().push(ChatEntry {
        role,
        text: text.into(),
        timestamp,
    });
    publish();
}

fn chat(role: &'static str, text: impl Into<String>) {
    chat_at(role, text, chrono::Utc::now().to_rfc3339());
}

fn thinking(message: &str) {
    let summary = message.lines().next().unwrap_or(message);
    *ACTIVITY.lock().unwrap() = summary.into();
    if message.starts_with("Model request started") {
        REASONING.lock().unwrap().clear();
    } else if message.starts_with("Reasoning after ")
        || message.starts_with("The provider returned no visible reasoning")
    {
        *REASONING.lock().unwrap() = message.into();
    }
    log(format!("[thinking] {summary}"));
}

fn observe(agent: &str, entry: &TranscriptEntry, replied: bool) {
    *ACTIVITY.lock().unwrap() = format!("{agent} completed: {}", entry.source);
    log(format!("[{agent}] {} => {}", entry.source, entry.result));
    if replied {
        chat("agent", entry.result.clone());
    }
}

fn seed_history(world: &World) {
    let mut history = Vec::new();
    for (id, message) in &world.state.messages {
        history.push((
            (id.0, 1, 0),
            "user",
            message.text.clone(),
            message.created_at.clone(),
        ));
        if let (Some(reply), Some(at), Some((after, order))) =
            (&message.reply, &message.reply_at, message.reply_order)
        {
            history.push(((after, 0, order), "agent", reply.clone(), at.clone()));
        }
    }
    history.sort_by_key(|entry| entry.0);
    for (_, role, text, timestamp) in history {
        chat_at(role, text, timestamp);
    }
    for agent in &world.state.agents {
        for entry in &agent.transcript {
            log(format!(
                "[{}] {} => {}",
                agent.name, entry.source, entry.result
            ));
        }
    }
}

fn root_instructions() -> String {
    "You are Continuum, a persistent autonomous agent inhabiting a Lisp bytecode machine. \
     Work indefinitely on purposeful tasks, inspect relevant evidence before guessing, and build useful \
     definitions without repeatedly reading your own logs or status. Answer every pending human message \
     with (reply ID TEXT). Effects may be nested inside ordinary Lisp expressions."
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
        if self.0.send(Command::HumanMessage(message)).is_err() {
            return false;
        }
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

fn format_chat(entry: &ChatEntry) -> String {
    format!(
        r#"<div class="msg {}"><div>{}</div><div class="meta">{}</div></div>"#,
        entry.role,
        escape_html(&entry.text),
        escape_html(&entry.timestamp)
    )
}

fn chat_html() -> String {
    CHAT.lock()
        .unwrap()
        .iter()
        .take(100)
        .map(format_chat)
        .collect()
}

fn chat_batch(after: usize) -> String {
    let chat = CHAT.lock().unwrap();
    let end = after.saturating_add(100).min(chat.len());
    serde_json::json!({
        "html": chat.get(after..end).unwrap_or_default().iter().map(format_chat).collect::<String>(),
        "next": end,
    }).to_string()
}

struct HttpRequest {
    method: String,
    target: String,
    body: Vec<u8>,
    host: String,
    token: Option<String>,
}

struct HttpResponse {
    status: &'static str,
    content_type: &'static str,
    body: String,
}

fn http(status: &'static str, content_type: &'static str, body: impl Into<String>) -> HttpResponse {
    HttpResponse {
        status,
        content_type,
        body: body.into(),
    }
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

async fn read_http(stream: &mut tokio::net::TcpStream) -> io::Result<HttpRequest> {
    let mut request = Vec::new();
    let header_end = loop {
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
        if request.len() >= 72 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
        let mut buffer = [0; 4096];
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete HTTP request",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    };
    if header_end > 72 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP headers too large",
        ));
    }
    let headers = std::str::from_utf8(&request[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP headers"))?;
    let mut lines = headers.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_owned();
    let target = first.next().unwrap_or_default().to_owned();
    if first.next().is_none() || method.is_empty() || target.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid HTTP request line",
        ));
    }
    let mut length = 0;
    let mut host = String::new();
    let mut token = None;
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().unwrap_or(usize::MAX);
        } else if name.eq_ignore_ascii_case("host") {
            host = value.to_owned();
        } else if name.eq_ignore_ascii_case("x-continuum-token") {
            token = Some(value.to_owned());
        }
    }
    if length > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP body too large",
        ));
    }
    while request.len() < header_end + length {
        let mut buffer = [0; 4096];
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete HTTP body",
            ));
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(HttpRequest {
        method,
        target,
        host,
        token,
        body: request[header_end..header_end + length].to_vec(),
    })
}

async fn events(seen: u64, stream: &tokio::net::TcpStream) -> Option<String> {
    loop {
        let changed = CHANGED.notified();
        let generation = GENERATION.load(Ordering::Acquire);
        if generation > seen {
            return Some(generation.to_string());
        }
        let mut byte = [0];
        tokio::select! {
            () = changed => {}
            _ = tokio::time::sleep(Duration::from_secs(15)) =>
                return Some(GENERATION.load(Ordering::Acquire).to_string()),
            _ = stream.peek(&mut byte) => return None,
        }
    }
}

fn host_allowed(host: &str, address: &str) -> bool {
    host == address
        || address
            .strip_prefix("127.0.0.1:")
            .is_some_and(|port| host == format!("localhost:{port}"))
}

async fn route(
    request: &HttpRequest,
    stream: &tokio::net::TcpStream,
    intervention: &Intervention,
    address: &str,
) -> Option<HttpResponse> {
    if !host_allowed(&request.host, address) {
        return Some(http(
            "421 Misdirected Request",
            "text/plain",
            "invalid host",
        ));
    }
    let path = request.target.split('?').next().unwrap_or(&request.target);
    if request.method == "POST" {
        let form_token = url::form_urlencoded::parse(&request.body)
            .find_map(|(name, value)| (name == "token").then(|| value.into_owned()));
        if request.token.as_deref() != Some(HTTP_TOKEN.as_str())
            && form_token.as_deref() != Some(HTTP_TOKEN.as_str())
        {
            return Some(http("403 Forbidden", "text/plain", "invalid request token"));
        }
    }
    if request.method == "GET" && path == "/events" {
        let seen = request
            .target
            .split_once('?')
            .map(|(_, query)| query)
            .into_iter()
            .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
            .find_map(|(name, value)| (name == "since").then(|| value.parse().ok()).flatten())
            .unwrap_or(0);
        return events(seen, stream)
            .await
            .map(|body| http("200 OK", "text/plain", body));
    }
    if request.method == "GET" && path == "/chat/history" {
        let after = request
            .target
            .split_once('?')
            .map(|(_, query)| query)
            .into_iter()
            .flat_map(|query| url::form_urlencoded::parse(query.as_bytes()))
            .find_map(|(name, value)| (name == "after").then(|| value.parse().ok()).flatten());
        if let Some(after) = after {
            return Some(http("200 OK", "application/json", chat_batch(after)));
        }
    }
    Some(match (request.method.as_str(), path) {
        ("GET", "/" | "/thoughts") => http(
            "200 OK",
            "text/html; charset=utf-8",
            include_str!("../web/thoughts.html"),
        ),
        ("GET", "/chat") => http(
            "200 OK",
            "text/html; charset=utf-8",
            include_str!("../web/chat.html").replace("__CONTINUUM_TOKEN__", &HTTP_TOKEN),
        ),
        ("GET", "/thoughts.json") => http(
            "200 OK",
            "application/json",
            serde_json::to_string(&*LOGS.lock().unwrap()).unwrap(),
        ),
        ("GET", "/activity.json") => http(
            "200 OK",
            "application/json",
            serde_json::json!({
                "status": &*ACTIVITY.lock().unwrap(), "reasoning": &*REASONING.lock().unwrap()
            })
            .to_string(),
        ),
        ("GET", "/chat/history") => http("200 OK", "text/html; charset=utf-8", chat_html()),
        ("POST", "/chat/send") => {
            let message = url::form_urlencoded::parse(&request.body)
                .find_map(|(name, value)| (name == "message").then(|| value.into_owned()))
                .unwrap_or_default();
            if message.is_empty() {
                http("400 Bad Request", "text/plain", "message is required")
            } else if intervention.submit(message) {
                http("202 Accepted", "text/plain", "accepted")
            } else {
                http("503 Service Unavailable", "text/plain", "runtime stopped")
            }
        }
        ("POST", path) if control(&intervention.0, path) => {
            http("202 Accepted", "text/plain", "accepted")
        }
        _ => http("404 Not Found", "text/plain", "not found"),
    })
}

async fn serve_http(
    mut stream: tokio::net::TcpStream,
    intervention: Intervention,
    address: Arc<String>,
) {
    let response = match tokio::time::timeout(Duration::from_secs(5), read_http(&mut stream)).await
    {
        Ok(Ok(request)) => route(&request, &stream, &intervention, &address).await,
        Ok(Err(_)) => Some(http("400 Bad Request", "text/plain", "bad request")),
        Err(_) => Some(http("408 Request Timeout", "text/plain", "request timeout")),
    };
    let Some(response) = response else { return };
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    );
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(response.body.as_bytes()).await?;
        stream.shutdown().await
    })
    .await;
}

async fn start_http(intervention: Intervention) -> Result<()> {
    let address = std::env::var("CONTINUUM_HTTP_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .context("HTTP listen failed")?;
    log(format!("[http] listening on http://{address}"));
    let address = Arc::new(address);
    let permits = Arc::new(tokio::sync::Semaphore::new(64));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let intervention = intervention.clone();
            let address = address.clone();
            tokio::spawn(async move {
                let _permit = permit;
                serve_http(stream, intervention, address).await;
            });
        }
    });
    Ok(())
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

    let thinking: effects::ThinkingObserver = Arc::new(thinking);
    let mut runtime = Runtime::with_starter(world, ImageStore::new(&directory), move |effect| {
        effects::start_observed(effect, thinking.clone())
    });
    let handle = runtime.handle();
    let snapshot = handle.clone();
    runtime.observe(Arc::new(move |agent, entry, replied| {
        observe(agent, entry, replied);
        let _ = snapshot.send(Command::Snapshot);
    }));
    runtime.observe_humans(Arc::new(|message| {
        chat_at("user", message.text.clone(), message.created_at.clone());
    }));
    let intervention = Intervention(handle);
    start_input(intervention.clone());
    start_http(intervention).await?;
    log(format!(
        "Continuum Snowflake ABI {} — bytecode runtime online",
        persistent_lisp_harness::snowflake::ABI
    ));
    runtime.run().await.context("Snowflake runtime stopped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use persistent_lisp_harness::snowflake::value::MessageId;
    use persistent_lisp_harness::snowflake::world::Message;

    #[test]
    fn recovered_chat_preserves_cross_message_reply_order() {
        CHAT.lock().unwrap().clear();
        let mut world = World::default();
        world.state.next_message = 2;
        world.state.messages.insert(
            MessageId(0),
            Message {
                text: "first".into(),
                created_at: "created-1".into(),
                reply: Some("first reply".into()),
                reply_at: Some("replied-1".into()),
                reply_order: Some((2, 0)),
            },
        );
        world.state.messages.insert(
            MessageId(1),
            Message {
                text: "second".into(),
                created_at: "created-2".into(),
                reply: Some("second reply".into()),
                reply_at: Some("replied-2".into()),
                reply_order: Some((2, 1)),
            },
        );
        seed_history(&world);
        let history: Vec<_> = CHAT
            .lock()
            .unwrap()
            .iter()
            .map(|entry| (entry.role, entry.text.clone()))
            .collect();
        assert_eq!(
            history,
            vec![
                ("user", "first".into()),
                ("user", "second".into()),
                ("agent", "first reply".into()),
                ("agent", "second reply".into()),
            ]
        );
    }
}
