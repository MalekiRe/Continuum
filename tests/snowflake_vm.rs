use persistent_lisp_harness::snowflake::compile::compile;
use persistent_lisp_harness::snowflake::effects::{self, EffectError, EffectRequest};
use persistent_lisp_harness::snowflake::runtime::{CANCEL_LISP, PAUSE, RUN};
use persistent_lisp_harness::snowflake::value::Value;
use persistent_lisp_harness::snowflake::vm::{Task, TaskPoll};
use persistent_lisp_harness::snowflake::world::World;
use std::sync::atomic::AtomicU8;

fn task(world: &mut World, source: &str) -> Task {
    let control = AtomicU8::new(RUN);
    let entry = compile(world, source, &control).unwrap();
    Task::start(world, entry).unwrap()
}

fn evaluate(source: &str) -> Value {
    let mut world = World::default();
    effects::install(&mut world);
    let mut task = task(&mut world, source);
    match task.poll(&mut world, &AtomicU8::new(RUN)) {
        TaskPoll::Complete(value) => value,
        _ => panic!("task did not complete"),
    }
}

#[test]
fn closures_share_stable_mutable_cells_and_letrec_tail_calls() {
    assert_eq!(
        evaluate("(let ((f ((lambda (x) (lambda () (set! x (+ x 1)))) 0))) (begin (f) (f)))"),
        Value::Int(2)
    );
    assert_eq!(
        evaluate(
            "(letrec ((loop (lambda (n acc) (if (= n 0) acc (loop (- n 1) (+ acc 1)))))) (loop 20000 0))"
        ),
        Value::Int(20000)
    );
}

#[test]
fn two_effects_suspend_and_resume_once_each() {
    let mut world = World::default();
    effects::install(&mut world);
    let mut task = task(&mut world, r#"(list (bash "one") (model "two"))"#);
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Effect(EffectRequest::Bash(ref text)) if text == "one"
    ));
    task.commit_boundary(&world);
    assert!(task.resume(Ok(Value::String("first".into()))).is_ok());
    assert!(task.resume(Ok(Value::Nil)).is_err());
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Effect(EffectRequest::Model(ref text)) if text == "two"
    ));
    task.commit_boundary(&world);
    task.resume(Ok(Value::String("second".into()))).unwrap();
    assert_eq!(
        match task.poll(&mut world, &AtomicU8::new(RUN)) {
            TaskPoll::Complete(value) => value,
            _ => panic!("task did not complete"),
        },
        Value::List(vec![
            Value::String("first".into()),
            Value::String("second".into())
        ])
    );
}

#[test]
fn pause_is_resumable_cancel_and_errors_roll_back() {
    let mut world = World::default();
    effects::install(&mut world);
    let mut paused = task(&mut world, "(define saved 1)");
    assert!(matches!(
        paused.poll(&mut world, &AtomicU8::new(PAUSE)),
        TaskPoll::Paused
    ));
    assert!(matches!(
        paused.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Complete(Value::Int(1))
    ));

    let mut cancelled = task(&mut world, "(define discarded 2)");
    assert!(matches!(
        cancelled.poll(&mut world, &AtomicU8::new(CANCEL_LISP)),
        TaskPoll::Cancelled
    ));
    let discarded = world.state.symbols.intern("discarded");
    assert!(!world.state.globals.contains_key(&discarded));

    let mut failed = task(&mut world, "(begin (define temporary 3) (missing))");
    assert!(matches!(
        failed.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Failed(_)
    ));
    let temporary = world.state.symbols.intern("temporary");
    assert!(!world.state.globals.contains_key(&temporary));
}

#[test]
fn immutable_hosts_are_first_class_and_effect_failures_are_single_use() {
    assert_eq!(evaluate("(let ((alias +)) (alias 2 3))"), Value::Int(5));

    let mut world = World::default();
    effects::install(&mut world);
    let mut task = task(&mut world, r#"(bash "failure")"#);
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Effect(_)
    ));
    assert!(task.resume(Err(EffectError("no".into()))).is_err());
    assert!(task.resume(Ok(Value::Nil)).is_err());
    task.abort(&mut world);
}

#[test]
fn effect_boundaries_preserve_only_accepted_segments() {
    let mut world = World::default();
    effects::install(&mut world);
    let mut task = task(
        &mut world,
        r#"(begin (define before 1) (bash "ok") (define after 2) (missing))"#,
    );
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Effect(_)
    ));
    task.commit_boundary(&world);
    task.resume(Ok(Value::String("done".into()))).unwrap();
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Failed(_)
    ));
    let before = world.state.symbols.intern("before");
    let after = world.state.symbols.intern("after");
    assert_eq!(world.state.globals[&before].value, Value::Int(1));
    assert!(!world.state.globals.contains_key(&after));
}

#[test]
fn integer_overflow_is_an_effect_error_and_rolls_back() {
    let mut world = World::default();
    effects::install(&mut world);
    let mut task = task(&mut world, "(+ 9223372036854775807 1)");
    assert!(matches!(
        task.poll(&mut world, &AtomicU8::new(RUN)),
        TaskPoll::Failed(_)
    ));
}
