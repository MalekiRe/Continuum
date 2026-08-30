use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;
const TERMINATION_GRACE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub root: PathBuf,
    pub timeout: Duration,
    pub output_limit: usize,
}

impl ExecutorConfig {
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            timeout: Duration::from_secs(60),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub truncated: bool,
    pub elapsed_ms: u128,
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
    // This phase is only visible to the thread holding the state lock while it
    // spawns. It lets that thread reserve the executor before a PID exists.
    Starting,
    Running { process_group: i32 },
    // Keep the run reserved while its pipe readers finish, but discard the
    // PGID as soon as the group is gone so it can never refer to a reused PID.
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
    pub fn new(config: ExecutorConfig) -> Result<Self, String> {
        std::fs::create_dir_all(&config.root).map_err(|e| {
            format!(
                "cannot create executor root {}: {}",
                config.root.display(),
                e
            )
        })?;
        let root = std::fs::canonicalize(&config.root).map_err(|e| {
            format!(
                "cannot canonicalize executor root {}: {}",
                config.root.display(),
                e
            )
        })?;
        enable_child_subreaping()?;
        Ok(Self {
            config: ExecutorConfig { root, ..config },
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

    pub fn cancel(&self) -> bool {
        let mut state = self.lock_state();
        let Some(active) = state.active.as_mut() else {
            // Cancellation belongs to a particular active run. In particular,
            // an idle cancellation must not be remembered by the next run.
            return false;
        };
        active.cancelled = true;
        if let ActivePhase::Running { process_group } = active.phase {
            // The state lock keeps cleanup from discarding this PGID (and thus
            // allowing it to be reused) until after this signal is sent.
            signal_group(process_group, libc::SIGTERM);
        }
        true
    }

    pub fn run(&self, command: &str) -> Result<ExecutionResult, String> {
        self.run_with_timeout(command, self.config.timeout)
    }

    pub fn run_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> Result<ExecutionResult, String> {
        use std::os::unix::process::CommandExt;

        let started = Instant::now();
        let stdout_progress = Arc::new(AtomicU64::new(0));
        let stderr_progress = Arc::new(AtomicU64::new(0));

        // Reserve the one execution slot and retain the lock through spawn.
        // cancel() can therefore never observe a half-installed PID/PGID.
        let mut state = self.lock_state();
        if state.active.is_some() {
            return Err("executor is already running a command".into());
        }
        state.active = Some(ActiveRun {
            started,
            phase: ActivePhase::Starting,
            cancelled: false,
            stdout_bytes: stdout_progress.clone(),
            stderr_bytes: stderr_progress.clone(),
        });

        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.config.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawn_result = cmd.spawn();
        let mut child = match spawn_result {
            Ok(child) => child,
            Err(error) => {
                state.active = None;
                return Err(format!("failed to spawn bash: {error}"));
            }
        };
        let process_group = child.id() as i32;
        state.active.as_mut().expect("reserved active run").phase =
            ActivePhase::Running { process_group };
        drop(state);

        // These are guaranteed by Stdio::piped(). If that invariant is ever
        // broken, clean up the child/group before releasing the run slot.
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => return self.fail_after_spawn(child, process_group, "missing child stdout"),
        };
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => return self.fail_after_spawn(child, process_group, "missing child stderr"),
        };
        let limit = self.config.output_limit;
        let out_reader = thread::spawn(move || read_bounded(stdout, limit, stdout_progress));
        let err_reader = thread::spawn(move || read_bounded(stderr, limit, stderr_progress));

        let mut timed_out = false;
        let mut status: Option<ExitStatus> = None;
        let mut poll_error = None;
        loop {
            let cancelled = self.active_cancelled();
            if cancelled {
                break;
            }
            if started.elapsed() >= timeout {
                timed_out = true;
                break;
            }
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    poll_error = Some(format!("failed to poll bash: {error}"));
                    break;
                }
            }
        }

        // Even a successful/early-exiting bash can leave background members
        // holding its output pipes. Always retire the *whole* process group
        // before joining readers or declaring the run complete.
        let cleanup_result = retire_process_group(&mut child, process_group, status);
        {
            let mut state = self.lock_state();
            if let Some(active) = state.active.as_mut() {
                active.phase = ActivePhase::Draining;
            }
        }

        let out_result = out_reader
            .join()
            .map_err(|_| "stdout reader panicked".to_string());
        let err_result = err_reader
            .join()
            .map_err(|_| "stderr reader panicked".to_string());

        let cancelled = self.active_cancelled();
        self.lock_state().active = None;

        if let Some(error) = poll_error {
            return Err(error);
        }
        let status = cleanup_result?;
        let (stdout_bytes, out_truncated) = out_result?;
        let (stderr_bytes, err_truncated) = err_result?;
        Ok(ExecutionResult {
            exit_code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            timed_out,
            cancelled,
            truncated: out_truncated || err_truncated,
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

    fn fail_after_spawn(
        &self,
        mut child: Child,
        process_group: i32,
        message: &str,
    ) -> Result<ExecutionResult, String> {
        let _ = retire_process_group(&mut child, process_group, None);
        self.lock_state().active = None;
        Err(message.into())
    }
}

fn signal_group(process_group: i32, signal: i32) {
    unsafe {
        libc::kill(-process_group, signal);
    }
}

fn process_group_exists(process_group: i32) -> bool {
    let result = unsafe { libc::kill(-process_group, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn retire_process_group(
    child: &mut Child,
    process_group: i32,
    mut status: Option<ExitStatus>,
) -> Result<ExitStatus, String> {
    // TERM gives shells a brief chance to run their normal signal cleanup.
    // It is also sent after a normal leader exit: other members can still own
    // stdout/stderr and would otherwise make reader joins wait indefinitely.
    signal_group(process_group, libc::SIGTERM);
    let deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < deadline {
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|e| format!("failed to poll bash during cleanup: {e}"))?;
        }
        if !process_group_exists(process_group) {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }

    // Once SIGKILL has been delivered no live group member can fork or retain
    // an output descriptor. Reap our direct child before returning.
    signal_group(process_group, libc::SIGKILL);
    let status = if let Some(status) = status {
        status
    } else {
        child
            .wait()
            .map_err(|e| format!("failed to reap bash: {e}"))?
    };
    reap_group_children(process_group)?;
    Ok(status)
}

#[cfg(target_os = "linux")]
fn enable_child_subreaping() -> Result<(), String> {
    let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if result == -1 {
        Err(format!(
            "failed to make executor a child subreaper: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
fn enable_child_subreaping() -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_group_children(process_group: i32) -> Result<(), String> {
    loop {
        let mut status = 0;
        let waited = unsafe { libc::waitpid(-process_group, &mut status, 0) };
        if waited > 0 {
            continue;
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Ok(()),
            _ => return Err(format!("failed to reap process-group descendant: {error}")),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_group_children(_process_group: i32) -> Result<(), String> {
    Ok(())
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    byte_count: Arc<AtomicU64>,
) -> (Vec<u8>, bool) {
    let mut kept = Vec::with_capacity(limit.min(8192));
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                byte_count.fetch_add(n as u64, Ordering::Relaxed);
                let room = limit.saturating_sub(kept.len());
                if room > 0 {
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                if n > room {
                    truncated = true;
                }
            }
        }
    }
    (kept, truncated)
}
