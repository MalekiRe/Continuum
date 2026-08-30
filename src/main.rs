//! Persistent Agent Lisp Harness — Continuous Agent
//!
//! "Unless it explicitly returns or waits, the kernel immediately
//!  schedules its next turn. Human messages and control work take
//!  priority. There is no idle backoff."
//!     — The Design

use persistent_lisp_harness::kernel::{self, SnapshotKind, FrameStatus};
use persistent_lisp_harness::Kernel;
use persistent_lisp_harness::EnvRef;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn load_or_create_kernel() -> (&'static mut Kernel, EnvRef) {
    if Path::new("snapshots").exists() {
        match Kernel::recover_from_latest() {
            Ok((k, env)) => {
                let ptr = Box::leak(Box::new(k));
                println!("[kernel] recovered from snapshot");
                return (ptr, env);
            }
            Err(e) => {
                println!("[kernel] recovery failed: {} — starting fresh", e);
            }
        }
    }
    let (k, env) = Kernel::new();
    let _ = std::fs::create_dir_all("data");
    let ptr = Box::leak(Box::new(k));
    println!("[kernel] fresh start — version {}", ptr.version);
    (ptr, env)
}

/// Check for human messages from stdin. Returns true if exit was requested.
fn handle_human_input(kernel: &mut Kernel, env: &EnvRef, rx: &mpsc::Receiver<String>) -> bool {
    let human_msg = rx.try_recv().ok();
    if let Some(msg) = human_msg {
        if msg == "!!exit" || msg == "!!quit" {
            kernel.snapshot(SnapshotKind::Full, env);
            println!("[kernel] goodbye!");
            return true;
        }
        kernel.human_message(&msg);
        println!("[human] delivered to agent frame");
    }
    false
}

/// Take an hourly full snapshot if due.
fn check_hourly_snapshot(kernel: &mut Kernel, env: &EnvRef, timer: &mut Instant) {
    if timer.elapsed() >= Duration::from_secs(3600) {
        kernel.check_hourly_snapshot(env);
        *timer = Instant::now();
    }
}

/// Check 15-minute supervision: if a top-level call runs too long, review it.
fn check_supervision(kernel: &mut Kernel, timer: &mut Instant) {
    if timer.elapsed() >= Duration::from_secs(900) {
        let decision = kernel::Scheduler::fifteen_minute_review(
            kernel, chrono::Utc::now(), 15 * 60 * 1000,
        );
        match &decision {
            kernel::ReviewDecision::Cancel(reason) => {
                println!("[supervisor] cancelling: {}", reason);
                if let Some(frame) = kernel.frames.last_mut() {
                    frame.status = FrameStatus::Completed;
                }
            }
            kernel::ReviewDecision::Advice(advice) => {
                println!("[supervisor] advice: {}", advice);
            }
            _ => {}
        }
        *timer = Instant::now();
    }
}

/// Check if the agent's root frame has completed and needs restarting.
fn maybe_restart_agent(kernel: &mut Kernel, env: &mut EnvRef) -> bool {
    if kernel.frames.is_empty()
        || kernel.frames.iter().all(|f| f.status == FrameStatus::Completed)
    {
        println!("[agent] all frames completed. Restarting...");
        kernel.eval("(agent/loop \"Initial context\")", env).ok();
        true
    } else {
        false
    }
}

/// Check for a pending subagent result and deliver it to the agent.
fn handle_subagent_result(kernel: &mut Kernel) {
    if let Some(result) = kernel.take_subagent_result() {
        println!("[agent] subagent returned: {}", result);
    }
}

/// Check if the current frame is waiting for human input, and deliver it.
fn handle_waiting_frame(kernel: &mut Kernel, env: &mut EnvRef) -> bool {
    let is_waiting = kernel
        .frames
        .last()
        .map(|f| f.status == FrameStatus::Waiting)
        .unwrap_or(false);

    if is_waiting {
        if let Some(msg) = kernel.take_pending_message() {
            let _ = kernel.eval_repl(&format!(
                r#"(agent/cognize "Human message received: {}")"#,
                msg
            ), env);
        }
        true
    } else {
        false
    }
}

/// Run one cognition turn for the agent.
fn run_cognition_turn(kernel: &mut Kernel, env: &mut EnvRef) {
    let has_pending = kernel
        .frames
        .last()
        .map(|f| !f.message_queue.is_empty())
        .unwrap_or(false);

    let source = if has_pending {
        if let Some(msg) = kernel.take_pending_message() {
            format!("(agent/cognize \"Human message: {}\")", msg)
        } else {
            "(agent/loop nil)".to_string()
        }
    } else {
        "(agent/loop nil)".to_string()
    };

    match kernel.eval(&source, env) {
        Ok(_) => {
            // Continue immediately — no backoff
        }
        Err(e) => {
            println!("[agent] error: {}", e);
            kernel.snapshot(SnapshotKind::Incremental, env);
        }
    }
}

fn main() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║  Persistent Agent Lisp Harness v0.1.0       ║");
    println!("║  Continuous autonomous agent.                ║");
    println!("║  Type a message + Enter to interact.         ║");
    println!("║  Type '!!exit' to quit.                     ║");
    println!("╚══════════════════════════════════════════════╝");

    let (kernel, mut env) = load_or_create_kernel();

    // Load agent core
    let agent_core = r#"
        (define-data result/Result
          (Ok value)
          (Err problem)
          (Cancelled reason)
          (Indeterminate problem))

        (define (agent/cognize context)
          (println "[agent] context:" context)
          context)

        (define (agent/loop context)
          (agent/loop (agent/cognize context)))
    "#;

    match kernel.eval(agent_core, &mut env) {
        Ok(_) => println!("[agent] core loaded"),
        Err(e) => println!("[agent] core: {}", e),
    }

    // Human input channel
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(input) => {
                    let input = input.trim().to_string();
                    if !input.is_empty() {
                        let _ = tx.send(input);
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Continuous cognition loop
    let mut hourly_timer = Instant::now();
    let mut supervision_timer = Instant::now();

    loop {
        if handle_human_input(kernel, &env, &rx) {
            break;
        }
        check_hourly_snapshot(kernel, &env, &mut hourly_timer);
        check_supervision(kernel, &mut supervision_timer);


        if maybe_restart_agent(kernel, &mut env) {
            continue;
        }
        handle_subagent_result(kernel);

        if handle_waiting_frame(kernel, &mut env) {
            continue;
        }

        run_cognition_turn(kernel, &mut env);
    }
}
