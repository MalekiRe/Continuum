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

The kernel is the Lisp VM. It provides 50 native functions — arithmetic, list operations, type predicates, output, persistence, inspection, model operations, and `bash`. File I/O, web requests, git, package management, and other computer interaction go through `bash`; Rust's scheduler owns model inference and external suspension. The agent defines its own tools in Lisp.

## Language

A Scheme-like Lisp with:

| Special forms | `define`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote`, `undefine` |
|---|---|
| Macros | Pattern-based, non-hygienic `define-syntax` with a small `syntax-rules`-style matcher (including trailing `...` ellipsis) |
| Data types | `define-data` for tagged value families with automatic constructor functions |
| Pattern matching | `match` with constructor pattern destructuring |
| Tagged values | `(Ok value)`, `(Err problem)`, `(Cancelled reason)`, `(Indeterminate problem)` |

## Design

**Safepoint interrupts.** The kernel can interrupt Lisp evaluation from another thread. A global atomic flag is checked every 1,000 expressions — human input sets it while Lisp is running, and evaluation terminates at the next safepoint.

**Scheduling.** Rust owns the continuous model → Lisp → evaluation loop. Each model response must be exactly one raw Lisp form. Forms and results enter the active frame's bounded transcript, which is included in the next model context. External suspension operations such as `bash`, `model/call`, and `agent/call` must be top-level.

**Tail call optimization.** Calls in actual tail position reuse the evaluator trampoline instead of growing the Rust stack, enabling unbounded tail recursion while preserving non-tail callers.

**Closures only capture frames, not namespaces.** Closures retain stable IDs into a serializable lexical heap. Namespaces are shared by reference at call time, avoiding O(n) serialization cost per closure.

**Subagents.** Top-level `(agent/call "name" "request")` pushes a child frame with its own model context, transcript, memory, and messages. A top-level `(agent/return value)` pops the child and delivers the result to its parent. Only one evaluation runs at a time.

**Human interaction.** Messages are persisted in frame context and interrupt active model, Lisp, or shell work. A message remains pending until the agent explicitly answers it with `(message/reply "message-id" "answer")`.

**Snapshots.** Every snapshot serializes the full kernel state to a checksummed JSON envelope. Recovery tries snapshots newest-first, falls back across corrupt files, and never replays execution. Writes use a synced temporary file and atomic rename, and the newest 48 snapshots are retained. After recovery, every active frame receives a restart notice. Existing v2 snapshots remain readable.

**Wake timers.** `(wake ms action)` schedules `action` to be delivered to the originating frame as context after `ms` milliseconds. Timers are checked once per cognition loop iteration.

## Quick Start

```bash
export OPENROUTER_API_KEY=...
cargo run
```

Starts a continuous agent and an HTTP UI at `http://localhost:8080`. The agent lives until `!!exit`. Type a message in the terminal or web UI; the model responds by emitting exactly one Lisp form at a time. The Lisp language includes:

```lisp
(+ 1 2) ; => 3
(bash "echo hello")
(define-data result/Result (Ok value) (Err problem))
(match (user/result/Result/Ok 42)
  ((result/Result/Ok n) (+ n 1))
  ((result/Result/Err msg) -1)) ; => 43
```

Snapshots happen automatically. Recover from a crash with `cargo run` — it auto-detects the latest valid snapshot. If snapshot files exist but none is valid, startup stops with a continuity error instead of silently starting over.

## Principles

1. **Kernel is minimal.** Only what the VM needs to function. Everything else is Lisp.
2. **`bash` is the universal computer interface.** File I/O, web requests, network calls — all go through shell commands. There are no built-in Lisp HTTP, file, or JSON APIs; the Rust scheduler separately handles model and suspension operations. The configured working directory is not an OS-level filesystem sandbox.
3. **Definitions persist.** Every `define` creates a new version. `undefine` removes the current binding without erasing the bounded history.
4. **Snapshots are atomic.** The kernel is never saved mid-evaluation. Recovery restores a consistent top-level state, including frame transcripts, messages, closures, and pending external operations.

## Native Functions

| Category | Functions |
|---|---|
| Arithmetic | `+` `-` `*` `/` `<` `=` `>` |
| Lists | `cons` `car` `cdr` `list` `append` `nth` `length` |
| Output | `display` `println` |
| Type predicates | `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?` |
| System | `system/clock` `system/version` |
| Model and messages | `model/call` `human/wait` `message/reply` |
| Persistence and context | `memory/remember` `memory/forget` `memory/list` `context/add-hook` `context/clear-hooks` `transcript/recent` |
| Source and inspection | `source/get` `source/list` `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/history` |
| Subagents | `agent/call` `agent/return` |
| Shell | `bash` |
| Scheduling | `wake` |
| Strings | `string-append` `string-search` `substring` |
| Utilities | `map/get` `vector/get` `kernel/error` |
