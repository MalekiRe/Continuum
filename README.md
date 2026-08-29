# Persistent Agent Lisp Harness

A **continuously existing agent** whose computer is a persistent Lisp world.

```
Model
  ↕
Persistent Lisp VM
  ↕
Immutable Rust kernel
  ↕
Tools, APIs, processes, storage
```

## Architecture

**Kernel** (Rust, ~3,800 lines) owns the Lisp VM, persistent namespaces with versioned history, model & subagent scheduling, snapshots & crash recovery, native functions, the event log & artifact store, context compaction, human interrupts, and execution supervision.

**Lisp** owns everything else. The kernel provides only the primitives the VM cannot exist without — arithmetic, type predicates, list operations, I/O, persistence, inspection, and a `bash` function for executing shell commands. File I/O, web requests, string manipulation, model inference, and all higher-level tools are Lisp that the agent defines for itself.

## Kernel Natives

**Arithmetic:** `+` `-` `*` `/` `<` `=` `>`
**Lists:** `cons` `car` `cdr` `list`
**I/O:** `display` `println` `read`
**Types:** `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?`
**Control:** `continue` `cancel-current` `error`
**System:** `system/clock` `system/version` `system/interrupt` `system/clear-interrupt`
**Persistence:** `system/snapshot` `system/compact` `system/event-log`
**Inspection:** `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/source` `inspect/history`
**History:** `history/read` `history/zoom` `history/find`
**Subagents:** `agent/call`
**Shell:** `bash(cmd)` — the universal tool interface
**Scheduling:** `wake(ms, action)` — timer-based interrupts
**Utilities:** `map/get` `vector/get`

## Language

A small Scheme-like Lisp with familiar special forms (`define`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote`) and:
- `define-syntax` with `syntax-rules` (including ellipsis)
- `define-data` for tagged value families with automatic constructor functions
- `match` with constructor pattern destructuring
- Tagged values: `(Ok value)`, `(Err problem)`, `(Cancelled reason)`, `(Indeterminate problem)`
- Opaque kernel references (`#<process 12345>`)

## Snapshots & Recovery

Every top-level Lisp call commits a snapshot before evaluation. Recovery loads the saved image directly — it never replays execution. Snapshots are versioned, checksummed (SHA256), atomically committed, and rotated. After recovery, every active frame receives a `(system/Restarted :kind :unclean :downtime ...)` notice.

## Subagents

`(agent/call 'name request)` spawns a child frame. The caller pauses, the child runs, and the child's return value is delivered to the caller. Only one model invocation runs at a time.

## Human Interaction

Messages are queued as interrupts to every active frame. Current work is suspended at the next safepoint, the interaction runs, and returns `control/Continue` or `(control/CancelCurrent ...)`.

## Supervision

- **Efficiency review**: compares token generation with time spent waiting in tool calls.
- **15-minute review**: if a top-level call runs for 15 minutes without returning to cognition, it's reviewed.

## Quick Start

```bash
cargo run
```

Starts a continuous agent REPL. The agent lives until `!!exit`. Type any Lisp expression:

```lisp
lisp> (+ 1 2)
3
lisp> (bash "echo hello")
{:exit 0 :stdout "hello\n" :stderr ""}
lisp> (define-data result/Result (Ok value) (Err problem))
result/Result
lisp> (match (user/result/Result/Ok 42)
         ((result/Result/Ok n) (+ n 1))
         ((result/Result/Err msg) -1))
43
```

Snapshots happen automatically on every evaluation. Recover from a crash with `cargo run` — it auto-detects the latest snapshot.

## Design Principles

1. **Kernel is minimal.** Only what the VM needs to function. Everything else is Lisp.
2. **`bash` is the universal tool interface.** File I/O, web requests, network calls — all go through shell commands.
3. **Definitions persist.** Every `define` creates a new version. `undefine` removes the current binding without erasing history.
4. **Snapshots are atomic.** You can never save mid-call. Recovery restores the exact pre-call state.
5. **The kernel can always interrupt Lisp.** A safepoint mechanism checks every 1000 expressions.
