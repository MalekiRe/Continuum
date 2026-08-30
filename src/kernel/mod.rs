pub mod native;
use crate::vm::env::EnvRef;
use crate::vm::eval;
use crate::vm::value::Function;
use crate::vm::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeEntry {
    pub wake_at: chrono::DateTime<chrono::Utc>,
    pub action: String,
    pub frame_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kernel {
    pub env: EnvRef,
    pub frames: Vec<Frame>,
    pub storage: SnapshotConfig,
    pub next_frame_id: u64,
    pub wake_timers: Vec<WakeEntry>,
    /// Current expression source being evaluated (for source retention).
    #[serde(skip)]
    pub current_source: Option<String>,
}

/// An agent frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub id: String,
    pub name: String,
    pub status: FrameStatus,
    #[serde(default)]
    pub messages: Vec<PendingMessage>,
    #[serde(default, skip_serializing)]
    pending_message: Option<String>,
    #[serde(default, skip_serializing)]
    message_queue: Vec<String>,
    pub state: FrameState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FrameStatus {
    Running,
    Waiting,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMessage {
    pub id: Option<String>,
    pub text: String,
}

/// A single model action and its evaluated result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub source: String,
    pub result: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameState {
    #[serde(default, rename = "current_continuation", skip_serializing)]
    legacy_continuation: Option<LegacyContinuation>,
    #[serde(default)]
    pub transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub compacted_context: String,
    #[serde(default, deserialize_with = "deserialize_pending_trap")]
    pub pending_trap: Option<PendingTrap>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub context_hooks: Vec<String>,
    #[serde(default)]
    pub memory: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyContinuation {
    #[serde(default)]
    saved_source: Option<Vec<Value>>,
}

/// A frame-owned top-level suspension request. Nested suspension is
/// deliberately rejected until the evaluator has a serializable stack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingTrap {
    pub source: String,
    pub operation: VmTrap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VmTrap {
    CallModel { prompt: String },
    RunBash { command: String },
    AwaitHuman,
    CallAgent { name: String, request: String },
    ReturnAgent { value: Value },
    Reply { message_id: String, text: String },
}

fn deserialize_pending_trap<'de, D>(deserializer: D) -> Result<Option<PendingTrap>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(value) = value else { return Ok(None) };
    if let Ok(pending) = serde_json::from_value::<PendingTrap>(value.clone()) {
        return Ok(Some(pending));
    }
    // v2 snapshots stored the operation directly and the source separately.
    let operation = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
    Ok(Some(PendingTrap {
        source: "(resume external operation)".into(),
        operation,
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    pub snapshot_dir: String,
    pub snapshot_count: u64,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            snapshot_dir: "snapshots".into(),
            snapshot_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub id: String,
    pub timestamp: String,
    pub checksum: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotEnvelope {
    format_version: u32,
    id: String,
    timestamp: String,
    kernel: serde_json::Value,
    checksum: String,
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel {
    pub fn new() -> Self {
        let mut kernel = Kernel {
            env: EnvRef::new(),
            frames: Vec::new(),
            wake_timers: Vec::new(),
            current_source: None,
            storage: SnapshotConfig::default(),
            next_frame_id: 1,
        };

        kernel.register_tools();

        let root_frame = Frame {
            id: format!("frame-{}", kernel.next_frame_id),
            name: "root".into(),
            status: FrameStatus::Running,
            messages: Vec::new(),
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                legacy_continuation: None,
                transcript: Vec::new(),
                compacted_context: String::new(),
                pending_trap: None,
                instructions: String::new(),
                context_hooks: Vec::new(),
                memory: Vec::new(),
            },
        };
        kernel.frames.push(root_frame);
        kernel.next_frame_id += 1;

        // Create data directory
        let _ = std::fs::create_dir_all("data");
        let _ = std::fs::create_dir_all("snapshots");

        kernel
    }

    pub fn capture_lexical_env(&self) -> u64 {
        self.env.current_environment()
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        let checkpoint = (
            self.env.clone(),
            self.frames.clone(),
            self.wake_timers.clone(),
            self.next_frame_id,
        );
        let previous_source = self.current_source.replace(source.to_string());
        eval::EVAL_RUNNING.store(true, std::sync::atomic::Ordering::Release);
        let result = eval::eval(source, self);
        eval::EVAL_RUNNING.store(false, std::sync::atomic::Ordering::Release);
        eval::EVAL_INTERRUPTED.store(false, std::sync::atomic::Ordering::Release);
        self.current_source = previous_source;

        if result.is_err() {
            let (env, frames, wake_timers, next_frame_id) = checkpoint;
            self.env = env;
            self.frames = frames;
            self.wake_timers = wake_timers;
            self.next_frame_id = next_frame_id;
        }
        result
    }

    /// True only when the current top-level form has the requested head.
    /// Used to reject nested suspension until continuations are explicit.
    pub fn current_form_is(&self, expected: &str) -> bool {
        let Some(source) = self.current_source.as_deref() else {
            return false;
        };
        let Ok(forms) = crate::vm::reader::read_all(source) else {
            return false;
        };
        if forms.len() != 1 {
            return false;
        }
        matches!(&forms[0], Value::List(items)
            if matches!(items.first(), Some(Value::Symbol(head)) if head == expected))
    }

    /// Create a child frame for a subagent call.
    /// Returns the child's frame ID.
    pub fn spawn_subagent(&mut self, name: &str, request: &str) -> String {
        let id = format!("frame-{}", self.next_frame_id);
        self.next_frame_id += 1;
        if let Some(parent) = self.frames.last_mut() {
            parent.status = FrameStatus::Waiting;
        }
        self.frames.push(Frame {
            id: id.clone(),
            name: name.to_string(),
            status: FrameStatus::Running,
            messages: Vec::new(),
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                legacy_continuation: None,
                transcript: Vec::new(),
                compacted_context: String::new(),
                pending_trap: None,
                instructions: format!(
                    "You are the '{}' subagent. Complete this task and finish with (agent/return value): {}",
                    name, request
                ),
                context_hooks: Vec::new(),
                memory: Vec::new(),
            },
        });
        id
    }

    /// Complete the current subagent frame and return its result to the parent.
    pub fn return_from_subagent(&mut self) {
        self.frames.pop();
        if let Some(parent) = self.frames.last_mut() {
            parent.status = FrameStatus::Running;
        }
    }

    /// Deliver a human message as an interrupt to the current frame.
    pub fn human_message(&mut self, text: &str) -> Result<String, String> {
        if self.frames.iter().any(|frame| {
            frame
                .messages
                .iter()
                .filter(|message| message.id.is_some())
                .count()
                >= 128
        }) {
            return Err("too many unanswered human messages".into());
        }
        let id = format!("msg-{}", uuid::Uuid::new_v4());
        let text: String = text.chars().take(8_000).collect();
        for frame in &mut self.frames {
            frame.messages.push(PendingMessage {
                id: Some(id.clone()),
                text: text.clone(),
            });
            frame.status = FrameStatus::Running;
        }
        Ok(id)
    }

    pub fn has_pending_message(&self, id: &str) -> bool {
        self.frames.last().is_some_and(|frame| {
            frame
                .messages
                .iter()
                .any(|message| message.id.as_deref() == Some(id))
        })
    }

    pub fn complete_message(&mut self, id: &str) {
        for frame in &mut self.frames {
            frame
                .messages
                .retain(|message| message.id.as_deref() != Some(id));
        }
    }

    fn migrate_legacy_messages(&mut self) {
        fn convert(text: String) -> PendingMessage {
            if let Some(rest) = text.strip_prefix("Human message [")
                && let Some((id, body)) = rest.split_once("]: ")
            {
                return PendingMessage {
                    id: Some(id.into()),
                    text: body.into(),
                };
            }
            PendingMessage { id: None, text }
        }
        for frame in &mut self.frames {
            if let Some(legacy) = frame.state.legacy_continuation.take()
                && let Some(pending) = frame.state.pending_trap.as_mut()
                && pending.source == "(resume external operation)"
                && let Some(forms) = legacy.saved_source
            {
                pending.source = forms
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            if let Some(text) = frame.pending_message.take() {
                let message = convert(text);
                if !frame
                    .messages
                    .iter()
                    .any(|existing| existing.text == message.text)
                {
                    frame.messages.push(message);
                }
            }
            for text in std::mem::take(&mut frame.message_queue) {
                if text.starts_with("(system/HumanMessage ") {
                    continue;
                }
                if !frame.messages.iter().any(|message| message.text == text) {
                    frame.messages.push(PendingMessage { id: None, text });
                }
            }
        }
    }

    /// Check and fire scheduled wake timers.
    pub fn check_wake_timers(&mut self) -> usize {
        let now = chrono::Utc::now();
        let mut fired = Vec::new();
        self.wake_timers.retain(|entry| {
            if entry.wake_at <= now {
                fired.push(entry.clone());
                false
            } else {
                true
            }
        });
        for entry in &fired {
            if let Some(frame) = self
                .frames
                .iter_mut()
                .find(|frame| frame.id == entry.frame_id)
            {
                frame.messages.push(PendingMessage {
                    id: None,
                    text: entry.action.clone(),
                });
                frame.status = FrameStatus::Running;
            }
        }
        fired.len()
    }

    fn collect_lexical_arena(&mut self) {
        fn visit(value: &Value, environments: &mut HashSet<u64>) {
            match value {
                Value::Function(Function::Interpreted { env_id, body, .. }) => {
                    environments.insert(*env_id);
                    for value in body {
                        visit(value, environments);
                    }
                }
                Value::List(values) | Value::Vector(values) => {
                    for value in values {
                        visit(value, environments);
                    }
                }
                Value::Map(values) => {
                    for (key, value) in values {
                        visit(key, environments);
                        visit(value, environments);
                    }
                }
                Value::Macro(crate::vm::value::Macro::SyntaxRules { rules, .. }) => {
                    for (pattern, template) in rules {
                        for value in pattern {
                            visit(value, environments);
                        }
                        visit(template, environments);
                    }
                }
                Value::Tagged { fields, .. } => {
                    for value in fields {
                        visit(value, environments);
                    }
                }
                _ => {}
            }
        }

        let mut reachable = HashSet::from([0, self.env.current_environment()]);
        for namespace in self.env.namespaces.values() {
            for value in namespace.bindings.values() {
                visit(value, &mut reachable);
            }
            for history in namespace.history.values() {
                for record in history {
                    visit(&record.value, &mut reachable);
                }
            }
        }
        for frame in &self.frames {
            if let Some(PendingTrap {
                operation: VmTrap::ReturnAgent { value },
                ..
            }) = &frame.state.pending_trap
            {
                visit(value, &mut reachable);
            }
        }

        let mut pending: Vec<_> = reachable.iter().copied().collect();
        let mut scanned = HashSet::new();
        while let Some(id) = pending.pop() {
            if !scanned.insert(id) {
                continue;
            }
            if let Some(environment) = self.env.lexical.environments.get(&id) {
                if let Some(parent) = environment.parent {
                    reachable.insert(parent);
                }
                for cell in environment.bindings.values() {
                    if let Some(value) = self.env.lexical.cells.get(cell) {
                        visit(value, &mut reachable);
                    }
                }
                pending.extend(reachable.difference(&scanned).copied());
            }
        }

        self.env
            .lexical
            .environments
            .retain(|id, _| reachable.contains(id));
        let reachable_cells: HashSet<_> = self
            .env
            .lexical
            .environments
            .values()
            .flat_map(|environment| environment.bindings.values().copied())
            .collect();
        self.env
            .lexical
            .cells
            .retain(|id, _| reachable_cells.contains(id));
    }

    pub fn snapshot(&mut self) -> Result<SnapshotInfo, String> {
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();
        let id = format!("snap-{}", now.format("%Y%m%d-%H%M%S-%6f"));

        let mut saved = self.clone();
        saved.collect_lexical_arena();
        saved.storage.snapshot_count += 1;
        let kernel = serde_json::to_value(&saved)
            .map_err(|error| format!("snapshot serialization: {}", error))?;
        let payload =
            serde_json::to_vec(&kernel).map_err(|error| format!("snapshot payload: {}", error))?;
        let checksum = sha256(&payload);
        let envelope = SnapshotEnvelope {
            format_version: 4,
            id: id.clone(),
            timestamp: timestamp.clone(),
            kernel,
            checksum: checksum.clone(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| format!("snapshot envelope: {}", error))?;
        let directory = std::path::PathBuf::from(&self.storage.snapshot_dir);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("create snapshot directory: {}", error))?;
        atomic_write(&directory.join(format!("snapshot-{}.json", id)), &bytes)?;
        prune_snapshots(&directory, 48)?;
        sync_directory(&directory)?;
        self.storage = saved.storage;
        self.env.lexical = saved.env.lexical;
        Ok(SnapshotInfo {
            id,
            timestamp,
            checksum,
        })
    }

    pub fn recover_from_latest() -> Result<Self, String> {
        Self::recover_from_dir("snapshots")
    }

    pub fn recover_from_dir(directory: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory)
            .map_err(|e| format!("create snapshot directory: {}", e))?;
        let mut files: Vec<_> = std::fs::read_dir(directory)
            .map_err(|e| format!("read snapshot directory: {}", e))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                path.extension().is_some_and(|ext| ext == "json")
                    && ["full-", "inc-", "snapshot-"]
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
            })
            .collect();
        files.sort_by_key(|path| std::cmp::Reverse(snapshot_sort_key(path)));
        if files.is_empty() {
            return Err("no snapshots found".into());
        }

        let mut failures = Vec::new();
        for path in files {
            match recover_snapshot_file(&path) {
                Ok(mut kernel) => {
                    kernel.register_tools();
                    kernel.current_source = None;
                    kernel.migrate_legacy_messages();
                    let notice = format!("Restarted from {}", path.display());
                    for frame in &mut kernel.frames {
                        frame.messages.push(PendingMessage {
                            id: None,
                            text: notice.clone(),
                        });
                        if frame.status == FrameStatus::Waiting
                            && frame.state.pending_trap.is_none()
                        {
                            frame.status = FrameStatus::Running;
                        }
                    }
                    return Ok(kernel);
                }
                Err(error) => failures.push(format!("{}: {}", path.display(), error)),
            }
        }
        Err(format!("all snapshots invalid: {}", failures.join("; ")))
    }

    // ---- Registration ----

    fn define_native(
        &mut self,
        qualified_name: &str,
        arity: u32,
        func: fn(&mut Kernel, Vec<Value>) -> Result<Value, String>,
    ) {
        let val = Value::Function(Function::Native {
            name: qualified_name.to_string(),
            arity,
            func,
        });
        // Qualify unqualified names with "kernel/" so force_define works.
        let full_name = if qualified_name.contains('/') {
            qualified_name.to_string()
        } else {
            format!("kernel/{}", qualified_name)
        };
        self.env.force_define(&full_name, val);
    }

    pub fn inspect_namespace(&self, name: &str) -> Option<Vec<String>> {
        self.env.namespaces.get(name).map(|ns| ns.list_bindings())
    }

    pub fn find_bindings(&self, query: &str) -> Vec<String> {
        let q = query.to_lowercase();
        let mut results = Vec::new();
        for (ns_name, ns) in self.env.namespaces.iter() {
            for binding in ns.list_bindings() {
                let qualified = format!("{}/{}", ns_name, binding);
                if qualified.to_lowercase().contains(&q) {
                    results.push(qualified);
                }
            }
        }
        results.sort();
        results
    }

    pub fn set_trap(&mut self, operation: VmTrap) -> Result<(), String> {
        let source = self.current_source.clone().unwrap_or_default();
        let frame = self
            .frames
            .last_mut()
            .ok_or_else(|| "no active frame".to_string())?;
        if frame.state.pending_trap.is_some() {
            return Err("frame already has a pending trap".into());
        }
        frame.state.pending_trap = Some(PendingTrap { source, operation });
        Ok(())
    }

    pub fn append_transcript(&mut self, source: &str, result: &str) {
        if let Some(id) = self.frames.last().map(|f| f.id.clone()) {
            self.append_transcript_to(&id, source, result);
        }
    }

    pub fn append_transcript_to(&mut self, frame_id: &str, source: &str, result: &str) {
        if let Some(frame) = self.frames.iter_mut().find(|f| f.id == frame_id) {
            frame.state.transcript.push(TranscriptEntry {
                source: source.to_string(),
                result: result.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
            });
        }
    }

    pub fn store_source(&mut self, name: &str, source: &str) {
        let qualified = qualify_user_name(name);
        let _ = self.env.store_source(&qualified, source.to_string());
    }

    pub fn has_trap(&self) -> bool {
        self.pending_trap().is_some()
    }

    pub fn pending_trap(&self) -> Option<PendingTrap> {
        self.frames.last()?.state.pending_trap.clone()
    }

    pub fn clear_trap(&mut self) {
        if let Some(frame) = self.frames.last_mut() {
            frame.state.pending_trap = None;
        }
    }

    pub fn take_trap(&mut self) -> Option<PendingTrap> {
        self.frames.last_mut()?.state.pending_trap.take()
    }
}
fn qualify_user_name(name: &str) -> String {
    if name.contains('/') {
        name.into()
    } else {
        format!("user/{}", name)
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("file")
    ));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|e| format!("create {}: {}", temporary.display(), e))?;
    file.write_all(bytes)
        .map_err(|e| format!("write {}: {}", temporary.display(), e))?;
    file.sync_all()
        .map_err(|e| format!("sync {}: {}", temporary.display(), e))?;
    std::fs::rename(&temporary, path).map_err(|e| {
        format!(
            "rename {} to {}: {}",
            temporary.display(),
            path.display(),
            e
        )
    })?;
    Ok(())
}

fn sync_directory(path: &std::path::Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("sync snapshot directory {}: {}", path.display(), e))
}

fn prune_snapshots(directory: &std::path::Path, keep: usize) -> Result<(), String> {
    let mut files: Vec<_> = std::fs::read_dir(directory)
        .map_err(|error| format!("read snapshot directory: {}", error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            path.extension()
                .is_some_and(|extension| extension == "json")
                && ["full-", "inc-", "snapshot-"]
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    files.sort_by_key(|path| std::cmp::Reverse(snapshot_sort_key(path)));
    for path in files.into_iter().skip(keep) {
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove {}: {}", path.display(), error))?;
        let _ = std::fs::remove_file(path.with_extension("meta"));
    }
    Ok(())
}

fn snapshot_sort_key(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim_start_matches("full-")
        .trim_start_matches("inc-")
        .trim_start_matches("snapshot-")
        .trim_end_matches(".json")
        .to_string()
}

fn recover_snapshot_file(path: &std::path::Path) -> Result<Kernel, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read: {}", e))?;
    if let Ok(envelope) = serde_json::from_slice::<SnapshotEnvelope>(&bytes) {
        if !matches!(envelope.format_version, 2..=4) {
            return Err(format!(
                "unsupported snapshot format {}",
                envelope.format_version
            ));
        }
        let payload = serde_json::to_vec(&envelope.kernel)
            .map_err(|e| format!("serialize payload for checksum: {}", e))?;
        let actual = sha256(&payload);
        if actual != envelope.checksum {
            return Err(format!(
                "checksum mismatch: expected {}, got {}",
                envelope.checksum, actual
            ));
        }
        return serde_json::from_value(envelope.kernel)
            .map_err(|e| format!("deserialize kernel: {}", e));
    }

    // Legacy format used {kernel, env} with checksum in the sidecar metadata.
    let legacy: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse JSON: {}", e))?;
    let kernel_value = legacy
        .get("kernel")
        .cloned()
        .ok_or_else(|| "missing kernel field".to_string())?;
    let meta_path = path.with_extension("meta");
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&meta_path).map_err(|e| format!("legacy metadata required: {}", e))?,
    )
    .map_err(|e| format!("parse legacy metadata: {}", e))?;
    let expected = meta
        .get("checksum")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "legacy metadata missing checksum".to_string())?;
    let actual = sha256(&bytes);
    if expected != actual {
        return Err("legacy checksum mismatch".into());
    }
    serde_json::from_value(kernel_value).map_err(|e| format!("deserialize legacy kernel: {}", e))
}
