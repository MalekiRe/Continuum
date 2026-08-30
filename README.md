# Continuum

A **continuously existing agent** whose computer is a persistent Lisp world.

```
Model/AI
  ↕
Lisp VM (continuum)
  ↕
Rust kernel
  ↕
Tools, APIs, processes, storage
```

The kernel is small. Lisp owns everything else. The kernel provides only what the VM cannot exist without — arithmetic, list operations, I/O, persistence, inspection, and a single `bash` function for executing shell commands. File I/O, web requests, string manipulation, model inference, and all higher-level tools are Lisp that the agent defines for itself.

## Architecture

**Kernel** (Rust, 3,145 lines) owns the Lisp VM, persistent namespaces with versioned history, subagent scheduling, snapshots & crash recovery, native functions, human interrupts, and execution supervision.

**EnvRef** lives on the `Kernel` struct itself — `kernel.env` is the single source of truth for all bindings. Every eval function receives `&mut Kernel` and accesses `kernel.env` at use sites. No globals, no thread-local storage, no raw pointer dereferencing anywhere.

**Lisp** is a Scheme-like language with modern affordances:

| Special forms | `define`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote`, `undefine` |
|---|---|
| Macros | `define-syntax` with `syntax-rules` (including `...` ellipsis for variable-length patterns) |
| Data types | `define-data` for tagged value families with automatic constructor functions |
| Pattern matching | `match` with constructor pattern destructuring |
| Tagged values | `(Ok value)`, `(Err problem)`, `(Cancelled reason)`, `(Indeterminate problem)` |

## Kernel Natives (41 functions)

**Arithmetic:** `+` `-` `*` `/` `<` `=` `>`
**Lists:** `cons` `car` `cdr` `list`
**I/O:** `display` `println` `read`
**Types:** `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?`
**Control:** `control/Continue` `control/CancelCurrent` `control/Error`
**System:** `system/clock` `system/version` `system/interrupt` `system/clear-interrupt`
**Persistence:** `system/snapshot` `system/event-log`
**Inspection:** `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/source` `inspect/history`
**Subagents:** `agent/call`
**Shell:** `bash(cmd)` — the universal tool interface. Everything outside the Lisp world goes through shell commands.
**Scheduling:** `wake(ms, action)` — fire-and-forget timer-based interrupts
**Utilities:** `map/get` `vector/get`

## Design

### The kernel can always interrupt Lisp
A safepoint mechanism checks a global atomic flag every 1,000 expressions. The kernel can interrupt evaluation from another thread — useful for human interrupts, timeouts, and supervision.

### Tail call optimization
Single-expression function bodies reuse the current frame instead of pushing a new one, enabling unbounded recursion without stack growth.

### Closures only capture frames, not namespaces
Only lexical frames are serialized into closures. Namespaces are shared by reference at call time through the kernel's env. This avoids O(n) serialization cost per closure.

### `agent/call` — flat_map-style subagents
`(agent/call 'child-name request)` pushes a child frame, evaluates the request, pops the frame, and delivers the result to the parent. The caller blocks; the child runs synchronously. Only one evaluation runs at a time.

### Snapshots are atomic
Every snapshot serializes both kernel and env together as a JSON object. Recovery loads the saved image directly — it never replays execution. Snapshots are versioned, checksummed (SHA256), and rotated. After recovery, every active frame receives a `(system/Restarted :kind :unclean :downtime ...)` notice.

### Human interaction
Messages are queued as interrupts to every active frame. Current work is suspended at the next safepoint, the interaction runs, and returns `control/Continue` or `(control/CancelCurrent ...)`.

### Supervision
The scheduler performs a 15-minute review of any top-level call that exceeds its budget. If a frame has been running with excessive queued messages, it can be cancelled.

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

Snapshots happen automatically. Recover from a crash with `cargo run` — it auto-detects the latest snapshot.

## Design Principles

1. **Kernel is minimal.** Only what the VM needs to function. Everything else is Lisp.
2. **`bash` is the universal tool interface.** File I/O, web requests, network calls — all go through shell commands. The kernel doesn't need built-in HTTP, file APIs, or JSON parsing.
3. **Definitions persist.** Every `define` creates a new version. `undefine` removes the current binding without erasing history. `inspect/history` shows the full version trail.
4. **Snapshots are atomic.** You can never save mid-call. Recovery restores the exact pre-call state.
5. **The kernel can always interrupt Lisp.** A safepoint mechanism checks every 1,000 expressions.

## Stats

- 3,145 lines of Rust
- 21 tests across 3 test suites
