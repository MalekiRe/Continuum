
//! Persistent Agent Lisp Harness - REPL
use persistent_lisp_harness::{Kernel, kernel::{self, SnapshotKind}};
use std::io::{self, Write};
use std::path::Path;

/// Load or create kernel with recovery.
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
    println!("╔══════════════════════════════════════════╗");
    println!("║  Persistent Agent Lisp Harness v0.1.0   ║");
    println!("║  Type (help) for info, (exit) to quit.  ║");
    println!("╚══════════════════════════════════════════╝");
    println!();

    let mut kernel = load_or_create_kernel();

    // Set up kernel hook so system/natives can access the kernel
    kernel::set_kernel_hook(&mut kernel);

    // Startup definitions
    let startup = r#"
        (define (help)
          (println "=== Persistent Agent Lisp Harness ===")
          (println "Special forms: define, undefine, lambda, if, begin, let, let*, letrec, set!")
          (println "              quote, quasiquote, define-syntax, define-data, match")
          (println "Natives:       +, -, *, /, =, <, >, cons, car, cdr, list")
          (println "              display, println, read, nil?, number?, symbol?, string?")
          (println "              list?, function?, keyword?")
          (println "System:        system/version, system/clock, system/snapshot")
          (println "              system/compact, system/event-log")
          (println "Control:       control/Continue, control/Wait, control/Return")
          (println "Tools:         web/search, fs/read, fs/write, proc/run, message/reply")
          (println "              clock/wake, agent/call, string/join, string/split")
          (println "              map/get, vector/get")
          (println "")
          (println "Meta:          (system/snapshot) — save state")
          (println "               (system/event-log) — show event log")
          (println "               (inspect/namespaces) — list namespaces")
          (println "               (inspect/history 'name) — show version history")
          (println "               (exit) — quit"))

        (define-data result/Result
          (Ok value)
          (Err problem)
          (Cancelled reason)
          (Indeterminate problem))
    "#;

    match kernel.eval(startup) {
        Ok(_) => println!("[lisp] startup loaded"),
        Err(e) => println!("[lisp] startup warning: {}", e),
    }

    // Main REPL loop
    loop {
        let depth = kernel.frames.len();
        let prompt = if depth > 1 {
            format!("lisp[{}]> ", depth - 1)
        } else {
            "lisp> ".to_string()
        };

        print!("{}", prompt);
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        if input == "(exit)" || input == "exit" || input == "quit" {
            kernel.snapshot(SnapshotKind::Full);
            println!("Goodbye!");
            break;
        }

        // Multi-line input support
        let mut full_input = input.clone();
        let mut open_parens = input.matches('(').count() as i64
            - input.matches(')').count() as i64;
        while open_parens > 0 {
            print!("  ... ");
            io::stdout().flush().unwrap();
            let mut line = String::new();
            io::stdin().read_line(&mut line).unwrap();
            let line = line.trim().to_string();
            open_parens += line.matches('(').count() as i64
                - line.matches(')').count() as i64;
            full_input.push_str(" ");
            full_input.push_str(&line);
        }

        let result = kernel.eval_repl(&full_input);
        println!("{}", result);
    }

    kernel.snapshot(SnapshotKind::Full);
    println!("[kernel] final snapshot saved");
}
