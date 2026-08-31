pub mod native;
mod runtime;
use crate::ids::{FrameId, MessageId, SnapshotId};
use crate::vm::env::{BindingOrigin, EnvRef, EnvironmentId};
use crate::vm::eval;
use crate::vm::value::Value;
use serde::{Deserialize, Serialize};

const SNAPSHOT_FORMAT: u32 = 1;
const RUNTIME_REVISION: &str = "tiny-kernel-v1";

fn runtime_fingerprint() -> String {
    sha256(
        format!(
            "{RUNTIME_REVISION}\n{}\n{}",
            native::signature(),
            include_str!("../prelude.lisp")
        )
        .as_bytes(),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WakeEntry {
    pub(crate) wake_at: chrono::DateTime<chrono::Utc>,
    pub(crate) action: String,
    pub(crate) frame_id: FrameId,
}

fn default_next_notice_sequence() -> u64 {
    1
}

#[derive(Debug, Clone)]
struct CurrentForm {
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Current parsed top-level form and exact source (for suspension and retention).
    #[serde(skip)]
    current_form: Option<CurrentForm>,
    #[serde(skip)]
    pub(crate) eval_control: eval::EvalControl,
    #[serde(skip)]
    output: crate::output::OutputSink,
    #[serde(skip)]
    definition_origin: BindingOrigin,
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
        self.compacted_context.render()
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn context_hooks(&self) -> &[String] {
        &self.context_hooks
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
pub(crate) struct CompactedEntry {
    pub(crate) timestamp: String,
    pub(crate) source: String,
    pub(crate) result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CompactedContext {
    pub(crate) entries: std::collections::VecDeque<CompactedEntry>,
    pub(crate) omitted_turns: u64,
}

impl CompactedContext {
    pub(crate) fn rendered_len(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| entry.timestamp.len() + entry.source.len() + entry.result.len() + 10)
            .sum()
    }

    pub(crate) fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut output = String::new();
        if self.omitted_turns > 0 {
            let _ = writeln!(output, "[{} older turns omitted]", self.omitted_turns);
        }
        for entry in &self.entries {
            let _ = writeln!(
                output,
                "[{}] {} => {}",
                entry.timestamp, entry.source, entry.result
            );
        }
        output
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FrameState {
    #[serde(default)]
    pub(crate) transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub(crate) compacted_context: CompactedContext,
    #[serde(default)]
    pub(crate) instructions: String,
    #[serde(default)]
    pub(crate) context_hooks: Vec<String>,
    #[serde(default)]
    pub(crate) memory: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VmTrap {
    CallModel { prompt: String },
    RunBash { command: String },
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
    format_version: u32,
    runtime_fingerprint: String,
    id: SnapshotId,
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
            notices: Vec::new(),
            next_notice_sequence: 1,
            current_form: None,
            eval_control: eval::EvalControl::default(),
            output: crate::output::OutputSink::default(),
            definition_origin: BindingOrigin::Agent,
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
        let id = SnapshotId::new(format!(
            "snap-{}-{}",
            now.format("%Y%m%d-%H%M%S-%6f"),
            uuid::Uuid::new_v4()
        ));
        let mut saved = self.clone();
        saved.collect_lexical_arena();
        saved.storage.snapshot_count = saved
            .storage
            .snapshot_count
            .checked_add(1)
            .ok_or_else(|| SnapshotError::Invalid("snapshot counter is exhausted".into()))?;
        let kernel = serde_json::to_value(&saved)
            .map_err(|error| SnapshotError::json("snapshot serialization", error))?;
        let payload = serde_json::to_vec(&kernel)
            .map_err(|error| SnapshotError::json("snapshot payload", error))?;
        let checksum = sha256(&payload);
        let envelope = SnapshotEnvelope {
            format_version: SNAPSHOT_FORMAT,
            runtime_fingerprint: runtime_fingerprint(),
            id: id.clone(),
            timestamp: timestamp.clone(),
            kernel,
            checksum: checksum.clone(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| SnapshotError::json("snapshot envelope", error))?;
        let directory = self.storage.snapshot_dir.clone();
        std::fs::create_dir_all(&directory)
            .map_err(|error| SnapshotError::io("create snapshot directory", error))?;
        atomic_write(&directory.join(format!("snapshot-{id}.json")), &bytes)?;
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
        if let Some(frame) = self.frames.iter_mut().find(|f| &f.id == frame_id) {
            frame.state.transcript.push(TranscriptEntry {
                source: source.to_string(),
                result: result.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
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

fn snapshot_sort_key(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .trim_start_matches("snapshot-")
        .trim_end_matches(".json")
        .to_string()
}

fn recover_snapshot_file(path: &std::path::Path) -> Result<Kernel, SnapshotError> {
    let bytes = std::fs::read(path)
        .map_err(|error| SnapshotError::io(format!("read {}", path.display()), error))?;
    let envelope: SnapshotEnvelope = serde_json::from_slice(&bytes)
        .map_err(|error| SnapshotError::json("parse snapshot envelope", error))?;
    if envelope.format_version != SNAPSHOT_FORMAT {
        return Err(SnapshotError::Invalid(format!(
            "unsupported snapshot format {}",
            envelope.format_version
        )));
    }
    if envelope.runtime_fingerprint != runtime_fingerprint() {
        return Err(SnapshotError::Invalid(
            "runtime fingerprint mismatch".into(),
        ));
    }
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
