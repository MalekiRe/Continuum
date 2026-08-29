
//! Persistent Agent Lisp Harness — Continuous Agent REPL
//!
//! The agent runs continuously. Unless it explicitly returns or waits,
//! the kernel immediately schedules its next turn. Human messages
//! interrupt and take priority.

use persistent_lisp_harness::{Kernel, Value, kernel::{self, SnapshotKind, FrameStatus}};
use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
    println!("║  Continuous autonomous agent environment.    ║");
    println!("║  Type a message to the agent at any time.    ║");
    println!("║  Type '!!exit' to quit.                     ║");
    println!("╚══════════════════════════════════════════════╝");

    let mut kernel = load_or_create_kernel();
    kernel::set_kernel_hook(&mut kernel);

    // Load the agent core library
    let agent_core = r#"
        ;; Agent core library — continuous cognition loop

        ;; The agent's main cognition function.
        ;; Override this to customize behavior.
        (define (agent/cognize context)
          (let ((result (agent/think context "What should I do next?")))
            (match result
              ((result/Ok text)
               (println "[agent] ~" text)
               text)
              ((result/Err msg)
               (println "[agent] error: ~" msg)
               nil))))

        ;; The main agent loop.
        ;; Calls (agent/cognize) until explicitly stopped.
        (define (agent/start)
          (begin
            (println "[agent] started continuous cognition")
            (agent/loop "Initial context")))

        (define (agent/loop context)
          (let ((result (agent/cognize context)))
            (begin
              (system/snapshot)
              (agent/loop (string/join " " (list "Previous context:" context "Result:" result))))))

        ;; Entry point
        (agent/start)
    "#;

    match kernel.eval(agent_core) {
        Ok(val) => println!("[agent] core loaded: {}", val),
        Err(e) => println!("[agent] core load warning: {}", e),
    }

    // Set up human input channel
    let (tx, rx) = mpsc::channel::<String>();
    let _input_thread = thread::spawn(move || {
        loop {
            let mut input = String::new();
            io::stdin().read_line(&mut input).unwrap();
            let input = input.trim().to_string();
            if !input.is_empty() {
                tx.send(input).unwrap_or(());
            }
        }
    });

    // Agent loop — runs cognition turns, checks for human messages
    let mut last_snapshot = std::time::Instant::now();

    loop {
        // Check for human messages (non-blocking)
        let human_msg = rx.try_recv().ok();

        if let Some(msg) = human_msg {
            if msg == "!!exit" || msg == "!!quit" {
                kernel.snapshot(SnapshotKind::Full);
                println!("[kernel] goodbye!");
                break;
            }

            // Deliver human message as interrupt
            kernel.human_message(&msg);
            println!("[human] message delivered to agent frame");
        }

        // Check if current frame is waiting — if so, wait for human input
        if kernel.frames.last().map(|f| f.status == kernel::FrameStatus::Waiting).unwrap_or(false) {
            // Frame is waiting — pause briefly and check again
            thread::sleep(Duration::from_millis(100));

            // Take a snapshot periodically
            if last_snapshot.elapsed() > Duration::from_secs(300) {
                kernel.snapshot(SnapshotKind::Incremental);
                last_snapshot = std::time::Instant::now();
            }
            continue;
        }

        // Check if the root frame is gone (agent returned)
        if kernel.frames.is_empty() || 
           kernel.frames.iter().all(|f| f.status == kernel::FrameStatus::Completed) {
            println!("[agent] all frames completed. Restarting...");
            kernel.eval("(agent/start)").ok();
            continue;
        }

        // Check for pending subagent results
        if let Some(result) = kernel.take_subagent_result() {
            println!("[agent] subagent returned: {}", result);
        }

        // Run a Lisp cognition turn
        match kernel.eval("(agent/loop nil)") {
            Ok(val) => {
                // Unless the agent explicitly returns/wait, schedule next turn
                let should_continue = !matches!(&val, 
                    Value::Keyword(s) if s == "Return" || s == "Wait");

                if !should_continue {
                    println!("[agent] pause requested");
                    thread::sleep(Duration::from_millis(500));
                }
            }
            Err(e) => {
                println!("[agent] cognition error: {}", e);
                // Take snapshot on error to preserve state
                kernel.snapshot(SnapshotKind::Incremental);
                thread::sleep(Duration::from_millis(1000));
            }
        }

        // Periodic snapshot
        if last_snapshot.elapsed() > Duration::from_secs(300) {
            kernel.snapshot(SnapshotKind::Incremental);
            last_snapshot = std::time::Instant::now();
        }

        // Brief yield to avoid busy-waiting
        thread::sleep(Duration::from_millis(50));
    }
}
