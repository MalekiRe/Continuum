use persistent_lisp_harness::{ExecutionOutcome, Executor, ExecutorConfig, ExecutorError};
use std::time::{Duration, Instant};

fn executor(label: &str, timeout: Duration, output_limit: usize) -> (Executor, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "continuum-executor-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    let mut config = ExecutorConfig::with_working_directory(&root);
    config.timeout = Some(timeout);
    config.output_limit = output_limit;
    (Executor::new(config).unwrap(), root)
}

fn wait_until_running(executor: &Executor) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !executor.is_running() {
        assert!(Instant::now() < deadline, "executor did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn timeout_kills_process_group_and_prevents_delayed_side_effect() {
    let (executor, root) = executor("timeout", Duration::from_millis(150), 1024);
    let marker = root.join("marker");
    let started = Instant::now();
    let result = executor.run("(sleep 2; touch marker) & wait").unwrap();
    assert!(result.outcome == ExecutionOutcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker.exists(),
        "descendant survived timeout and wrote marker"
    );
}

#[test]
fn external_cancel_returns_promptly() {
    let (executor, _) = executor("cancel", Duration::from_secs(10), 1024);
    let runner = executor.clone();
    let started = Instant::now();
    let handle = std::thread::spawn(move || runner.run("sleep 10").unwrap());
    wait_until_running(&executor);
    let status = executor.active_status().expect("running status");
    assert!(status.elapsed < Duration::from_secs(2));
    assert!(status.process_group > 0);
    assert!(executor.cancel().unwrap());
    let result = handle.join().unwrap();
    assert!(result.outcome == ExecutionOutcome::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn idle_cancel_does_not_cancel_the_next_run() {
    let (executor, root) = executor("idle-cancel", Duration::from_secs(2), 1024);
    assert!(!executor.cancel().unwrap());
    let result = executor.run("touch did-run; printf ok").unwrap();
    assert!(result.outcome != ExecutionOutcome::Cancelled);
    assert!(result.outcome != ExecutionOutcome::TimedOut);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.output.stdout, "ok");
    assert!(root.join("did-run").exists());
}

#[test]
fn concurrent_run_is_rejected_without_disturbing_active_run() {
    let (executor, root) = executor("concurrent", Duration::from_secs(10), 1024);
    let runner = executor.clone();
    let handle = std::thread::spawn(move || runner.run("sleep 10").unwrap());
    wait_until_running(&executor);

    let error = executor
        .run("touch concurrent-should-not-run")
        .expect_err("a second run must be rejected");
    assert!(matches!(error, ExecutorError::AlreadyRunning));
    assert!(!root.join("concurrent-should-not-run").exists());

    assert!(executor.cancel().unwrap());
    assert!(handle.join().unwrap().outcome == ExecutionOutcome::Cancelled);
}

#[test]
fn background_descendant_and_its_output_pipe_do_not_outlive_completion() {
    let (executor, root) = executor("background-completion", Duration::from_secs(10), 1024);
    let started = Instant::now();
    let result = executor
        .run("(sleep 2; echo late; touch late-marker) & echo leader-finished")
        .unwrap();

    assert_eq!(result.exit_code, 0);
    assert!(result.output.stdout.contains("leader-finished"));
    assert!(!result.output.stdout.contains("late"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "inherited output pipe kept execution alive"
    );
    std::thread::sleep(Duration::from_millis(250));
    assert!(!root.join("late-marker").exists());
}

#[test]
fn timeout_closes_output_from_term_ignoring_background_descendant() {
    let (executor, _) = executor("background-timeout", Duration::from_millis(100), 256);
    let started = Instant::now();
    let result = executor
        .run("(trap '' TERM; while :; do printf x; sleep 0.01; done) & wait")
        .unwrap();

    assert!(result.outcome == ExecutionOutcome::TimedOut);
    assert!(result.output.truncated || !result.output.stdout.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "background descendant kept its output pipe open after timeout"
    );
}

#[test]
fn output_is_bounded_but_fully_drained() {
    let (executor, _) = executor("bounded", Duration::from_secs(2), 128);
    let result = executor.run("yes x | head -c 100000").unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.output.truncated);
    assert!(result.output.stdout.len() <= 128);
}

#[test]
fn spawn_failure_releases_active_reservation() {
    let (executor, root) = executor("spawn-failure", Duration::from_secs(2), 1024);
    std::fs::remove_dir(&root).unwrap();

    let error = executor.run("printf unreachable").unwrap_err();
    assert!(matches!(error, ExecutorError::Spawn(_)));
    assert!(!executor.is_running());

    std::fs::create_dir(&root).unwrap();
    let result = executor.run("printf recovered").unwrap();
    assert_eq!(result.outcome, ExecutionOutcome::Exited);
    assert_eq!(result.output.stdout, "recovered");
}

#[test]
fn command_runs_in_fixed_root() {
    let (executor, root) = executor("root", Duration::from_secs(2), 1024);
    let result = executor.run("pwd").unwrap();
    assert_eq!(
        result.output.stdout.trim(),
        std::fs::canonicalize(root).unwrap().to_string_lossy()
    );
}

#[test]
fn running_status_reports_output_progress() {
    let (executor, _) = executor("progress", Duration::from_secs(10), 1024);
    let runner = executor.clone();
    let handle = std::thread::spawn(move || {
        runner
            .run("while true; do echo progress; sleep 0.01; done")
            .unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = executor.active_status()
            && status.stdout_bytes > 0
        {
            assert!(status.process_group > 0);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no live output progress reported"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(executor.cancel().unwrap());
    assert!(handle.join().unwrap().outcome == ExecutionOutcome::Cancelled);
}

#[test]
fn command_environment_is_scrubbed_and_workspace_scoped() {
    let (executor, root) = executor("environment", Duration::from_secs(2), 1024);
    let result = executor
        .run(r#"printf '%s\n%s\n%s' "$HOME" "$PATH" "${CARGO_MANIFEST_DIR-unset}""#)
        .unwrap();
    let lines: Vec<_> = result.output.stdout.lines().collect();
    let expected_home = std::fs::canonicalize(root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        lines,
        [
            expected_home.as_str(),
            "/usr/local/bin:/usr/bin:/bin",
            "unset",
        ]
    );
}
