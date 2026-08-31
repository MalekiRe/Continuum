use super::*;
use crate::vm::env::BindingOrigin;
use crate::vm::value::collect_captured_environments;
use std::collections::HashSet;

impl Kernel {
    fn begin_eval_transaction(&mut self) {
        self.env.begin_transaction();
        self.deferred_hooks.clear();
        self.active_hooks.clear();
        self.eval_transaction = Some(EvalTransaction {
            frame_id: self.frames.last().map(|frame| frame.id.clone()),
            context_before: IndexMap::new(),
            memory_before: IndexMap::new(),
            hooks_before: None,
            wake_len: self.wake_timers.len(),
            history_len: self.history.len(),
            next_event_id: self.next_event_id,
        });
    }

    fn commit_eval_transaction(&mut self) {
        self.env.commit_transaction();
        self.eval_transaction = None;
        self.deferred_hooks.clear();
        self.active_hooks.clear();
    }

    fn rollback_eval_transaction(&mut self) {
        self.env.rollback_transaction();
        let Some(transaction) = self.eval_transaction.take() else {
            return;
        };
        if let Some(frame_id) = transaction.frame_id
            && let Some(frame) = self.frames.iter_mut().find(|frame| frame.id == frame_id)
        {
            for key in transaction.context_before.keys() {
                frame
                    .state
                    .context_entries
                    .retain(|entry| &entry.key != key);
            }
            let mut context: Vec<_> = transaction.context_before.into_values().flatten().collect();
            context.sort_by_key(|(index, _)| *index);
            for (index, entry) in context {
                frame
                    .state
                    .context_entries
                    .insert(index.min(frame.state.context_entries.len()), entry);
            }

            for id in transaction.memory_before.keys() {
                frame.state.memory.retain(|entry| &entry.id != id);
            }
            let mut memory: Vec<_> = transaction.memory_before.into_values().flatten().collect();
            memory.sort_by_key(|(index, _)| *index);
            for (index, entry) in memory {
                frame
                    .state
                    .memory
                    .insert(index.min(frame.state.memory.len()), entry);
            }
            history::rebuild_memory_index(&frame.state.memory, &mut frame.state.memory_index);
        }
        if let Some(hooks) = transaction.hooks_before {
            self.hooks = hooks;
        }
        self.wake_timers.truncate(transaction.wake_len);
        self.history.truncate(transaction.history_len);
        self.next_event_id = transaction.next_event_id;
        self.deferred_hooks.clear();
        self.active_hooks.clear();
    }

    fn note_context_before(&mut self, key: &str) {
        let before = self.frames.last().and_then(|frame| {
            frame
                .state
                .context_entries
                .iter()
                .position(|entry| entry.key == key)
                .map(|index| (index, frame.state.context_entries[index].clone()))
        });
        if let Some(transaction) = &mut self.eval_transaction {
            transaction
                .context_before
                .entry(key.into())
                .or_insert(before);
        }
    }

    fn note_memory_before(&mut self, id: &MemoryId) {
        let before = self.frames.last().and_then(|frame| {
            frame
                .state
                .memory
                .iter()
                .position(|entry| &entry.id == id)
                .map(|index| (index, frame.state.memory[index].clone()))
        });
        if let Some(transaction) = &mut self.eval_transaction {
            transaction
                .memory_before
                .entry(id.clone())
                .or_insert(before);
        }
    }

    fn note_hooks_before(&mut self) {
        if let Some(transaction) = &mut self.eval_transaction
            && transaction.hooks_before.is_none()
        {
            transaction.hooks_before = Some(self.hooks.clone());
        }
    }

    pub(crate) fn inject_context(
        &mut self,
        key: String,
        lifetime: ContextLifetime,
        text: String,
    ) -> bool {
        self.note_context_before(&key);
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        if let Some(entry) = frame
            .state
            .context_entries
            .iter_mut()
            .find(|entry| entry.key == key)
        {
            entry.lifetime = lifetime;
            entry.text = text;
        } else {
            frame.state.context_entries.push(ContextEntry {
                key,
                lifetime,
                text,
            });
        }
        true
    }

    pub(crate) fn remove_context(&mut self, key: &str) -> bool {
        self.note_context_before(key);
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        let before = frame.state.context_entries.len();
        frame.state.context_entries.retain(|entry| entry.key != key);
        before != frame.state.context_entries.len()
    }

    pub(crate) fn add_hook(&mut self, spec: HookSpec) {
        self.note_hooks_before();
        if let Some(existing) = self.hooks.iter_mut().find(|hook| hook.id == spec.id) {
            *existing = spec;
        } else {
            self.hooks.push(spec);
        }
    }

    pub(crate) fn remove_hook(&mut self, id: &str) -> bool {
        self.note_hooks_before();
        let before = self.hooks.len();
        self.hooks.retain(|hook| hook.id != id);
        before != self.hooks.len()
    }

    pub(crate) fn list_hooks(&self, target: Option<&str>) -> Vec<HookSpec> {
        self.hooks
            .iter()
            .filter(|hook| target.is_none_or(|target| hook.target == target))
            .cloned()
            .collect()
    }

    pub(crate) fn hooks_for(&self, target: &str, phase: HookPhase) -> Vec<HookSpec> {
        if target == "kernel/trap" || target.starts_with("hook/") || target.starts_with("context/")
        {
            return Vec::new();
        }
        self.hooks
            .iter()
            .filter(|hook| hook.target == target && hook.phase == phase)
            .cloned()
            .collect()
    }

    pub(crate) fn enter_hook(&mut self, id: &str) -> bool {
        self.active_hooks.insert(id.into())
    }

    pub(crate) fn leave_hook(&mut self, id: &str) {
        self.active_hooks.remove(id);
    }

    pub(crate) fn defer_hooks(&mut self, deferred: DeferredHook) {
        self.deferred_hooks.push(deferred);
    }

    pub(crate) fn take_deferred_hooks(&mut self) -> Vec<DeferredHook> {
        std::mem::take(&mut self.deferred_hooks)
    }

    pub(crate) fn clear_deferred_hooks(&mut self) {
        self.deferred_hooks.clear();
    }

    pub(crate) fn run_stage(&mut self, target: &str, value: Value) -> Result<(), eval::EvalError> {
        let target = Value::symbol(target);
        let form = Value::list(vec![
            Value::symbol("hook/run"),
            Value::list(vec![Value::symbol("quote"), target]),
            value,
        ]);
        let source = form.to_string();
        self.eval_value(&source).map(drop)
    }

    pub(crate) fn record_event(
        &mut self,
        frame_id: Option<FrameId>,
        kind: impl Into<String>,
        text: impl Into<String>,
        timestamp: String,
    ) -> u64 {
        let id = self.next_event_id;
        self.next_event_id = id.checked_add(1).expect("history event sequence exhausted");
        self.history.push(HistoryEvent {
            id,
            timestamp,
            frame_id,
            kind: kind.into(),
            text: text.into(),
        });
        id
    }

    pub(crate) fn record_now(
        &mut self,
        frame_id: Option<FrameId>,
        kind: impl Into<String>,
        text: impl Into<String>,
    ) -> u64 {
        self.record_event(frame_id, kind, text, chrono::Utc::now().to_rfc3339())
    }

    pub(crate) fn remember(&mut self, key: String, value: String) -> MemoryId {
        let frame_id = self.frames.last().map(|frame| frame.id.clone());
        let now = chrono::Utc::now().to_rfc3339();
        let existing = self.frames.last().and_then(|frame| {
            frame
                .state
                .memory
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| entry.id.clone())
        });
        let id =
            existing.unwrap_or_else(|| MemoryId::new(format!("memory-{}", uuid::Uuid::new_v4())));
        self.note_memory_before(&id);
        if let Some(entry) = self
            .frames
            .last_mut()
            .and_then(|frame| frame.state.memory.iter_mut().find(|entry| entry.id == id))
        {
            entry.value = value.clone();
            entry.updated_at = now;
        } else if let Some(frame) = self.frames.last_mut() {
            frame.state.memory.push(MemoryEntry {
                id: id.clone(),
                key: key.clone(),
                value: value.clone(),
                updated_at: now,
            });
        }
        if let Some(frame) = self.frames.last_mut() {
            history::rebuild_memory_index(&frame.state.memory, &mut frame.state.memory_index);
        }
        self.record_now(frame_id, "memory", format!("remember {id} {key}: {value}"));
        id
    }

    pub(crate) fn note_memory(&mut self, value: String) -> MemoryId {
        let id = MemoryId::new(format!("memory-{}", uuid::Uuid::new_v4()));
        let key = id.to_string();
        self.remember(key, value)
    }

    pub(crate) fn forget_memory(&mut self, selector: &str) -> bool {
        let frame_id = self.frames.last().map(|frame| frame.id.clone());
        let Some(frame) = self.frames.last_mut() else {
            return false;
        };
        let selected: Vec<_> = frame
            .state
            .memory
            .iter()
            .filter(|entry| entry.key == selector || entry.id.as_str() == selector)
            .map(|entry| entry.id.clone())
            .collect();
        let _ = frame;
        for id in &selected {
            self.note_memory_before(id);
        }
        let frame = self.frames.last_mut().expect("active frame");
        let before = frame.state.memory.len();
        frame
            .state
            .memory
            .retain(|entry| entry.key != selector && entry.id.as_str() != selector);
        let removed = frame.state.memory.len() != before;
        if removed {
            history::rebuild_memory_index(&frame.state.memory, &mut frame.state.memory_index);
            self.record_now(frame_id, "memory", format!("forget {selector}"));
        }
        removed
    }

    pub(crate) fn visible_memory(&self) -> Vec<&MemoryEntry> {
        let Some(active) = self.frames.last() else {
            return Vec::new();
        };
        let mut entries: Vec<_> = self
            .frames
            .first()
            .into_iter()
            .flat_map(|root| root.state.memory.iter())
            .collect();
        if self.frames.len() > 1 {
            entries.extend(active.state.memory.iter());
        }
        entries
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

    pub fn eval(&mut self, source: &str) -> Result<EvalOutcome, eval::EvalError> {
        self.begin_eval_transaction();
        let previous_form = self.current_form.replace(CurrentForm {
            source: source.into(),
        });
        let evaluation = self.eval_control.begin();
        let result = evaluation.finish(
            crate::vm::reader::read_all(source)
                .map_err(|error| eval::EvalError::SyntaxError(error.to_string()))
                .and_then(|forms| eval::eval_forms(forms, self)),
        );
        self.current_form = previous_form;
        match result {
            Ok(value) => {
                self.commit_eval_transaction();
                Ok(EvalOutcome::Value(value))
            }
            Err(eval::EvalError::Trap(operation)) => {
                self.commit_eval_transaction();
                Ok(EvalOutcome::Trap(TrapRequest {
                    source: source.into(),
                    operation,
                }))
            }
            Err(error) => {
                self.rollback_eval_transaction();
                Err(error)
            }
        }
    }

    pub fn eval_value(&mut self, source: &str) -> Result<Value, eval::EvalError> {
        match self.eval(source)? {
            EvalOutcome::Value(value) => Ok(value),
            EvalOutcome::Trap(_) => Err(eval::EvalError::InvalidForm(
                "external operation requires scheduler ownership".into(),
            )),
        }
    }

    pub(crate) fn traps_allowed(&self) -> bool {
        self.trap_allowed
    }

    pub(crate) fn with_trap_permission<T>(
        &mut self,
        allowed: bool,
        evaluate: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = std::mem::replace(&mut self.trap_allowed, allowed);
        let result = evaluate(self);
        self.trap_allowed = previous;
        result
    }

    pub(crate) fn current_source(&self) -> Option<&str> {
        self.current_form.as_ref().map(|form| form.source.as_str())
    }

    pub(crate) fn current_form_is(&self, expected: &str) -> bool {
        self.current_source()
            .and_then(|source| crate::vm::reader::read_all(source).ok())
            .is_some_and(|forms| {
                matches!(
                    forms.as_slice(),
                    [Value::List(items)]
                        if matches!(items.first(), Some(Value::Symbol(head)) if head == expected)
                )
            })
    }

    pub(crate) fn define_binding(
        &mut self,
        name: &str,
        value: Value,
        source: Option<String>,
    ) -> Result<(), crate::vm::env::EnvError> {
        self.env.define(name, value, source, self.definition_origin)
    }

    pub fn spawn_subagent(
        &mut self,
        name: &str,
        request: &str,
    ) -> Result<FrameId, AllocationError> {
        let sequence = self.next_frame_id;
        self.next_frame_id = sequence
            .checked_add(1)
            .ok_or(AllocationError::Exhausted("frame"))?;
        let id = FrameId::new(format!("frame-{sequence}"));
        self.record_now(
            self.frames.last().map(|frame| frame.id.clone()),
            "agent-call",
            format!("{name}: {request}"),
        );
        self.frames.push(Frame {
            id: id.clone(),
            name: name.into(),
            waiting_for_human: false,
            notice_cursor: self.next_notice_sequence.saturating_sub(1),
            state: FrameState {
                instructions: format!(
                    "You are the '{name}' subagent. Complete this task and finish with (agent/return value): {request}"
                ),
                ..FrameState::default()
            },
        });
        Ok(id)
    }

    pub(crate) fn return_from_subagent(&mut self) {
        if let Some(child) = self.frames.pop() {
            self.record_now(Some(child.id.clone()), "agent-return", child.name.clone());
            for notice in &mut self.notices {
                notice.target_frames.retain(|target| target != &child.id);
            }
            self.retire_notices();
        }
    }

    fn ensure_notice_capacity(&mut self, count: u64) -> Result<(), AllocationError> {
        if self.next_notice_sequence.checked_add(count).is_none() {
            for frame in &mut self.frames {
                frame.notice_cursor = u64::try_from(
                    self.notices
                        .partition_point(|notice| notice.sequence <= frame.notice_cursor),
                )
                .map_err(|_| AllocationError::Exhausted("notice"))?;
            }
            for (index, notice) in self.notices.iter_mut().enumerate() {
                notice.sequence =
                    u64::try_from(index + 1).map_err(|_| AllocationError::Exhausted("notice"))?;
            }
            self.next_notice_sequence = u64::try_from(self.notices.len())
                .ok()
                .and_then(|count| count.checked_add(1))
                .ok_or(AllocationError::Exhausted("notice"))?;
        }
        self.next_notice_sequence
            .checked_add(count)
            .ok_or(AllocationError::Exhausted("notice"))?;
        Ok(())
    }

    pub(super) fn push_notice(
        &mut self,
        id: Option<MessageId>,
        text: String,
        target_frames: Vec<FrameId>,
    ) -> Result<u64, AllocationError> {
        self.ensure_notice_capacity(1)?;
        let sequence = self.next_notice_sequence;
        self.next_notice_sequence += 1;
        self.notices.push(StackNotice {
            sequence,
            id,
            text,
            target_frames,
            handled: false,
        });
        Ok(sequence)
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
        let targets = self.frames.iter().map(|frame| frame.id.clone()).collect();
        self.record_now(None, "human", text.to_owned());
        self.push_notice(
            Some(id.clone()),
            text.chars().take(8_000).collect(),
            targets,
        )?;
        if let Some(active) = self.frames.last_mut() {
            active.waiting_for_human = false;
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
            .ok_or_else(|| MessageError::Unknown(id.clone()))?;
        notice.handled = true;
        self.retire_notices();
        Ok(())
    }

    pub fn notices_for_frame(&self, frame_id: &FrameId) -> Vec<&StackNotice> {
        let cursor = self
            .frames
            .iter()
            .find(|frame| &frame.id == frame_id)
            .map_or(0, |frame| frame.notice_cursor);
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
            let seen = notice.target_frames.iter().all(|target| {
                self.frames
                    .iter()
                    .find(|frame| frame.id == *target)
                    .is_none_or(|frame| frame.notice_cursor >= notice.sequence)
            });
            !seen || (notice.id.is_some() && !notice.handled)
        });
    }

    pub fn check_wake_timers(&mut self) -> Result<usize, AllocationError> {
        let now = chrono::Utc::now();
        let fired: Vec<_> = self
            .wake_timers
            .extract_if(.., |entry| entry.wake_at <= now)
            .filter(|entry| self.frames.iter().any(|frame| frame.id == entry.frame_id))
            .collect();
        let required = u64::try_from(fired.len()).map_err(|_| AllocationError::Exhausted("notice"));
        if let Err(error) = required.and_then(|count| self.ensure_notice_capacity(count)) {
            self.wake_timers.extend(fired);
            return Err(error);
        }
        let count = fired.len();
        for entry in fired {
            self.push_notice(None, entry.action, vec![entry.frame_id])?;
        }
        Ok(count)
    }

    pub(super) fn validate_recovered(&mut self) -> Result<(), SnapshotError> {
        if self.frames.is_empty() {
            return Err(SnapshotError::Invalid("snapshot has no frames".into()));
        }
        self.env.validate().map_err(SnapshotError::Invalid)?;
        let mut frame_ids = HashSet::new();
        if self
            .frames
            .iter()
            .any(|frame| !frame_ids.insert(frame.id.clone()))
        {
            return Err(SnapshotError::Invalid(
                "snapshot has duplicate frame IDs".into(),
            ));
        }
        let mut message_ids = HashSet::new();
        let mut previous_sequence = 0;
        for notice in &self.notices {
            let mut targets = HashSet::new();
            if notice.target_frames.is_empty()
                || notice
                    .target_frames
                    .iter()
                    .any(|target| !frame_ids.contains(target) || !targets.insert(target))
                || notice.sequence <= previous_sequence
            {
                return Err(SnapshotError::Invalid(
                    "snapshot has invalid notice state".into(),
                ));
            }
            previous_sequence = notice.sequence;
            if !notice.handled
                && let Some(id) = &notice.id
                && !message_ids.insert(id.clone())
            {
                return Err(SnapshotError::Invalid(
                    "snapshot has duplicate pending message IDs".into(),
                ));
            }
        }
        let greatest_seen = self
            .frames
            .iter()
            .map(|frame| frame.notice_cursor)
            .fold(previous_sequence, u64::max);
        self.next_notice_sequence = self.next_notice_sequence.max(
            greatest_seen
                .checked_add(1)
                .ok_or_else(|| SnapshotError::Invalid("notice sequence is exhausted".into()))?,
        );
        if self
            .wake_timers
            .iter()
            .any(|entry| !frame_ids.contains(&entry.frame_id))
        {
            return Err(SnapshotError::Invalid(
                "snapshot wake timer targets an unknown frame".into(),
            ));
        }
        if let Some(maximum) = self
            .frames
            .iter()
            .filter_map(|frame| {
                frame
                    .id
                    .as_str()
                    .strip_prefix("frame-")?
                    .parse::<u64>()
                    .ok()
            })
            .max()
        {
            self.next_frame_id = self.next_frame_id.max(
                maximum
                    .checked_add(1)
                    .ok_or_else(|| SnapshotError::Invalid("frame sequence is exhausted".into()))?,
            );
        }
        let mut previous_event = 0_u64;
        for event in &self.history {
            if event.id <= previous_event {
                return Err(SnapshotError::Invalid(
                    "snapshot history events are not strictly ordered".into(),
                ));
            }
            previous_event = event.id;
        }
        self.next_event_id =
            self.next_event_id
                .max(previous_event.checked_add(1).ok_or_else(|| {
                    SnapshotError::Invalid("history event sequence is exhausted".into())
                })?);
        for frame in &mut self.frames {
            history::rebuild_memory_index(&frame.state.memory, &mut frame.state.memory_index);
        }
        self.definition_origin = BindingOrigin::Agent;
        self.trap_allowed = false;
        self.active_hooks.clear();
        self.deferred_hooks.clear();
        self.eval_transaction = None;
        Ok(())
    }

    pub(crate) fn collect_garbage(&mut self) {
        self.collect_lexical_arena();
    }

    pub(crate) fn control_notice(&mut self, text: String) -> Result<u64, AllocationError> {
        self.record_now(None, "control", text.clone());
        let targets = self.frames.iter().map(|frame| frame.id.clone()).collect();
        self.push_notice(None, text, targets)
    }

    pub(super) fn collect_lexical_arena(&mut self) {
        let mut reachable = HashSet::from([EnvironmentId::ROOT, self.env.current_environment()]);
        for namespace in self.env.namespaces.values() {
            collect_captured_environments(namespace.bindings.values(), &mut reachable);
        }
        let mut pending: Vec<_> = reachable.iter().copied().collect();
        let mut scanned = HashSet::new();
        while let Some(id) = pending.pop() {
            if !scanned.insert(id) {
                continue;
            }
            if let Some(environment) = self.env.lexical.environments.get(&id) {
                if let Some(parent) = environment.parent
                    && reachable.insert(parent)
                {
                    pending.push(parent);
                }
                for cell in environment.bindings.values() {
                    if let Some(value) = self.env.lexical.cells.get(cell) {
                        let before = reachable.len();
                        collect_captured_environments([value], &mut reachable);
                        if reachable.len() != before {
                            pending.extend(reachable.difference(&scanned).copied());
                        }
                    }
                }
            }
        }
        self.env
            .lexical
            .environments
            .retain(|id, _| reachable.contains(id));
        let cells: HashSet<_> = self
            .env
            .lexical
            .environments
            .values()
            .flat_map(|environment| environment.bindings.values().copied())
            .collect();
        self.env.lexical.cells.retain(|id, _| cells.contains(id));
    }

    pub fn inspect_namespace(&self, name: &str) -> Option<Vec<String>> {
        self.env
            .namespaces
            .get(name)
            .map(|namespace| namespace.list_bindings())
    }

    pub fn find_bindings(&self, query: &str) -> Vec<String> {
        let query = query.to_lowercase();
        let mut results = Vec::new();
        for (namespace, values) in self.env.namespaces.iter() {
            for binding in values.list_bindings() {
                let qualified = format!("{namespace}/{binding}");
                if qualified.to_lowercase().contains(&query) {
                    results.push(qualified);
                }
            }
        }
        results.sort();
        results
    }
}
