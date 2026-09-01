use persistent_lisp_harness::snowflake::effects::ExternalRun;
use persistent_lisp_harness::snowflake::image::ImageStore;
use persistent_lisp_harness::snowflake::runtime::{Command, Runtime};
use persistent_lisp_harness::snowflake::value::Value;
use persistent_lisp_harness::snowflake::world::{Agent, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("snowflake-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn wait_for(counter: &AtomicUsize, target: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::Acquire) < target {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

fn scripted(
    counter: Arc<AtomicUsize>,
    sources: Vec<&'static str>,
) -> impl Fn(&persistent_lisp_harness::snowflake::effects::EffectRequest) -> ExternalRun + Send + Sync
{
    move |_| {
        let turn = counter.fetch_add(1, Ordering::AcqRel);
        let source = sources.get(turn).copied();
        let cancelled = tokio_util::sync::CancellationToken::new();
        let future_token = cancelled.clone();
        ExternalRun::new(
            Box::pin(async move {
                match source {
                    Some(source) => Ok(Value::String(source.into())),
                    None => {
                        future_token.cancelled().await;
                        Err(persistent_lisp_harness::snowflake::effects::EffectError(
                            "cancelled".into(),
                        ))
                    }
                }
            }),
            move || cancelled.cancel(),
        )
    }
}

async fn finish_runtime(
    worker: tokio::task::JoinHandle<
        Result<Runtime, persistent_lisp_harness::snowflake::runtime::RuntimeError>,
    >,
) -> Runtime {
    tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .expect("runtime shutdown hung")
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn nested_agent_live_return_resumes_parked_parent() {
    let directory = directory();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(
            counter.clone(),
            vec![r#"(agent "child" "help")"#, r#"(return "done")"#],
        ),
    );
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&counter, 3).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    assert_eq!(runtime.world.state.agents.len(), 1);
    assert_eq!(
        runtime.world.state.agents[0]
            .transcript
            .last()
            .unwrap()
            .result,
        "done"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn recovered_child_return_takes_durable_transcript_fallback() {
    let directory = directory();
    let mut world = World::default();
    world.state.agents = vec![
        Agent::new("parent".into(), String::new()),
        Agent::new("child".into(), String::new()),
    ];
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        world,
        ImageStore::new(&directory),
        scripted(counter.clone(), vec![r#"(return "recovered")"#]),
    );
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&counter, 2).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    let parent = &runtime.world.state.agents[0];
    assert_eq!(
        parent.transcript.last().unwrap().source,
        "(agent/result child)"
    );
    assert_eq!(parent.transcript.last().unwrap().result, "recovered");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn snapshot_during_external_pause_never_calls_cancellation() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let cancelled = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();
    let cancelled_clone = cancelled.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            let turn = started_clone.fetch_add(1, Ordering::AcqRel);
            let cancelled = cancelled_clone.clone();
            let wake = Arc::new(tokio::sync::Notify::new());
            let future_wake = wake.clone();
            ExternalRun::new(
                Box::pin(async move {
                    if turn == 0 {
                        Ok(Value::String(r#"(model "wait")"#.into()))
                    } else {
                        future_wake.notified().await;
                        Err(persistent_lisp_harness::snowflake::effects::EffectError(
                            "cancelled".into(),
                        ))
                    }
                }),
                move || {
                    cancelled.store(true, Ordering::Release);
                    wake.notify_one();
                },
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 2).await;
    handle.send(Command::Snapshot).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!cancelled.load(Ordering::Acquire));
    handle
        .send(Command::HumanMessage("must survive".into()))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!cancelled.load(Ordering::Acquire));
    handle.send(Command::CancelExternal).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !cancelled.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    assert_eq!(runtime.world.state.messages.len(), 1);
    assert!(ImageStore::new(&directory).load().is_ok());
    drop(runtime);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn intentional_model_cancellation_does_not_enter_failure_backoff() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let seen = started.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            seen.fetch_add(1, Ordering::AcqRel);
            let cancelled = tokio_util::sync::CancellationToken::new();
            let future_cancel = cancelled.clone();
            ExternalRun::new(
                Box::pin(async move {
                    future_cancel.cancelled().await;
                    Err(persistent_lisp_harness::snowflake::effects::EffectError(
                        "model request interrupted by human input".into(),
                    ))
                }),
                move || cancelled.cancel(),
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 1).await;
    handle.send(Command::CancelExternal).unwrap();
    wait_for(&started, 2).await;
    handle.send(Command::Shutdown).unwrap();
    finish_runtime(worker).await;
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn shutdown_closes_human_message_admission_for_handle_and_direct_commands() {
    let directory = directory();
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        |_| unreachable!(),
    );
    let handle = runtime.handle();
    runtime.command(Command::Shutdown).unwrap();
    assert!(
        runtime
            .command(Command::HumanMessage("direct".into()))
            .is_err()
    );
    assert!(handle.send(Command::HumanMessage("late".into())).is_err());
    let mut queued = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        |_| unreachable!(),
    );
    queued.handle().send(Command::Shutdown).unwrap();
    assert!(
        queued
            .command(Command::HumanMessage("late direct".into()))
            .is_err()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn persistent_model_errors_back_off_but_human_input_wakes_the_runtime() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let seen = started.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            seen.fetch_add(1, Ordering::AcqRel);
            ExternalRun::new(
                Box::pin(async {
                    Err(persistent_lisp_harness::snowflake::effects::EffectError(
                        "offline".into(),
                    ))
                }),
                || {},
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 1).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(started.load(Ordering::Acquire), 1);
    handle.send(Command::HumanMessage("wake".into())).unwrap();
    wait_for(&started, 2).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    assert_eq!(runtime.world.state.messages.len(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn human_message_reply_is_durable_and_answered() {
    let directory = directory();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(counter.clone(), vec![r#"(reply 0 "yes")"#]),
    );
    runtime
        .command(Command::HumanMessage("hello".into()))
        .unwrap();
    let observed_reply = Arc::new(std::sync::Mutex::new(false));
    let seen_reply = observed_reply.clone();
    runtime.observe(Arc::new(move |_, _, replied| {
        *seen_reply.lock().unwrap() |= replied
    }));
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&counter, 2).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    let message = runtime.world.state.messages.values().next().unwrap();
    assert!(message.reply.is_some());
    assert_eq!(message.reply.as_deref(), Some("yes"));
    assert!(*observed_reply.lock().unwrap());
    assert!(runtime.world.state.agents[0].inbox.is_empty());
    assert_eq!(
        runtime.world.state.agents[0]
            .transcript
            .last()
            .unwrap()
            .result,
        "yes"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn task_completion_commits_and_task_failure_rolls_back_then_loop_continues() {
    let directory = directory();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(
            counter.clone(),
            vec!["(define kept 1)", "(begin (define discarded 2) (missing))"],
        ),
    );
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&counter, 3).await;
    handle.send(Command::Shutdown).unwrap();
    let mut runtime = finish_runtime(worker).await;
    let kept = runtime.world.state.symbols.intern("kept");
    let discarded = runtime.world.state.symbols.intern("discarded");
    assert_eq!(runtime.world.state.globals[&kept].value, Value::Int(1));
    assert!(!runtime.world.state.globals.contains_key(&discarded));
    let failure = runtime.world.state.agents[0].transcript.last().unwrap();
    assert_eq!(failure.source, "(begin (define discarded 2) (missing))");
    assert!(failure.result.starts_with("error:"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn shutdown_is_bounded_when_an_adapter_ignores_cancellation() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let seen = started.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            seen.fetch_add(1, Ordering::AcqRel);
            ExternalRun::new(Box::pin(std::future::pending()), || {})
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 1).await;
    handle.send(Command::Shutdown).unwrap();
    for index in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _ = handle.send(Command::HumanMessage(format!("late-{index}")));
    }
    let runtime = finish_runtime(worker).await;
    assert!(ImageStore::new(&directory).load().is_ok());
    drop(runtime);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn shutdown_cancellation_cannot_be_overwritten_by_later_pause_commands() {
    let directory = directory();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(
            counter.clone(),
            vec!["(letrec ((loop (lambda () (loop)))) (loop))"],
        ),
    );
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&counter, 1).await;
    handle.send(Command::Shutdown).unwrap();
    handle.send(Command::Snapshot).unwrap();
    let runtime = finish_runtime(worker).await;
    assert!(ImageStore::new(&directory).load().is_ok());
    drop(runtime);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unanswered_child_messages_return_to_the_parent_inbox() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let child_release = Arc::new(tokio::sync::Notify::new());
    let seen = started.clone();
    let release = child_release.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            let turn = seen.fetch_add(1, Ordering::AcqRel);
            let release = release.clone();
            let cancelled = tokio_util::sync::CancellationToken::new();
            let future_cancel = cancelled.clone();
            ExternalRun::new(
                Box::pin(async move {
                    match turn {
                        0 => Ok(Value::String(r#"(agent "child" "help")"#.into())),
                        1 => {
                            release.notified().await;
                            Ok(Value::String(r#"(return "done")"#.into()))
                        }
                        _ => {
                            future_cancel.cancelled().await;
                            Err(persistent_lisp_harness::snowflake::effects::EffectError(
                                "cancelled".into(),
                            ))
                        }
                    }
                }),
                move || cancelled.cancel(),
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 2).await;
    handle
        .send(Command::HumanMessage("still pending".into()))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    child_release.notify_one();
    wait_for(&started, 3).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    assert_eq!(
        runtime.world.state.agents[0].inbox,
        vec![persistent_lisp_harness::snowflake::value::MessageId(0)]
    );
    assert!(
        runtime
            .world
            .state
            .messages
            .values()
            .next()
            .unwrap()
            .reply
            .is_none()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn dropping_run_during_lisp_preserves_world_and_cancels_worker() {
    let directory = directory();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(counter, vec!["(letrec ((loop (lambda () (loop)))) (loop))"]),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), runtime.run())
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(runtime.world.state.agents.len(), 1);
    assert!(runtime.world.state.symbols.get("+").is_some());
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn shutdown_deadline_preserves_only_the_accepted_external_segment() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let seen = started.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            let turn = seen.fetch_add(1, Ordering::AcqRel);
            ExternalRun::new(
                Box::pin(async move {
                    if turn == 0 {
                        Ok(Value::String(
                            r#"(begin (define tentative 9) (model "hang"))"#.into(),
                        ))
                    } else {
                        std::future::pending().await
                    }
                }),
                || {},
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 2).await;
    handle.send(Command::Shutdown).unwrap();
    for index in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let _ = handle.send(Command::HumanMessage(format!("deadline-{index}")));
    }
    let mut runtime = finish_runtime(worker).await;
    let tentative = runtime.world.state.symbols.intern("tentative");
    assert_eq!(runtime.world.state.globals[&tentative].value, Value::Int(9));
    let mut recovered = ImageStore::new(&directory).load().unwrap();
    let tentative = recovered.state.symbols.intern("tentative");
    assert_eq!(recovered.state.globals[&tentative].value, Value::Int(9));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cancel_lisp_sent_during_external_never_cancels_the_resumed_task() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let seen = started.clone();
    let released = release.clone();
    let mut runtime =
        Runtime::with_starter(World::default(), ImageStore::new(&directory), move |_| {
            let turn = seen.fetch_add(1, Ordering::AcqRel);
            let released = released.clone();
            let cancellation = tokio_util::sync::CancellationToken::new();
            let future_cancel = cancellation.clone();
            ExternalRun::new(
                Box::pin(async move {
                    match turn {
                        0 => Ok(Value::String(
                            r#"(begin (model "hold") "effectdone")"#.into(),
                        )),
                        1 => {
                            released.notified().await;
                            Ok(Value::String("released".into()))
                        }
                        _ => {
                            future_cancel.cancelled().await;
                            Err(persistent_lisp_harness::snowflake::effects::EffectError(
                                "cancelled".into(),
                            ))
                        }
                    }
                }),
                move || cancellation.cancel(),
            )
        });
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 2).await;
    handle.send(Command::CancelLisp).unwrap();
    release.notify_one();
    wait_for(&started, 3).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    let result = &runtime.world.state.agents[0]
        .transcript
        .last()
        .unwrap()
        .result;
    assert_eq!(result, "effectdone");
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn runtime_handle_still_cancels_actively_running_lisp() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(
            started.clone(),
            vec!["(letrec ((loop (lambda () (loop)))) (loop))"],
        ),
    );
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    handle.send(Command::CancelLisp).unwrap();
    wait_for(&started, 2).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    assert_eq!(
        runtime.world.state.agents[0]
            .transcript
            .last()
            .unwrap()
            .result,
        "error: Lisp cancelled"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn child_cannot_reply_to_a_message_owned_by_its_parent() {
    let directory = directory();
    let started = Arc::new(AtomicUsize::new(0));
    let mut runtime = Runtime::with_starter(
        World::default(),
        ImageStore::new(&directory),
        scripted(
            started.clone(),
            vec![
                r#"(agent "child" "help")"#,
                r#"(reply 0 "stolen")"#,
                r#"(return "done")"#,
            ],
        ),
    );
    runtime
        .command(Command::HumanMessage("root only".into()))
        .unwrap();
    let handle = runtime.handle();
    let worker = tokio::spawn(async move { runtime.run().await.map(|()| runtime) });
    wait_for(&started, 4).await;
    handle.send(Command::Shutdown).unwrap();
    let runtime = finish_runtime(worker).await;
    let message = runtime.world.state.messages.values().next().unwrap();
    assert!(message.reply.is_none());
    assert_eq!(
        runtime.world.state.agents[0].inbox,
        vec![persistent_lisp_harness::snowflake::value::MessageId(0)]
    );
    std::fs::remove_dir_all(directory).unwrap();
}
