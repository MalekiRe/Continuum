use async_trait::async_trait;
use persistent_lisp_harness::{
    Executor, ExecutorConfig, Kernel, ModelClient, ModelError, ModelRequest, Scheduler, TurnOutcome,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct FakeModel {
    replies: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl FakeModel {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: Arc::new(Mutex::new(
                replies.iter().rev().map(|s| s.to_string()).collect(),
            )),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl ModelClient for FakeModel {
    async fn complete(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        self.requests.lock().unwrap().push(request);
        self.replies
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| ModelError::Client("no fake reply".into()))
    }
}

fn temp_root(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("continuum-{}-{}", label, uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn scheduler(replies: &[&str]) -> (Scheduler<FakeModel>, FakeModel) {
    let model = FakeModel::new(replies);
    let executor = Executor::new(ExecutorConfig::with_working_directory(temp_root(
        "scheduler",
    )))
    .unwrap();
    (Scheduler::new(model.clone(), executor), model)
}

#[tokio::test]
async fn model_bash_result_enters_next_context() {
    let (scheduler, model) = scheduler(&[
        r#"(bash "printf hello")"#,
        "(define git/status 42)",
        "git/status",
    ]);
    let mut kernel = Kernel::new();
    let first = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(
        matches!(first, TurnOutcome::ToolCompleted { ref result, .. } if result.contains("hello"))
    );
    scheduler.run_turn(&mut kernel).await.unwrap();
    {
        let requests = model.requests.lock().unwrap();
        assert!(
            requests[1].context.contains("hello"),
            "next context omitted bash result"
        );
    }
    let third = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(matches!(third, TurnOutcome::Evaluated { ref result, .. } if result == "42"));
}

#[tokio::test]
async fn human_message_is_seen_then_explicitly_replied_to() {
    let mut kernel = Kernel::new();
    let id = kernel.human_message("question").unwrap();
    let reply = format!(r#"(message/reply "{}" "answer")"#, id);
    let (scheduler, model) = scheduler(&[&reply]);
    let outcome = scheduler.run_turn(&mut kernel).await.unwrap();
    assert_eq!(
        outcome,
        TurnOutcome::Replied {
            message_id: id.clone(),
            text: "answer".into()
        }
    );
    let request = &model.requests.lock().unwrap()[0];
    assert!(request.context.contains(id.as_str()));
    assert!(!kernel.has_pending_message(&id));
}

#[tokio::test]
async fn subagent_gets_own_context_and_returns_to_parent() {
    let (scheduler, model) = scheduler(&[
        r#"(agent/call "researcher" "inspect the build")"#,
        r#"(agent/return "done")"#,
    ]);
    let mut kernel = Kernel::new();
    let parent = kernel.frames()[0].id().clone();
    let first = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(matches!(first, TurnOutcome::Spawned { ref parent_id, .. } if parent_id == &parent));
    assert_eq!(kernel.frames().len(), 2);
    assert!(
        kernel
            .frames()
            .last()
            .unwrap()
            .state()
            .instructions()
            .contains("researcher")
    );
    assert_eq!(
        kernel.frames()[0].status(),
        persistent_lisp_harness::FrameStatus::Waiting
    );
    let second = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(
        matches!(second, TurnOutcome::Returned { ref parent_id, ref result } if parent_id == &parent && result == "done")
    );
    assert_eq!(kernel.frames().len(), 1);
    assert_eq!(
        kernel.frames()[0].status(),
        persistent_lisp_harness::FrameStatus::Running
    );
    let requests = model.requests.lock().unwrap();
    assert!(requests[1].system.contains("researcher"));
    assert!(requests[1].system.contains("inspect the build"));
}

#[test]
fn nested_suspension_is_rejected_transactionally() {
    let mut kernel = Kernel::new();
    let result = kernel.eval(r#"(begin (define x 1) (bash "true"))"#);
    assert!(result.is_err());
    assert!(kernel.eval("x").is_err());
    assert!(kernel.take_trap().is_none());
}

#[test]
fn source_is_exact_and_rolled_back_with_failed_turn() {
    let mut kernel = Kernel::new();
    let source = "(define (hello name)\n  (string-append \"hi \" name))";
    kernel.eval(source).unwrap();
    assert_eq!(kernel.environment().source("user/hello"), Some(source));
    let bad = kernel.eval("(begin (define (ghost) 1) (missing))");
    assert!(bad.is_err());
    assert!(kernel.environment().source("user/ghost").is_none());
    assert!(kernel.eval("ghost").is_err());
}

#[test]
fn model_must_emit_exactly_one_form() {
    assert!(persistent_lisp_harness::scheduler::normalize_one_form("(+ 1 2) (+ 3 4)").is_err());
    assert!(
        persistent_lisp_harness::scheduler::normalize_one_form("```lisp\n(+ 1 2)\n```").is_err()
    );
    assert!(
        persistent_lisp_harness::scheduler::normalize_one_form("<lisp>(+ 1 2)</lisp>").is_err()
    );
}

#[tokio::test]
async fn transcript_compacts_chronologically() {
    let (scheduler, _) = scheduler(&["nil"]);
    let mut kernel = Kernel::new();
    for i in 0..5 {
        kernel.append_transcript(
            &format!("form-{}", i),
            &format!("result-{}-{}", i, "x".repeat(7_000)),
        );
    }
    scheduler.run_turn(&mut kernel).await.unwrap();
    let state = kernel.frames()[0].state();
    assert!(state.compacted_context().contains("form-0"));
    assert!(state.compacted_context().contains("form-1"));
    assert!(!state.compacted_context().contains("form-2"));
    assert_eq!(state.transcript().last().unwrap().source, "nil");
}

#[tokio::test]
async fn selected_memory_and_context_hooks_are_injected() {
    let (scheduler, model) = scheduler(&["nil"]);
    let mut kernel = Kernel::new();
    kernel
        .eval(r#"(memory/remember "project" "Continuum")"#)
        .unwrap();
    kernel
        .eval(r#"(context/add-hook "Prefer tests before edits")"#)
        .unwrap();
    scheduler.run_turn(&mut kernel).await.unwrap();
    let request = &model.requests.lock().unwrap()[0];
    assert!(request.context.contains("project: Continuum"));
    assert!(request.context.contains("Prefer tests before edits"));
}

#[tokio::test]
async fn scheduler_resumes_pending_tool_without_new_model_call() {
    let (scheduler, model) = scheduler(&[]);
    let mut kernel = Kernel::new();
    kernel.eval(r#"(bash "printf resumed")"#).unwrap();
    let outcome = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(
        matches!(outcome, TurnOutcome::ToolCompleted { ref result, .. } if result.contains("resumed"))
    );
    assert!(model.requests.lock().unwrap().is_empty());
    assert!(!kernel.has_trap());
}

#[tokio::test]
async fn definition_is_recalled_after_ten_model_turns() {
    let mut replies = vec![r#"(define git/status (lambda () "clean"))"#];
    replies.extend(std::iter::repeat_n("nil", 9));
    replies.push("(git/status)");
    let (scheduler, model) = scheduler(&replies);
    let mut kernel = Kernel::new();
    let mut last = None;
    for _ in 0..11 {
        last = Some(scheduler.run_turn(&mut kernel).await.unwrap());
    }
    assert!(
        matches!(last, Some(TurnOutcome::Evaluated { ref result, .. }) if result == "\"clean\"")
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 11);
    assert!(requests[10].context.contains("git/status"));
    assert!(requests[10].context.contains("(define git/status"));
}

#[tokio::test]
async fn malformed_model_output_is_recorded_for_self_correction() {
    let (scheduler, model) = scheduler(&["```lisp\nnil\n```", "nil"]);
    let mut kernel = Kernel::new();
    let first = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(
        matches!(first, TurnOutcome::Evaluated { ref result, .. } if result.contains("raw Lisp"))
    );
    scheduler.run_turn(&mut kernel).await.unwrap();
    let requests = model.requests.lock().unwrap();
    assert!(requests[1].context.contains("```lisp"));
    assert!(requests[1].context.contains("raw Lisp"));
}

#[tokio::test]
async fn pending_human_message_survives_until_valid_reply() {
    let mut kernel = Kernel::new();
    let id = kernel.human_message("do not lose me").unwrap();
    let reply = format!(r#"(message/reply "{}" "done")"#, id);
    let (scheduler, _) = scheduler(&["nil", &reply]);
    scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(kernel.has_pending_message(&id));
    scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(!kernel.has_pending_message(&id));
}

#[test]
fn reply_to_unknown_message_is_rejected_without_a_trap() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval(r#"(message/reply "made-up" "no")"#).is_err());
    assert!(!kernel.has_trap());
}

#[test]
fn recent_results_and_total_context_are_bounded() {
    let (scheduler, _) = scheduler(&[]);
    let mut kernel = Kernel::new();
    kernel.append_transcript("unicode-😀", &"x".repeat(100_000));
    let request = scheduler.build_request(&kernel);
    assert!(request.context.len() < 65_000);
    assert!(!request.context.contains(&"x".repeat(2_000)));
    assert!(request.context.contains("unicode-😀"));
}

#[test]
fn context_budget_preserves_high_priority_guidance() {
    let (scheduler, _) = scheduler(&[]);
    let mut kernel = Kernel::new();
    let message_id = kernel.human_message("PRIORITY-HUMAN").unwrap();
    kernel
        .eval(r#"(memory/remember "priority" "PRIORITY-MEMORY")"#)
        .unwrap();
    kernel
        .eval(r#"(context/add-hook "PRIORITY-HOOK")"#)
        .unwrap();
    for index in 0..100 {
        kernel.append_transcript(&format!("action-{index}"), &"noise".repeat(10_000));
    }
    let request = scheduler.build_request(&kernel);
    assert!(request.context.len() <= 62_000);
    assert!(request.context.contains(message_id.as_str()));
    assert!(request.context.contains("PRIORITY-HUMAN"));
    assert!(request.context.contains("PRIORITY-MEMORY"));
    assert!(request.context.contains("PRIORITY-HOOK"));
}

#[test]
fn wake_timer_delivers_to_its_original_frame() {
    let mut kernel = Kernel::new();
    let root = kernel.frames()[0].id().clone();
    kernel
        .schedule_wake_at(
            root.clone(),
            chrono::DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            "wake-root",
        )
        .unwrap();
    kernel.spawn_subagent("child", "wait").unwrap();
    assert_eq!(kernel.check_wake_timers().unwrap(), 1);
    assert!(
        kernel
            .notices_for_frame(&root)
            .iter()
            .any(|notice| notice.text == "wake-root")
    );
    let child = kernel.frames()[1].id().clone();
    assert!(
        !kernel
            .notices_for_frame(&child)
            .iter()
            .any(|notice| notice.text == "wake-root")
    );
}

#[derive(Clone)]
struct PendingFirstModel {
    calls: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}

struct ActiveCall(Arc<AtomicUsize>);

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ModelClient for PendingFirstModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _active_call = ActiveCall(Arc::clone(&self.active));
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            self.started.notify_one();
            std::future::pending().await
        } else {
            Ok("nil".into())
        }
    }
}

#[derive(Clone)]
struct PendingTrappedModel {
    calls: Arc<AtomicUsize>,
    trapped_request_started: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ModelClient for PendingTrappedModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(r#"(model/call "nested request")"#.into())
        } else {
            self.trapped_request_started.notify_one();
            std::future::pending().await
        }
    }
}

#[tokio::test]
async fn cancelled_turn_can_discard_its_installed_external_trap() {
    let model = PendingTrappedModel {
        calls: Arc::new(AtomicUsize::new(0)),
        trapped_request_started: Arc::new(tokio::sync::Notify::new()),
    };
    let executor = Executor::new(ExecutorConfig::with_working_directory(temp_root(
        "trapped-model-cancel",
    )))
    .unwrap();
    let scheduler = Scheduler::new(model.clone(), executor);
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(temp_root("trapped-model-snapshot"));

    let mut turn = Box::pin(scheduler.run_turn(&mut kernel));
    tokio::select! {
        _ = model.trapped_request_started.notified() => {}
        result = &mut turn => panic!("trapped model request finished unexpectedly: {result:?}"),
    }
    drop(turn);
    assert!(kernel.has_trap());

    kernel.discard_pending_operation();
    assert!(!kernel.has_trap());
    kernel.snapshot().unwrap();
}

#[tokio::test]
async fn interrupted_request_is_dropped_before_the_next_generation_starts() {
    let model = PendingFirstModel {
        calls: Arc::new(AtomicUsize::new(0)),
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
        started: Arc::new(tokio::sync::Notify::new()),
    };
    let executor = Executor::new(ExecutorConfig::with_working_directory(temp_root(
        "model-cancel",
    )))
    .unwrap();
    let scheduler = Scheduler::new(model.clone(), executor);
    let interrupt = scheduler.model_interrupt_handle();
    let mut kernel = Kernel::new();

    let first = scheduler.run_turn(&mut kernel);
    let cancel = async {
        model.started.notified().await;
        assert!(interrupt.request_interrupt());
    };
    let (first_result, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        tokio::join!(first, cancel)
    })
    .await
    .expect("cancelled generation did not stop");
    assert_eq!(
        first_result.unwrap_err().to_string(),
        "model request interrupted by human input"
    );
    assert_eq!(model.active.load(Ordering::SeqCst), 0);
    assert!(interrupt.request_interrupt());
    interrupt.clear_pending();

    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        scheduler.run_turn(&mut kernel),
    )
    .await
    .expect("next generation did not start")
    .unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(model.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(model.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn parent_sees_human_redirect_after_child_replies() {
    let (scheduler, model) = scheduler(&[r#"(agent/call "child" "work")"#]);
    let mut kernel = Kernel::new();
    scheduler.run_turn(&mut kernel).await.unwrap();
    let message_id = kernel.human_message("redirect the work").unwrap();
    let reply = format!(r#"(message/reply "{}" "acknowledged")"#, message_id);
    {
        let mut replies = model.replies.lock().unwrap();
        replies.push("nil".into());
        replies.push("(agent/return \"done\")".into());
        replies.push(reply);
    }
    scheduler.run_turn(&mut kernel).await.unwrap();
    scheduler.run_turn(&mut kernel).await.unwrap();
    scheduler.run_turn(&mut kernel).await.unwrap();
    let requests = model.requests.lock().unwrap();
    let parent_request = requests.last().unwrap();
    assert!(parent_request.context.contains(message_id.as_str()));
    assert!(parent_request.context.contains("redirect the work"));
    assert!(parent_request.context.contains("Answered human notice"));
    assert!(parent_request.context.contains("do not call message/reply"));
    assert!(!kernel.has_pending_message(&message_id));
}

#[tokio::test]
async fn external_model_failure_is_typed_and_clears_the_trap() {
    let (scheduler, _) = scheduler(&[]);
    let mut kernel = Kernel::new();
    kernel
        .eval(r#"(model/call "fail deterministically")"#)
        .unwrap();
    let error = scheduler.run_turn(&mut kernel).await.unwrap_err();
    assert!(matches!(
        error,
        persistent_lisp_harness::SchedulerError::Model(_)
    ));
    assert!(!kernel.has_trap());
    assert!(
        kernel.frames()[0]
            .state()
            .transcript()
            .last()
            .unwrap()
            .result
            .contains("error")
    );
}

#[test]
fn worst_case_context_sections_stay_within_the_total_request_budget() {
    let (scheduler, _) = scheduler(&[]);
    let mut kernel = Kernel::new();
    for index in 0..100 {
        kernel
            .spawn_subagent(&format!("frame-{index}"), &"task".repeat(5_000))
            .unwrap();
    }
    for index in 0..32 {
        kernel
            .human_message(&format!("notice-{index}-{}", "🦀".repeat(2_000)))
            .unwrap();
    }
    for index in 0..16 {
        kernel
            .eval(&format!(
                r#"(context/add-hook "hook-{index}-{}")"#,
                "h".repeat(1_980)
            ))
            .unwrap();
    }
    for index in 0..64 {
        kernel
            .eval(&format!(
                r#"(memory/remember "key-{index}" "{}")"#,
                "m".repeat(1_000)
            ))
            .unwrap();
    }
    for index in 0..100 {
        kernel.append_transcript(
            &format!("action-{index}-{}", "a".repeat(1_000)),
            &"r".repeat(5_000),
        );
    }
    let request = scheduler.build_request(&kernel);
    assert!(request.system.len() + request.context.len() <= 62_000);
    assert!(request.context.contains("Emit exactly one Lisp form"));
    assert!(request.context.contains("notice-0"));
    assert!(request.context.contains("notice-31"));
}

#[tokio::test]
async fn model_interrupt_queued_before_activation_cancels_the_imminent_generation() {
    let (scheduler, model) = scheduler(&["nil"]);
    let interrupt = scheduler.model_interrupt_handle();
    assert!(interrupt.request_interrupt());
    let mut kernel = Kernel::new();
    let error = scheduler.run_turn(&mut kernel).await.unwrap_err();
    assert!(matches!(
        error,
        persistent_lisp_harness::SchedulerError::Model(ModelError::Cancelled)
    ));
    assert!(model.requests.lock().unwrap().is_empty());
    scheduler.run_turn(&mut kernel).await.unwrap();
}

#[derive(Clone)]
struct CompletionHandoffModel {
    interrupt: Arc<Mutex<Option<persistent_lisp_harness::ModelInterruptHandle>>>,
}

#[async_trait]
impl ModelClient for CompletionHandoffModel {
    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<String, ModelError> {
        self.interrupt
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .request_interrupt();
        Ok("nil".into())
    }
}

#[tokio::test]
async fn interrupt_at_model_completion_discards_the_completed_generation() {
    let slot = Arc::new(Mutex::new(None));
    let executor =
        Executor::new(ExecutorConfig::with_working_directory(temp_root("handoff"))).unwrap();
    let scheduler = Scheduler::new(
        CompletionHandoffModel {
            interrupt: slot.clone(),
        },
        executor,
    );
    *slot.lock().unwrap() = Some(scheduler.model_interrupt_handle());
    let mut kernel = Kernel::new();
    let error = scheduler.run_turn(&mut kernel).await.unwrap_err();
    assert!(matches!(
        error,
        persistent_lisp_harness::SchedulerError::Model(ModelError::Cancelled)
    ));
    assert!(kernel.frames()[0].state().transcript().is_empty());
}

#[test]
fn agent_boundaries_require_strings() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval("(agent/call 'researcher \"task\")").is_err());
    assert!(kernel.eval("(agent/call \"researcher\" 'task)").is_err());
    kernel.spawn_subagent("researcher", "task").unwrap();
    assert!(kernel.eval("(agent/return 42)").is_err());
}

#[tokio::test]
async fn human_wait_is_snapshot_safe_and_wakes_on_message() {
    let (scheduler, _) = scheduler(&["(human/wait)", "nil"]);
    let mut kernel = Kernel::new();
    let outcome = scheduler.run_turn(&mut kernel).await.unwrap();
    assert!(matches!(outcome, TurnOutcome::ToolCompleted { .. }));
    assert_eq!(
        kernel.frames()[0].status(),
        persistent_lisp_harness::FrameStatus::Waiting
    );
    assert!(!kernel.has_trap());

    let snapshots = temp_root("wait-snapshot");
    kernel.set_snapshot_directory(&snapshots);
    kernel.snapshot().unwrap();
    kernel.human_message("resume").unwrap();
    assert_eq!(
        kernel.frames()[0].status(),
        persistent_lisp_harness::FrameStatus::Running
    );
    scheduler.run_turn(&mut kernel).await.unwrap();
}

#[test]
fn constrained_context_keeps_the_newest_transcript_entries() {
    let (scheduler, _) = scheduler(&[]);
    let mut kernel = Kernel::new();
    for index in 0..30 {
        kernel.append_transcript(
            &format!("action-{index}"),
            &format!("result-{index}-{}", "x".repeat(1_500)),
        );
    }
    let request = scheduler.build_request(&kernel);
    assert!(request.context.contains("action-29"));
    assert!(request.context.contains("action-28"));
    assert!(!request.context.contains("action-0"));
}
