mod unix;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("cannot {operation} executor working directory {path}: {source}")]
    WorkingDirectory {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to enable child subreaping: {0}")]
    Subreaper(#[source] io::Error),
    #[error("executor is already running a command")]
    AlreadyRunning,
    #[error("failed to spawn bash: {0}")]
    Spawn(#[source] io::Error),
    #[error("missing child {0}")]
    MissingPipe(&'static str),
    #[error("failed to signal process group {process_group} with signal {signal}: {source}")]
    Signal {
        process_group: i32,
        signal: i32,
        #[source]
        source: io::Error,
    },
    #[error("failed to {operation} bash: {source}")]
    Process {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("failed to read child {stream}: {source}")]
    OutputRead {
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{0} reader panicked")]
    ReaderPanicked(&'static str),
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub working_directory: PathBuf,
    pub timeout: Duration,
    pub output_limit: usize,
}

impl ExecutorConfig {
    pub fn with_working_directory(working_directory: impl Into<PathBuf>) -> Self {
        Self {
            working_directory: working_directory.into(),
            timeout: Duration::from_secs(60),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionOutcome {
    Exited,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub outcome: ExecutionOutcome,
    pub output: CapturedOutput,
    pub elapsed_ms: u128,
}

// The executor's Rust API uses typed outcome/output fields, while the tool wire
// format deliberately remains compatible with existing transcripts and clients.
#[derive(Serialize)]
struct ExecutionResultWireRef<'a> {
    exit_code: i32,
    stdout: &'a str,
    stderr: &'a str,
    timed_out: bool,
    cancelled: bool,
    truncated: bool,
    elapsed_ms: u128,
}

#[derive(Deserialize)]
struct ExecutionResultWire {
    exit_code: i32,
    stdout: String,
    stderr: String,
    timed_out: bool,
    cancelled: bool,
    truncated: bool,
    elapsed_ms: u128,
}

impl Serialize for ExecutionResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ExecutionResultWireRef {
            exit_code: self.exit_code,
            stdout: &self.output.stdout,
            stderr: &self.output.stderr,
            timed_out: self.outcome == ExecutionOutcome::TimedOut,
            cancelled: self.outcome == ExecutionOutcome::Cancelled,
            truncated: self.output.truncated,
            elapsed_ms: self.elapsed_ms,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExecutionResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ExecutionResultWire::deserialize(deserializer)?;
        // Old results could represent both flags at once. Cancellation takes
        // precedence when migrating that invalid state into the typed model.
        let outcome = if wire.cancelled {
            ExecutionOutcome::Cancelled
        } else if wire.timed_out {
            ExecutionOutcome::TimedOut
        } else {
            ExecutionOutcome::Exited
        };
        Ok(Self {
            exit_code: wire.exit_code,
            outcome,
            output: CapturedOutput {
                stdout: wire.stdout,
                stderr: wire.stderr,
                truncated: wire.truncated,
            },
            elapsed_ms: wire.elapsed_ms,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorStatus {
    pub process_group: i32,
    pub elapsed: Duration,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum ActivePhase {
    Starting,
    Running { process_group: i32 },
    Draining,
}

#[derive(Debug)]
struct ActiveRun {
    started: Instant,
    phase: ActivePhase,
    cancelled: bool,
    stdout_bytes: Arc<AtomicU64>,
    stderr_bytes: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct ExecutorState {
    active: Option<ActiveRun>,
}

#[derive(Debug, Clone)]
pub struct Executor {
    config: ExecutorConfig,
    state: Arc<Mutex<ExecutorState>>,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Result<Self, ExecutorError> {
        std::fs::create_dir_all(&config.working_directory).map_err(|source| {
            ExecutorError::WorkingDirectory {
                operation: "create",
                path: config.working_directory.clone(),
                source,
            }
        })?;
        let working_directory =
            std::fs::canonicalize(&config.working_directory).map_err(|source| {
                ExecutorError::WorkingDirectory {
                    operation: "canonicalize",
                    path: config.working_directory.clone(),
                    source,
                }
            })?;
        unix::enable_child_subreaping().map_err(ExecutorError::Subreaper)?;
        Ok(Self {
            config: ExecutorConfig {
                working_directory,
                ..config
            },
            state: Arc::new(Mutex::new(ExecutorState::default())),
        })
    }

    pub fn is_running(&self) -> bool {
        self.lock_state().active.is_some()
    }

    pub fn active_status(&self) -> Option<ExecutorStatus> {
        let state = self.lock_state();
        let active = state.active.as_ref()?;
        let ActivePhase::Running { process_group } = active.phase else {
            return None;
        };
        Some(ExecutorStatus {
            process_group,
            elapsed: active.started.elapsed(),
            stdout_bytes: active.stdout_bytes.load(Ordering::Relaxed),
            stderr_bytes: active.stderr_bytes.load(Ordering::Relaxed),
        })
    }

    pub fn cancel(&self) -> Result<bool, ExecutorError> {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            return Ok(false);
        };
        active.cancelled = true;
        if let ActivePhase::Running { process_group } = active.phase {
            // Holding the state lock ensures cleanup cannot retire this PGID
            // before the signal attempt. ESRCH means it already exited.
            unix::signal_group(process_group, libc::SIGTERM).map_err(|source| {
                ExecutorError::Signal {
                    process_group,
                    signal: libc::SIGTERM,
                    source,
                }
            })?;
        }
        Ok(true)
    }

    pub fn run(&self, command: &str) -> Result<ExecutionResult, ExecutorError> {
        self.run_with_timeout(command, self.config.timeout)
    }

    pub fn run_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecutionResult, ExecutorError> {
        let started = Instant::now();
        let stdout_progress = Arc::new(AtomicU64::new(0));
        let stderr_progress = Arc::new(AtomicU64::new(0));
        // Declared before the mutex guard so unwinding drops the mutex guard
        // first. Once armed, this guard releases the reservation and, after a
        // successful spawn, owns process-group cleanup on every exit path.
        let mut run_guard = RunGuard::new(self);

        let mut state = self.lock_state();
        if state.active.is_some() {
            return Err(ExecutorError::AlreadyRunning);
        }
        state.active = Some(ActiveRun {
            started,
            phase: ActivePhase::Starting,
            cancelled: false,
            stdout_bytes: stdout_progress.clone(),
            stderr_bytes: stderr_progress.clone(),
        });
        run_guard.arm();

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.config.working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unix::configure_process_session(&mut cmd);
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(source) => {
                drop(state);
                return Err(ExecutorError::Spawn(source));
            }
        };
        let process_group = child.id() as i32;
        state.active.as_mut().expect("reserved active run").phase =
            ActivePhase::Running { process_group };
        run_guard.install_process(child, process_group);
        drop(state);

        let stdout = run_guard
            .child_mut()
            .stdout
            .take()
            .ok_or(ExecutorError::MissingPipe("stdout"))?;
        let stderr = run_guard
            .child_mut()
            .stderr
            .take()
            .ok_or(ExecutorError::MissingPipe("stderr"))?;
        let limit = self.config.output_limit;
        let out_reader = thread::spawn(move || read_bounded(stdout, limit, stdout_progress));
        let err_reader = thread::spawn(move || read_bounded(stderr, limit, stderr_progress));

        let mut status: Option<ExitStatus> = None;
        let outcome = loop {
            if self.active_cancelled() {
                break ExecutionOutcome::Cancelled;
            }
            if started.elapsed() >= timeout {
                break ExecutionOutcome::TimedOut;
            }
            match run_guard.child_mut().try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    break ExecutionOutcome::Exited;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(source) => {
                    return Err(ExecutorError::Process {
                        operation: "poll",
                        source,
                    });
                }
            }
        };

        // Always retire the whole group. A successful shell may leave a
        // background descendant holding an output pipe.
        let exit = run_guard.retire(status)?;
        let out_result = join_reader(out_reader, "stdout");
        let err_result = join_reader(err_reader, "stderr");
        let (stdout_bytes, out_truncated) = out_result?;
        let (stderr_bytes, err_truncated) = err_result?;

        Ok(ExecutionResult {
            exit_code: exit.code().unwrap_or(-1),
            outcome,
            output: CapturedOutput {
                stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
                truncated: out_truncated || err_truncated,
            },
            elapsed_ms: started.elapsed().as_millis(),
        })
    }

    fn lock_state(&self) -> MutexGuard<'_, ExecutorState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn active_cancelled(&self) -> bool {
        self.lock_state()
            .active
            .as_ref()
            .is_some_and(|active| active.cancelled)
    }
}

struct RunGuard<'a> {
    executor: &'a Executor,
    armed: bool,
    process: Option<(Child, i32)>,
}

impl<'a> RunGuard<'a> {
    fn new(executor: &'a Executor) -> Self {
        Self {
            executor,
            armed: false,
            process: None,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn install_process(&mut self, child: Child, process_group: i32) {
        self.process = Some((child, process_group));
    }

    fn child_mut(&mut self) -> &mut Child {
        &mut self.process.as_mut().expect("spawned process guard").0
    }

    fn begin_draining(&self) {
        if !self.armed {
            return;
        }
        if let Some(active) = self.executor.lock_state().active.as_mut() {
            active.phase = ActivePhase::Draining;
        }
    }

    fn retire(&mut self, status: Option<ExitStatus>) -> Result<ExitStatus, ExecutorError> {
        self.begin_draining();
        let (child, process_group) = self.process.as_mut().expect("spawned process guard");
        let exit = unix::retire_process_group(child, *process_group, status)
            .map_err(|failure| failure.into_executor_error(*process_group))?;
        self.process = None;
        Ok(exit)
    }
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.begin_draining();
        if let Some((child, process_group)) = self.process.as_mut()
            && let Err(failure) = unix::retire_process_group(child, *process_group, None)
        {
            eprintln!(
                "executor cleanup failed: {}",
                failure.into_executor_error(*process_group)
            );
        }
        if self.armed {
            self.executor.lock_state().active = None;
        }
    }
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<(Vec<u8>, bool)>>,
    stream: &'static str,
) -> Result<(Vec<u8>, bool), ExecutorError> {
    reader
        .join()
        .map_err(|_| ExecutorError::ReaderPanicked(stream))?
        .map_err(|source| ExecutorError::OutputRead { stream, source })
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    byte_count: Arc<AtomicU64>,
) -> io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::with_capacity(limit.min(8192));
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        byte_count.fetch_add(n as u64, Ordering::Relaxed);
        let room = limit.saturating_sub(kept.len());
        if room > 0 {
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        if n > room {
            truncated = true;
        }
    }
    Ok((kept, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingReader(bool);

    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("injected read failure"));
            }
            self.0 = true;
            buffer[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }

    #[test]
    fn reservation_guard_releases_during_unwind() {
        let executor = Executor {
            config: ExecutorConfig::with_working_directory("unused"),
            state: Arc::new(Mutex::new(ExecutorState::default())),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = RunGuard::new(&executor);
            executor.lock_state().active = Some(ActiveRun {
                started: Instant::now(),
                phase: ActivePhase::Starting,
                cancelled: false,
                stdout_bytes: Arc::new(AtomicU64::new(0)),
                stderr_bytes: Arc::new(AtomicU64::new(0)),
            });
            guard.arm();
            panic!("injected panic");
        }));
        assert!(!executor.is_running());
    }

    #[test]
    fn bounded_reader_propagates_read_errors() {
        let progress = Arc::new(AtomicU64::new(0));
        let error = read_bounded(FailingReader(false), 10, progress.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(progress.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn result_wire_format_remains_flat_and_boolean_compatible() {
        let result = ExecutionResult {
            exit_code: -1,
            outcome: ExecutionOutcome::TimedOut,
            output: CapturedOutput {
                stdout: "out".into(),
                stderr: "err".into(),
                truncated: true,
            },
            elapsed_ms: 12,
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({
                "exit_code": -1,
                "stdout": "out",
                "stderr": "err",
                "timed_out": true,
                "cancelled": false,
                "truncated": true,
                "elapsed_ms": 12
            })
        );
    }

    #[test]
    fn legacy_overlapping_flags_deserialize_to_one_outcome() {
        let result: ExecutionResult = serde_json::from_value(serde_json::json!({
            "exit_code": -1,
            "stdout": "",
            "stderr": "",
            "timed_out": true,
            "cancelled": true,
            "truncated": false,
            "elapsed_ms": 1
        }))
        .unwrap();
        assert_eq!(result.outcome, ExecutionOutcome::Cancelled);
    }
}
