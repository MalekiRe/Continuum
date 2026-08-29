//! Persistent Agent Lisp Harness — Continuous Agent
//!
//! "Unless it explicitly returns or waits, the kernel immediately
//!  schedules its next turn. Human messages and control work take
//!  priority. There is no idle backoff."
//!     — The Design

use persistent_lisp_harness::{Kernel, Value, kernel::{self, SnapshotKind, FrameStatus}};
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn load_or_create_kernel() -> Kernel {
    if Path::new("snapshots").exists() {
        match Kernel::recover_from_latest() {
            Ok(k) => {
                println!("[kernel] recovered from snapshot");
                return k;
            }
            Err(e) => {
                println!("[kernel] recovery failed: {} — starting fresh", e);
            }
        }
    }
    let mut k = Kernel::new();
    k.register_tools();
    let _ = std::fs::create_dir_all("data");
    println!("[kernel] fresh start — version {}", k.version);
    k
}

fn main() {
    println!("╔══════════════════════════════════════════════╗");
    println!("║  Persistent Agent Lisp Harness v0.1.0       ║");
    println!("║  Continuous autonomous agent.                ║");
    println!("║  Type a message + Enter to interact.         ║");
    println!("║  Type '!!exit' to quit.                     ║");
    println!("╚══════════════════════════════════════════════╝");

    let mut kernel = load_or_create_kernel();
    kernel::set_kernel_hook(&mut kernel);

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

    match kernel.eval(agent_core) {
        Ok(_) => println!("[agent] core loaded"),
        Err(e) => println!("[agent] core: {}", e),
    }

    // Human input channel
    let (tx, rx) = mpsc::channel::<String>();
    let _input_thread = thread::spawn(move || {
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
    let mut call_start_time = Instant::now();

    loop {
        // === Check for human messages ===
        let human_msg = rx.try_recv().ok();
        if let Some(msg) = human_msg {
            if msg == "!!exit" || msg == "!!quit" {
                kernel.snapshot(SnapshotKind::Full);
                println!("[kernel] goodbye!");
                break;
            }
            kernel.human_message(&msg);
            println!("[human] delivered to agent frame");
        }

        // === Check hourly snapshot ===
        if hourly_timer.elapsed() >= Duration::from_secs(3600) {
            kernel.check_hourly_snapshot();
            hourly_timer = Instant::now();
        }

        // === Check 15-minute supervision ===
        if supervision_timer.elapsed() >= Duration::from_secs(900) {
            let decision = kernel::Scheduler::fifteen_minute_review(
                &kernel, chrono::Utc::now(), 15 * 60 * 1000
            );
            match &decision {
                kernel::ReviewDecision::Cancel(reason) => {
                    println!("[supervisor] cancelling: {}", reason);
                    kernel.record_event(
                        kernel::event_log::EventKind::Supervise {
                            action: "cancel".into(),
                            reason: reason.clone(),
                        },
                        kernel.current_frame_id(),
                    );
                    // Cancel current frame
                    if let Some(frame) = kernel.frames.last_mut() {
                        frame.status = FrameStatus::Completed;
                    }
                }
                kernel::ReviewDecision::Advice(advice) => {
                    println!("[supervisor] advice: {}", advice);
                    kernel.record_event(
                        kernel::event_log::EventKind::Supervise {
                            action: "advice".into(),
                            reason: advice.clone(),
                        },
                        kernel.current_frame_id(),
                    );
                }
                _ => {}
            }
            supervision_timer = Instant::now();
        }

        // === Check efficiency every 100 turns ===
        if kernel.event_counter % 100 == 0 && kernel.event_counter > 0 {
            if let Some(advice) = kernel::Scheduler::efficiency_review(&kernel) {
                println!("[supervisor] efficiency: {}", advice);
            }
        }

        // === Check if the root frame is gone (agent returned) ===
        if kernel.frames.is_empty() || 
           kernel.frames.iter().all(|f| f.status == FrameStatus::Completed) {
            println!("[agent] all frames completed. Restarting...");
            kernel.eval("(agent/loop \"Initial context\")").ok();
            continue;
        }

        // === Check if the current frame is waiting for a subagent result ===
        if let Some(result) = kernel.take_subagent_result() {
            println!("[agent] subagent returned: {}", result);
        }

        // === Check if the frame is waiting for human input ===
        if kernel.frames.last().map(|f| f.status == FrameStatus::Waiting).unwrap_or(false) {
            // Frame is waiting — check for pending messages
            if let Some(msg) = kernel.take_pending_message() {
                let eval_result = kernel.eval_repl(&format!(
                    r#"(agent/cognize "Human message received: {}")"#, msg
                ));
                // Don't println — agent printed its own output
                let _ = eval_result;
            }
            // No idle backoff — loop immediately
            continue;
        }

        // === Run the agent's next cognition turn ===
        let has_pending = kernel.frames.last()
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

        call_start_time = Instant::now();
        match kernel.eval(&source) {
            Ok(val) => {
                // Unless the agent explicitly returns/wait, schedule next turn immediately
                match &val {

                    _ => {
                        // Continue immediately — no backoff
                        continue;
                    }
                }
            }
            Err(e) => {
                println!("[agent] error: {}", e);
                // Error is not a reason to stop — keep going
                kernel.snapshot(SnapshotKind::Incremental);
                continue;
            }
        }
    }
}
