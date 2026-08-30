mod builtins;
pub mod native;
mod traps;
use crate::ids::{FrameId, MessageId, SnapshotId};
use crate::vm::env::{EnvRef, EnvironmentId};
use crate::vm::eval;
use crate::vm::value::Function;
use crate::vm::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WakeEntry {
    pub(crate) wake_at: chrono::DateTime<chrono::Utc>,
    pub(crate) action: String,
    pub(crate) frame_id: FrameId,
}

fn default_next_notice_sequence() -> u64 {
    1
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
    /// Current expression source being evaluated (for source retention).
    #[serde(skip)]
    pub(crate) current_source: Option<String>,
    #[serde(skip)]
    pub(crate) eval_control: eval::EvalControl,
    #[serde(skip)]
    output: crate::output::OutputSink,
    #[serde(default, rename = "lexical_heap", skip_serializing)]
    legacy_lexical_heap: indexmap::IndexMap<EnvironmentId, Vec<indexmap::IndexMap<String, Value>>>,
}

/// An agent frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub(crate) id: FrameId,
    pub(crate) name: String,
    pub(crate) status: FrameStatus,
    #[serde(default, rename = "messages", skip_serializing)]
    legacy_messages: Vec<LegacyMessage>,
    #[serde(default)]
    pub(crate) notice_cursor: u64,
    #[serde(default, skip_serializing)]
    pending_message: Option<String>,
    #[serde(default, skip_serializing)]
    message_queue: Vec<String>,
    pub(crate) state: FrameState,
}

impl Frame {
    pub fn id(&self) -> &FrameId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn status(&self) -> FrameStatus {
        self.status
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum FrameStatus {
    Running,
    Waiting,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct LegacyMessage {
    id: Option<MessageId>,
    text: String,
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
pub enum MessageError {
    #[error("too many unanswered human messages")]
    TooManyPending,
    #[error("unknown or completed message ID: {0}")]
    Unknown(String),
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

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct CompactedContext {
    pub(crate) entries: std::collections::VecDeque<CompactedEntry>,
    pub(crate) omitted_turns: u64,
}

impl<'de> Deserialize<'de> for CompactedContext {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Current {
                entries: std::collections::VecDeque<CompactedEntry>,
                omitted_turns: u64,
            },
            Legacy(String),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Current {
                entries,
                omitted_turns,
            } => Self {
                entries,
                omitted_turns,
            },
            Stored::Legacy(text) if text.is_empty() => Self::default(),
            Stored::Legacy(text) => Self {
                entries: [CompactedEntry {
                    timestamp: "legacy".into(),
                    source: "Earlier context".into(),
                    result: text,
                }]
                .into(),
                omitted_turns: 0,
            },
        })
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameState {
    #[serde(default, rename = "current_continuation", skip_serializing)]
    legacy_continuation: Option<LegacyContinuation>,
    #[serde(default)]
    pub(crate) transcript: Vec<TranscriptEntry>,
    #[serde(default)]
    pub(crate) compacted_context: CompactedContext,
    #[serde(default, deserialize_with = "deserialize_pending_trap")]
    pub(crate) pending_trap: Option<PendingTrap>,
    #[serde(default)]
    pub(crate) instructions: String,
    #[serde(default)]
    pub(crate) context_hooks: Vec<String>,
    #[serde(default)]
    pub(crate) memory: Vec<MemoryEntry>,
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

#[derive(Debug, thiserror::Error)]
pub enum TrapError {
    #[error("no active frame")]
    NoActiveFrame,
    #[error("frame already has a pending external operation")]
    AlreadyPending,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum VmTrap {
    CallModel { prompt: String },
    RunBash { command: String },
    AwaitHuman,
    CallAgent { name: String, request: String },
    ReturnAgent { value: Value },
    Reply { message_id: MessageId, text: String },
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
pub(crate) struct SnapshotConfig {
    pub(crate) snapshot_dir: String,
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
    #[error("snapshot refused while an external operation is pending")]
    Busy,
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

impl From<String> for SnapshotError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl From<&str> for SnapshotError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotEnvelope {
    format_version: u32,
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
            current_source: None,
            eval_control: eval::EvalControl::default(),
            output: crate::output::OutputSink::default(),
            legacy_lexical_heap: indexmap::IndexMap::new(),
            storage: SnapshotConfig::default(),
            next_frame_id: 1,
        };

        kernel.register_tools();

        let root_frame = Frame {
            id: FrameId::new(format!("frame-{}", kernel.next_frame_id)),
            name: "root".into(),
            status: FrameStatus::Running,
            legacy_messages: Vec::new(),
            notice_cursor: 0,
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                legacy_continuation: None,
                transcript: Vec::new(),
                compacted_context: CompactedContext::default(),
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

    pub(crate) fn capture_lexical_env(&self) -> EnvironmentId {
        self.env.current_environment()
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn active_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }

    pub fn environment(&self) -> &EnvRef {
        &self.env
    }

    pub fn set_snapshot_directory(&mut self, directory: impl Into<String>) {
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

    pub fn snapshot_count(&self) -> u64 {
        self.storage.snapshot_count
    }

    pub fn lookup(&self, name: &str) -> Option<&Value> {
        self.env.lookup(name)
    }

    pub fn schedule_wake_at(
        &mut self,
        frame_id: FrameId,
        wake_at: chrono::DateTime<chrono::Utc>,
        action: impl Into<String>,
    ) -> Result<(), ScheduleError> {
        if !self.frames.iter().any(|frame| frame.id == frame_id) {
            return Err(ScheduleError::UnknownFrame(frame_id));
        }
        self.wake_timers.push(WakeEntry {
            wake_at,
            action: action.into(),
            frame_id,
        });
        Ok(())
    }

    pub fn set_root_instructions_if_empty(&mut self, instructions: String) {
        if let Some(root) = self.frames.first_mut()
            && root.state.instructions.is_empty()
        {
            root.state.instructions = instructions;
        }
    }

    pub fn set_output_sink(&mut self, sink: crate::output::OutputSink) {
        self.output = sink;
    }

    pub(crate) fn write_output(&self, text: &str) {
        self.output.write(text);
    }

    pub fn eval_interrupt_handle(&self) -> eval::EvalInterruptHandle {
        self.eval_control.interrupt_handle()
    }

    pub fn eval(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        let checkpoint = (
            self.env.clone(),
            self.frames.clone(),
            self.wake_timers.clone(),
            self.next_frame_id,
        );
        let previous_source = self.current_source.replace(source.to_string());
        let evaluation = self.eval_control.begin();
        let result = evaluation.finish(eval::eval(source, self));
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
    pub(crate) fn current_form_is(&self, expected: &str) -> bool {
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
    pub fn spawn_subagent(&mut self, name: &str, request: &str) -> FrameId {
        let id = FrameId::new(format!("frame-{}", self.next_frame_id));
        self.next_frame_id += 1;
        if let Some(parent) = self.frames.last_mut() {
            parent.status = FrameStatus::Waiting;
        }
        self.frames.push(Frame {
            id: id.clone(),
            name: name.to_string(),
            status: FrameStatus::Running,
            legacy_messages: Vec::new(),
            notice_cursor: self.next_notice_sequence.saturating_sub(1),
            pending_message: None,
            message_queue: Vec::new(),
            state: FrameState {
                legacy_continuation: None,
                transcript: Vec::new(),
                compacted_context: CompactedContext::default(),
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
    pub(crate) fn return_from_subagent(&mut self) {
        self.frames.pop();
        if let Some(parent) = self.frames.last_mut() {
            parent.status = FrameStatus::Running;
        }
    }

    /// Deliver a human message as an interrupt to the current frame.
    fn push_notice(
        &mut self,
        id: Option<MessageId>,
        text: String,
        target_frames: Vec<FrameId>,
    ) -> u64 {
        let sequence = self.next_notice_sequence;
        self.next_notice_sequence += 1;
        self.notices.push(StackNotice {
            sequence,
            id,
            text,
            target_frames,
            handled: false,
        });
        sequence
    }

    pub fn human_message(&mut self, text: &str) -> Result<MessageId, MessageError> {
        if self
            .notices
            .iter()
            .filter(|notice| notice.id.is_some() && !notice.handled)
            .count()
            >= 128
        {
            return Err(MessageError::TooManyPending);
        }
        let id = MessageId::new(format!("msg-{}", uuid::Uuid::new_v4()));
        let targets: Vec<_> = self.frames.iter().map(|frame| frame.id.clone()).collect();
        self.push_notice(
            Some(id.clone()),
            text.chars().take(8_000).collect(),
            targets,
        );
        for frame in &mut self.frames {
            frame.status = FrameStatus::Running;
        }
        Ok(id)
    }

    pub fn has_pending_message(&self, id: &MessageId) -> bool {
        self.notices
            .iter()
            .any(|notice| notice.id.as_ref() == Some(id) && !notice.handled)
    }

    pub(crate) fn complete_message(&mut self, id: &MessageId) -> Result<(), MessageError> {
        let notice = self
            .notices
            .iter_mut()
            .find(|notice| notice.id.as_ref() == Some(id) && !notice.handled)
            .ok_or_else(|| MessageError::Unknown(id.to_string()))?;
        notice.handled = true;
        self.retire_notices();
        Ok(())
    }

    pub fn notices_for_frame(&self, frame_id: &FrameId) -> Vec<&StackNotice> {
        let cursor = self
            .frames
            .iter()
            .find(|frame| &frame.id == frame_id)
            .map(|frame| frame.notice_cursor)
            .unwrap_or_default();
        self.notices
            .iter()
            .filter(|notice| {
                notice.target_frames.iter().any(|target| target == frame_id)
                    && (notice.sequence > cursor || (notice.id.is_some() && !notice.handled))
            })
            .collect()
    }

    pub(crate) fn mark_notices_seen_through(&mut self, frame_id: &FrameId, through: u64) {
        let latest = self
            .notices
            .iter()
            .filter(|notice| {
                notice.sequence <= through
                    && notice.target_frames.iter().any(|target| target == frame_id)
            })
            .map(|notice| notice.sequence)
            .max();
        if let Some(latest) = latest
            && let Some(frame) = self.frames.iter_mut().find(|frame| &frame.id == frame_id)
        {
            frame.notice_cursor = frame.notice_cursor.max(latest);
        }
        self.retire_notices();
    }

    fn retire_notices(&mut self) {
        self.notices.retain(|notice| {
            let seen_by_all_remaining_targets = notice.target_frames.iter().all(|target| {
                self.frames
                    .iter()
                    .find(|frame| frame.id == *target)
                    .is_none_or(|frame| frame.notice_cursor >= notice.sequence)
            });
            !seen_by_all_remaining_targets || (notice.id.is_some() && !notice.handled)
        });
    }

    fn migrate_legacy_lexical_heap(&mut self) {
        if self.legacy_lexical_heap.is_empty() {
            return;
        }
        let legacy = std::mem::take(&mut self.legacy_lexical_heap);
        let mut remap = indexmap::IndexMap::new();
        for (old, frames) in legacy {
            remap.insert(old, self.env.import_legacy_frames(frames));
        }

        fn visit(value: &mut Value, remap: &indexmap::IndexMap<EnvironmentId, EnvironmentId>) {
            match value {
                Value::Function(Function::Interpreted { env_id, body, .. }) => {
                    if let Some(replacement) = remap.get(env_id) {
                        *env_id = *replacement;
                    }
                    for value in body {
                        visit(value, remap);
                    }
                }
                Value::List(values) | Value::Vector(values) => {
                    for value in values {
                        visit(value, remap);
                    }
                }
                Value::Map(values) => {
                    let old = std::mem::take(values);
                    for (mut key, mut value) in old {
                        visit(&mut key, remap);
                        visit(&mut value, remap);
                        values.insert(key, value);
                    }
                }
                Value::Macro(crate::vm::value::Macro::SyntaxRules { rules, .. }) => {
                    for (pattern, template) in rules {
                        for value in pattern {
                            visit(value, remap);
                        }
                        visit(template, remap);
                    }
                }
                Value::Tagged { fields, .. } => {
                    for value in fields {
                        visit(value, remap);
                    }
                }
                _ => {}
            }
        }

        for namespace in std::sync::Arc::make_mut(&mut self.env.namespaces).values_mut() {
            for value in namespace.bindings.values_mut() {
                visit(value, &remap);
            }
            for history in namespace.history.values_mut() {
                for record in history {
                    visit(&mut record.value, &remap);
                }
            }
        }
        for value in self.env.lexical.cells.values_mut() {
            visit(value, &remap);
        }
        for frame in &mut self.frames {
            if let Some(PendingTrap {
                operation: VmTrap::ReturnAgent { value },
                ..
            }) = frame.state.pending_trap.as_mut()
            {
                visit(value, &remap);
            }
        }
    }

    fn migrate_legacy_messages(&mut self) {
        fn convert(text: String) -> LegacyMessage {
            if let Some(rest) = text.strip_prefix("Human message [")
                && let Some((id, body)) = rest.split_once("]: ")
            {
                return LegacyMessage {
                    id: Some(MessageId::new(id)),
                    text: body.into(),
                };
            }
            LegacyMessage { id: None, text }
        }

        let mut migrated: Vec<(LegacyMessage, Vec<FrameId>)> = Vec::new();
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
            let mut messages = std::mem::take(&mut frame.legacy_messages);
            if let Some(text) = frame.pending_message.take() {
                messages.push(convert(text));
            }
            for text in std::mem::take(&mut frame.message_queue) {
                if !text.starts_with("(system/HumanMessage ") {
                    messages.push(LegacyMessage { id: None, text });
                }
            }
            for message in messages {
                if let Some((_, targets)) = migrated.iter_mut().find(|(existing, _)| {
                    existing.id == message.id && existing.text == message.text
                }) {
                    if !targets.contains(&frame.id) {
                        targets.push(frame.id.clone());
                    }
                } else {
                    migrated.push((message, vec![frame.id.clone()]));
                }
            }
        }
        for (message, targets) in migrated {
            if !self
                .notices
                .iter()
                .any(|notice| notice.id == message.id && notice.text == message.text)
            {
                self.push_notice(message.id, message.text, targets);
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
                frame.status = FrameStatus::Running;
                self.push_notice(None, entry.action.clone(), vec![entry.frame_id.clone()]);
            }
        }
        fired.len()
    }

    fn collect_lexical_arena(&mut self) {
        fn visit(value: &Value, environments: &mut HashSet<EnvironmentId>) {
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

        let mut reachable = HashSet::from([EnvironmentId::ROOT, self.env.current_environment()]);
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

    pub fn snapshot(&mut self) -> Result<SnapshotInfo, SnapshotError> {
        if self
            .frames
            .iter()
            .any(|frame| frame.state.pending_trap.is_some())
        {
            return Err(SnapshotError::Busy);
        }
        let now = chrono::Utc::now();
        let timestamp = now.to_rfc3339();
        let id = SnapshotId::new(format!("snap-{}", now.format("%Y%m%d-%H%M%S-%6f")));

        let mut saved = self.clone();
        saved.collect_lexical_arena();
        saved.storage.snapshot_count += 1;
        let kernel = serde_json::to_value(&saved)
            .map_err(|error| SnapshotError::json("snapshot serialization", error))?;
        let payload = serde_json::to_vec(&kernel)
            .map_err(|error| SnapshotError::json("snapshot payload", error))?;
        let checksum = sha256(&payload);
        let envelope = SnapshotEnvelope {
            format_version: 5,
            id: id.clone(),
            timestamp: timestamp.clone(),
            kernel,
            checksum: checksum.clone(),
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|error| SnapshotError::json("snapshot envelope", error))?;
        let directory = std::path::PathBuf::from(&self.storage.snapshot_dir);
        std::fs::create_dir_all(&directory)
            .map_err(|error| SnapshotError::io("create snapshot directory", error))?;
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

    pub fn recover_from_latest() -> Result<Self, SnapshotError> {
        Self::recover_from_dir("snapshots")
    }

    pub fn recover_from_dir(directory: impl AsRef<std::path::Path>) -> Result<Self, SnapshotError> {
        let directory = directory.as_ref();
        std::fs::create_dir_all(directory)
            .map_err(|error| SnapshotError::io("create snapshot directory", error))?;
        let mut files: Vec<_> = std::fs::read_dir(directory)
            .map_err(|error| SnapshotError::io("read snapshot directory", error))?
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
                    kernel.migrate_legacy_lexical_heap();
                    kernel.register_tools();
                    kernel.current_source = None;
                    kernel.migrate_legacy_messages();
                    let notice = format!(
                        "Restarted from {}; any in-flight external operation was interrupted",
                        path.display()
                    );
                    let targets = kernel.frames.iter().map(|frame| frame.id.clone()).collect();
                    for frame in &mut kernel.frames {
                        // External work is never inferred or re-executed after recovery.
                        frame.state.pending_trap = None;
                        frame.status = FrameStatus::Running;
                    }
                    kernel.push_notice(None, notice, targets);
                    return Ok(kernel);
                }
                Err(error) => failures.push(format!("{}: {}", path.display(), error)),
            }
        }
        Err(SnapshotError::AllInvalid(failures.join("; ")))
    }

    // ---- Registration ----

    pub(crate) fn define_native(
        &mut self,
        qualified_name: &str,
        arity: u32,
        func: fn(&mut Kernel, Vec<Value>) -> Result<Value, crate::vm::value::NativeError>,
    ) {
        self.define_native_with_arity(qualified_name, crate::vm::value::Arity::Exact(arity), func);
    }

    pub(crate) fn define_variadic_native(
        &mut self,
        qualified_name: &str,
        func: fn(&mut Kernel, Vec<Value>) -> Result<Value, crate::vm::value::NativeError>,
    ) {
        self.define_native_with_arity(qualified_name, crate::vm::value::Arity::Variadic, func);
    }

    fn define_native_with_arity(
        &mut self,
        qualified_name: &str,
        arity: crate::vm::value::Arity,
        func: fn(&mut Kernel, Vec<Value>) -> Result<Value, crate::vm::value::NativeError>,
    ) {
        let val = Value::Function(Function::Native {
            name: qualified_name.to_string(),
            arity,
            func,
        });
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

    pub(crate) fn set_trap(&mut self, operation: VmTrap) -> Result<(), TrapError> {
        let source = self.current_source.clone().unwrap_or_default();
        let frame = self.frames.last_mut().ok_or(TrapError::NoActiveFrame)?;
        if frame.state.pending_trap.is_some() {
            return Err(TrapError::AlreadyPending);
        }
        frame.state.pending_trap = Some(PendingTrap { source, operation });
        Ok(())
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

    pub(crate) fn store_source(&mut self, name: &str, source: &str) {
        let qualified = qualify_user_name(name);
        let _ = self.env.store_source(&qualified, source.to_string());
    }

    pub fn has_trap(&self) -> bool {
        self.pending_trap().is_some()
    }

    pub(crate) fn pending_trap(&self) -> Option<PendingTrap> {
        self.frames.last()?.state.pending_trap.clone()
    }

    pub(crate) fn clear_trap(&mut self) {
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
        .map_err(|source| SnapshotError::Io {
            context: format!("sync snapshot directory {}", path.display()),
            source,
        })
}

fn prune_snapshots(directory: &std::path::Path, keep: usize) -> Result<(), SnapshotError> {
    let mut files: Vec<_> = std::fs::read_dir(directory)
        .map_err(|error| SnapshotError::io("read snapshot directory", error))?
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
            .map_err(|error| SnapshotError::io(format!("remove {}", path.display()), error))?;
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

fn recover_snapshot_file(path: &std::path::Path) -> Result<Kernel, SnapshotError> {
    let bytes = std::fs::read(path)
        .map_err(|error| SnapshotError::io(format!("read {}", path.display()), error))?;
    if let Ok(envelope) = serde_json::from_slice::<SnapshotEnvelope>(&bytes) {
        if !matches!(envelope.format_version, 2..=5) {
            return Err(SnapshotError::Invalid(format!(
                "unsupported snapshot format {}",
                envelope.format_version
            )));
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
        return serde_json::from_value(envelope.kernel).map_err(|source| SnapshotError::Json {
            context: "deserialize kernel",
            source,
        });
    }

    // Legacy format used {kernel, env} with checksum in the sidecar metadata.
    let legacy: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| SnapshotError::json("parse snapshot JSON", error))?;
    let kernel_value = legacy
        .get("kernel")
        .cloned()
        .ok_or_else(|| "missing kernel field".to_string())?;
    let meta_path = path.with_extension("meta");
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&meta_path)
            .map_err(|error| SnapshotError::io("read required legacy metadata", error))?,
    )
    .map_err(|error| SnapshotError::json("parse legacy metadata", error))?;
    let expected = meta
        .get("checksum")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "legacy metadata missing checksum".to_string())?;
    let actual = sha256(&bytes);
    if expected != actual {
        return Err("legacy checksum mismatch".into());
    }
    serde_json::from_value(kernel_value).map_err(|source| SnapshotError::Json {
        context: "deserialize legacy kernel",
        source,
    })
}
