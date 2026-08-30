# Continuum

A **continuously existing agent** whose computer is a persistent Lisp world.

```
Model/AI
  ↕
Lisp environment (continuum)
  ↕
bash — the universal tool interface
  ↕
Files, processes, network, everything else
```

The kernel is the Lisp VM. It provides 41 native functions — arithmetic, list operations, type predicates, I/O, persistence, inspection, and `bash`. Everything else — file I/O, web requests, git, model inference, package management — goes through `bash`. The agent defines its own tools in Lisp.

## Language

A Scheme-like Lisp with:

| Special forms | `define`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote`, `undefine` |
|---|---|
| Macros | `define-syntax` with `syntax-rules` (including `...` ellipsis for variable-length patterns) |
| Data types | `define-data` for tagged value families with automatic constructor functions |
| Pattern matching | `match` with constructor pattern destructuring |
| Tagged values | `(Ok value)`, `(Err problem)`, `(Cancelled reason)`, `(Indeterminate problem)` |



## Design

**The kernel can always interrupt Lisp.** A safepoint mechanism checks a global flag every 1,000 expressions. The kernel can interrupt evaluation from another thread — useful for human interrupts, timeouts, and supervision.

**Tail call optimization.** Single-expression function bodies reuse the current frame instead of pushing a new one, enabling unbounded recursion without stack growth.

**Closures only capture frames, not namespaces.** Only lexical frames are serialized into closures. Namespaces are shared by reference at call time, avoiding O(n) serialization cost per closure.

**Subagents.** `(agent/call 'name request)` pushes a child frame, evaluates the request, pops the frame, and delivers the result to the parent. Only one evaluation runs at a time.

**Snapshots are atomic.** Every evaluation serializes the full kernel state to JSON. Recovery loads the saved image directly — it never replays execution. Snapshots are checksummed (SHA256) and rotated. After recovery, every active frame receives a `(system/Restarted :kind :unclean :downtime ...)` notice.

**Human interaction.** Messages are queued as interrupts to every active frame. Current work is suspended at the next safepoint, the interaction runs, and returns `control/Continue` or `(control/CancelCurrent ...)`.

**Supervision.** The scheduler performs a 15-minute review of any top-level call that exceeds its budget. If a frame has been running with excessive queued messages, it can be cancelled.

## Quick Start

```bash
cargo run
```

Starts a continuous agent. The agent lives until `!!exit`. Type any Lisp expression:

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

## Principles

1. **Kernel is minimal.** Only what the VM needs to function. Everything else is Lisp.
2. **`bash` is the universal tool interface.** File I/O, web requests, network calls — all go through shell commands. No built-in HTTP, file APIs, or JSON parsing.
3. **Definitions persist.** Every `define` creates a new version. `undefine` removes the current binding without erasing history.
4. **Snapshots are atomic.** You can never save mid-call. Recovery restores the exact pre-call state.
5. **The kernel can always interrupt Lisp.** A safepoint mechanism checks every 1,000 expressions.

## Native Functions

| Category | Functions |
|---|---|
| Arithmetic | `+` `-` `*` `/` `<` `=` `>` |
| Lists | `cons` `car` `cdr` `list` |
| I/O | `display` `println` `read` |
| Type predicates | `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?` |
| Control | `control/Continue` `control/CancelCurrent` `control/Error` |
| System | `system/clock` `system/version` `system/interrupt` `system/clear-interrupt` |
| Persistence | `system/snapshot` `system/event-log` |
| Inspection | `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/source` `inspect/history` |
| Subagents | `agent/call` |
| Shell | `bash` |
| Scheduling | `wake` |
| Utilities | `map/get` `vector/get` `kernel/error` |
