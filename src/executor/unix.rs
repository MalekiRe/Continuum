use super::ExecutorError;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const TERMINATION_GRACE: Duration = Duration::from_millis(100);

pub(super) fn configure_process_session(command: &mut Command) {
    // SAFETY: pre_exec runs after fork and before exec. The closure performs
    // only the async-signal-safe setsid syscall and constructs an io::Error
    // from errno on failure; it captures no parent-process state.
    unsafe {
        command.pre_exec(|| {
            // SAFETY: setsid takes no pointers and affects only the calling
            // child, making it the leader of a new session/process group.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Signals a process group. A missing group is already in the desired state.
pub(super) fn signal_group(process_group: i32, signal: i32) -> io::Result<()> {
    // SAFETY: kill receives a negated, positive child PGID and an OS signal
    // number. It dereferences no pointers. The negative PID targets the group.
    let result = unsafe { libc::kill(-process_group, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn process_group_exists(process_group: i32) -> io::Result<bool> {
    // SAFETY: signal zero performs existence/permission checking only. The
    // negative, positive child PGID targets a group and no pointers are used.
    let result = unsafe { libc::kill(-process_group, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

pub(super) fn retire_process_group(
    child: &mut Child,
    process_group: i32,
    mut status: Option<ExitStatus>,
) -> Result<ExitStatus, ExecutorError> {
    let signal_error = |signal, source| ExecutorError::Signal {
        process_group,
        signal,
        source,
    };
    let process_error = |operation, source| ExecutorError::Process { operation, source };
    signal_group(process_group, libc::SIGTERM)
        .map_err(|source| signal_error(libc::SIGTERM, source))?;
    let deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < deadline {
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|source| process_error("poll during cleanup", source))?;
        }
        if !process_group_exists(process_group).map_err(|source| signal_error(0, source))? {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    signal_group(process_group, libc::SIGKILL)
        .map_err(|source| signal_error(libc::SIGKILL, source))?;
    let status = match status {
        Some(status) => status,
        None => child
            .wait()
            .map_err(|source| process_error("reap", source))?,
    };
    reap_group_children(process_group)
        .map_err(|source| process_error("reap process-group descendant", source))?;
    Ok(status)
}

#[cfg(target_os = "linux")]
pub(super) fn enable_child_subreaping() -> io::Result<()> {
    // SAFETY: prctl(PR_SET_CHILD_SUBREAPER) accepts an integer flag and no
    // pointers. Enabling it is process-wide and is intentionally idempotent.
    let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn enable_child_subreaping() -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reap_group_children(process_group: i32) -> io::Result<()> {
    loop {
        let mut status = 0;
        // SAFETY: waitpid writes to a valid local status pointer. A negative
        // PGID waits only for adopted children in the executor's process group.
        let waited = unsafe { libc::waitpid(-process_group, &mut status, 0) };
        if waited > 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Ok(()),
            _ => return Err(error),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn reap_group_children(_process_group: i32) -> io::Result<()> {
    Ok(())
}
