use super::*;
use crate::vm::env::BindingOrigin;
use crate::vm::value::collect_captured_environments;
use std::collections::HashSet;

impl Kernel {
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
        let checkpoint = (
            self.env.clone(),
            self.frames.clone(),
            self.wake_timers.clone(),
            self.next_frame_id,
        );
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
            Ok(value) => Ok(EvalOutcome::Value(value)),
            Err(eval::EvalError::Trap(operation)) => Ok(EvalOutcome::Trap(TrapRequest {
                source: source.into(),
                operation,
            })),
            Err(error) => {
                let (env, frames, wake_timers, next_frame_id) = checkpoint;
                self.env = env;
                self.frames = frames;
                self.wake_timers = wake_timers;
                self.next_frame_id = next_frame_id;
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
        self.definition_origin = BindingOrigin::Agent;
        Ok(())
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
