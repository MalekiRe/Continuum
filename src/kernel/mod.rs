pub(crate) mod history;
pub mod native;
mod runtime;
use crate::ids::{FrameId, MemoryId, MessageId, SnapshotId};
use crate::vm::env::{BindingOrigin, EnvRef, EnvironmentId};
use crate::vm::eval;
use crate::vm::value::Value;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WakeEntry {
    pub(crate) wake_at: chrono::DateTime<chrono::Utc>,
    pub(crate) action: String,
    pub(crate) frame_id: FrameId,
}

fn default_next_notice_sequence() -> u64 {
    1
}

fn default_next_event_id() -> u64 {
    1
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HookPhase {
    Before,
    After,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HookSpec {
    pub id: String,
    pub target: String,
    pub phase: HookPhase,
    pub function: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DeferredHook {
    pub(crate) target: String,
    pub(crate) arguments: Vec<Value>,
    pub(crate) hooks: Vec<HookSpec>,
}

#[derive(Debug)]
struct EvalTransaction {
    frame_id: Option<FrameId>,
    context_before: IndexMap<String, Option<(usize, ContextEntry)>>,
    memory_before: IndexMap<MemoryId, Option<(usize, MemoryEntry)>>,
    hooks_before: Option<Vec<HookSpec>>,
    wake_len: usize,
    history_len: usize,
    next_event_id: u64,
}

#[derive(Debug, Clone)]
struct CurrentForm {
    source: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Kernel {
    pub(crate) env: EnvRef,
    pub(crate) frames: Vec<Frame>,
    pub(crate) storage: SnapshotConfig,
    pub(crate) next_frame_id: u64,
    pub(crate) wake_timers: Vec<WakeEntry>,
    #[serde(default)]
    notices: Vec<StackNotice>,
    #[serde(default = "default_next_notice_sequence")]
    next_notice_sequence: u64,
    #[serde(default)]
    pub(crate) history: Vec<HistoryEvent>,
    #[serde(default = "default_next_event_id")]
    next_event_id: u64,
    #[serde(default)]
    hooks: Vec<HookSpec>,
    /// Current parsed top-level form and exact source (for suspension and retention).
    #[serde(skip)]
    current_form: Option<CurrentForm>,
    #[serde(skip)]
    pub(crate) eval_control: eval::EvalControl,
    #[serde(skip)]
    output: crate::output::OutputSink,
    #[serde(skip)]
    definition_origin: BindingOrigin,
    #[serde(skip)]
    trap_allowed: bool,
    #[serde(skip)]
    active_hooks: std::collections::HashSet<String>,
    #[serde(skip)]
    deferred_hooks: Vec<DeferredHook>,
    #[serde(skip)]
    eval_transaction: Option<EvalTransaction>,
}

/// An agent frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub(crate) id: FrameId,
    pub(crate) name: String,
    pub(crate) waiting_for_human: bool,
    pub(crate) notice_cursor: u64,
    pub(crate) state: FrameState,
}

impl Frame {
    pub fn id(&self) -> &FrameId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn is_waiting_for_human(&self) -> bool {
        self.waiting_for_human
    }
    pub fn state(&self) -> &FrameState {
        &self.state
    }
}

impl FrameState {
    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }
    pub fn compacted_context(&self) -> String {
        history::render_spine(&self.spine, usize::MAX)
    }
    pub fn spine(&self) -> &[SpineNode] {
        &self.spine
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn context_entries(&self) -> &[ContextEntry] {
        &self.context_entries
    }
    pub fn memory(&self) -> &[MemoryEntry] {
        &self.memory
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackNotice {
    pub sequence: u64,
    pub id: Option<MessageId>,
    pub text: String,
    pub target_frames: Vec<FrameId>,
    pub handled: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("unknown frame: {0}")]
    UnknownFrame(FrameId),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AllocationError {
    #[error("{0} identifier sequence is exhausted")]
    Exhausted(&'static str),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MessageError {
    #[error("too many unanswered human messages")]
    TooManyPending,
    #[error("unknown or completed message ID: {0}")]
    Unknown(MessageId),
    #[error(transparent)]
    Allocation(#[from] AllocationError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoryEvent {
    pub id: u64,
    pub timestamp: String,
    pub frame_id: Option<FrameId>,
    pub kind: String,
    pub text: String,
}

/// A single model action and its evaluated result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub event_id: u64,
    pub source: String,
    pub result: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpineNode {
    pub level: u32,
    pub first_event: u64,
    pub last_event: u64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContextLifetime {
    Next,
    Frame,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextEntry {
    pub key: String,
    pub lifetime: ContextLifetime,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    pub id: MemoryId,
    pub key: String,
    pub value: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryNode {
    pub level: u32,
    pub first: MemoryId,
    pub last: MemoryId,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrameState {
    #[serde(default)]
    pub(crate) transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub(crate) spine: Vec<SpineNode>,
    #[serde(default)]
    pub(crate) instructions: String,
    #[serde(default)]
    pub(crate) context_entries: Vec<ContextEntry>,
    #[serde(default)]
    pub(crate) memory: Vec<MemoryEntry>,
    #[serde(default)]
    pub(crate) memory_index: Vec<MemoryNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VmTrap {
    CallModel { prompt: String },
    RunBash { command: String },
    StartBash { command: String },
    BashStatus { id: crate::ids::JobId },
    BashCancel { id: crate::ids::JobId },
    BashCollect { id: crate::ids::JobId },
    BashList,
    AwaitHuman,
    CallAgent { name: String, request: String },
    ReturnAgent { value: String },
    Reply { message_id: MessageId, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapRequest {
    pub source: String,
    pub operation: VmTrap,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalOutcome {
    Value(Value),
    Trap(TrapRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SnapshotConfig {
    pub(crate) snapshot_dir: std::path::PathBuf,
    pub(crate) snapshot_count: u64,
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
    pub id: SnapshotId,
    pub timestamp: String,
    pub checksum: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    Json {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("no snapshots found")]
    NotFound,
    #[error("all snapshots invalid: {0}")]
    AllInvalid(String),
    #[error("invalid snapshot: {0}")]
    Invalid(String),
}

impl SnapshotError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
    fn json(context: &'static str, source: serde_json::Error) -> Self {
        Self::Json { context, source }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotEnvelope {
    sequence: u64,
    checkpoint_at: String,
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
            notices: Vec::new(),
            next_notice_sequence: 1,
            history: Vec::new(),
            next_event_id: 1,
            hooks: Vec::new(),
            current_form: None,
            eval_control: eval::EvalControl::default(),
            output: crate::output::OutputSink::default(),
            definition_origin: BindingOrigin::Agent,
            trap_allowed: false,
            active_hooks: std::collections::HashSet::new(),
            deferred_hooks: Vec::new(),
            eval_transaction: None,
            storage: SnapshotConfig::default(),
            next_frame_id: 1,
        };

        native::install(&mut kernel);
        kernel.definition_origin = BindingOrigin::Prelude;
        match kernel
            .eval(include_str!("../prelude.lisp"))
            .expect("embedded prelude must evaluate")
        {
            EvalOutcome::Value(_) => {}
            EvalOutcome::Trap(_) => panic!("embedded prelude attempted an external operation"),
        }
        kernel.definition_origin = BindingOrigin::Agent;

        let root_frame = Frame {
            id: FrameId::new(format!("frame-{}", kernel.next_frame_id)),
            name: "root".into(),
            waiting_for_human: false,
            notice_cursor: 0,
            state: FrameState::default(),
        };
        kernel.frames.push(root_frame);
        kernel.next_frame_id += 1;

        kernel
    }

    pub(crate) fn capture_lexical_env(&self) -> EnvironmentId {
        self.env.current_environment()
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn environment(&self) -> &EnvRef {
        &self.env
    }

    pub fn set_snapshot_directory(&mut self, directory: impl Into<std::path::PathBuf>) {
        self.storage.snapshot_dir = directory.into();
    }

    pub fn lexical_arena_counts(&self) -> (usize, usize) {
        (
            self.env.lexical.environments.len(),
            self.env.lexical.cells.len(),
        )
    }

    pub fn wake_timer_count(&self) -> usize {
        self.wake_timers.len()
    }

    pub fn snapshot(&mut self) -> Result<SnapshotInfo, SnapshotError> {
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();
        let previous = self.storage.snapshot_count;
        let sequence = previous
            .checked_add(1)
            .ok_or_else(|| SnapshotError::Invalid("snapshot counter is exhausted".into()))?;
        self.collect_lexical_arena();
        self.storage.snapshot_count = sequence;
        let id = SnapshotId::new(format!("{sequence:020}"));
        let kernel = match serde_json::to_value(&*self) {
            Ok(kernel) => kernel,
            Err(error) => {
                self.storage.snapshot_count = previous;
                return Err(SnapshotError::json("snapshot serialization", error));
            }
        };
        let payload = serde_json::to_vec(&kernel)
            .map_err(|error| SnapshotError::json("snapshot payload", error))?;
        let checksum = sha256(&payload);
        let envelope = SnapshotEnvelope {
            sequence,
            checkpoint_at: timestamp.clone(),
            kernel,
            checksum: checksum.clone(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| SnapshotError::json("snapshot envelope", error))?;
        let directory = self.storage.snapshot_dir.clone();
        let result = (|| {
            std::fs::create_dir_all(&directory)
                .map_err(|error| SnapshotError::io("create snapshot directory", error))?;
            atomic_write(&directory.join(format!("snapshot-{id}.json")), &bytes)?;
            prune_snapshots(&directory, 48)?;
            sync_directory(&directory)?;
            Ok(SnapshotInfo {
                id,
                timestamp,
                checksum,
            })
        })();
        if result.is_err() {
            self.storage.snapshot_count = previous;
        }
        result
    }

    pub fn recover_from_latest() -> Result<Self, SnapshotError> {
        Self::recover_from_dir("snapshots")
    }

    pub fn recover_from_dir(directory: impl AsRef<std::path::Path>) -> Result<Self, SnapshotError> {
        let directory = directory.as_ref();
        let files = snapshot_files(directory)?;
        if files.is_empty() {
            return Err(SnapshotError::NotFound);
        }
        let mut failures = Vec::new();
        for path in files {
            match recover_snapshot_file(&path) {
                Ok(mut kernel) => {
                    kernel.current_form = None;
                    if let Err(error) = kernel.validate_recovered() {
                        failures.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                    let targets = kernel.frames.iter().map(|frame| frame.id.clone()).collect();
                    if let Err(error) = kernel.push_notice(
                        None,
                        format!("Restarted from {}", path.display()),
                        targets,
                    ) {
                        failures.push(format!("{}: {error}", path.display()));
                        continue;
                    }
                    kernel.storage.snapshot_dir = directory.to_path_buf();
                    if let Err(error) = kernel.run_stage("stage/after-restart", Value::Nil) {
                        let _ = kernel
                            .control_notice(format!("stage/after-restart hook failed: {error}"));
                    }
                    return Ok(kernel);
                }
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        Err(SnapshotError::AllInvalid(failures.join("; ")))
    }

    pub fn append_transcript(&mut self, source: &str, result: &str) {
        if let Some(id) = self.frames.last().map(|f| f.id.clone()) {
            self.append_transcript_to(&id, source, result);
        }
    }

    pub(crate) fn append_transcript_to(&mut self, frame_id: &FrameId, source: &str, result: &str) {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let event_id = self.record_event(
            Some(frame_id.clone()),
            "turn",
            format!("{source} => {result}"),
            timestamp.clone(),
        );
        if let Some(frame) = self.frames.iter_mut().find(|f| &f.id == frame_id) {
            frame.state.transcript.push(TranscriptEntry {
                event_id,
                source: source.to_string(),
                result: result.to_string(),
                timestamp,
            });
        }
    }
}
pub(crate) fn qualify_user_name(name: &str) -> String {
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

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    use std::io::Write;
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("file")
    ));
    let mut file = std::fs::File::create(&temporary)
        .map_err(|error| SnapshotError::io(format!("create {}", temporary.display()), error))?;
    file.write_all(bytes)
        .map_err(|error| SnapshotError::io(format!("write {}", temporary.display()), error))?;
    file.sync_all()
        .map_err(|error| SnapshotError::io(format!("sync {}", temporary.display()), error))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        SnapshotError::io(
            format!("rename {} to {}", temporary.display(), path.display()),
            error,
        )
    })?;
    Ok(())
}

fn sync_directory(path: &std::path::Path) -> Result<(), SnapshotError> {
    std::fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            SnapshotError::io(format!("sync snapshot directory {}", path.display()), error)
        })
}

fn snapshot_files(directory: &std::path::Path) -> Result<Vec<std::path::PathBuf>, SnapshotError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SnapshotError::NotFound
        } else {
            SnapshotError::io("read snapshot directory", error)
        }
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| SnapshotError::io("read snapshot directory entry", error))?
            .path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && name.starts_with("snapshot-")
        {
            files.push(path);
        }
    }
    files.sort_by_key(|path| std::cmp::Reverse(snapshot_sort_key(path)));
    Ok(files)
}

fn prune_snapshots(directory: &std::path::Path, keep: usize) -> Result<(), SnapshotError> {
    for path in snapshot_files(directory)?.into_iter().skip(keep) {
        std::fs::remove_file(&path)
            .map_err(|error| SnapshotError::io(format!("remove {}", path.display()), error))?;
    }
    Ok(())
}

fn snapshot_sort_key(path: &std::path::Path) -> u64 {
    path.file_stem()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("snapshot-"))
        .and_then(|sequence| sequence.parse().ok())
        .unwrap_or_default()
}

fn recover_snapshot_file(path: &std::path::Path) -> Result<Kernel, SnapshotError> {
    let bytes = std::fs::read(path)
        .map_err(|error| SnapshotError::io(format!("read {}", path.display()), error))?;
    let envelope: SnapshotEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| SnapshotError::json("parse snapshot envelope", error))?;
    let payload = serde_json::to_vec(&envelope.kernel)
        .map_err(|error| SnapshotError::json("serialize payload for checksum", error))?;
    let actual = sha256(&payload);
    if actual != envelope.checksum {
        return Err(SnapshotError::Invalid(format!(
            "checksum mismatch: expected {}, got {}",
            envelope.checksum, actual
        )));
    }
    serde_json::from_value(envelope.kernel)
        .map_err(|error| SnapshotError::json("deserialize kernel", error))
}
