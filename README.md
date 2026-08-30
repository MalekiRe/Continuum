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

**Safepoint interrupts.** The kernel can interrupt Lisp evaluation from another thread. A global atomic flag is checked every 1,000 expressions — when set, evaluation terminates at the next safepoint. Lisp code can set and clear this flag with `system/interrupt` and `system/clear-interrupt`.

**Supervision.** Every cognition loop iteration checks two conditions and delivers advisory notices — the agent stays in control:
- **Long-running eval** (advisory at 15min, hard kill at 1hr): if a top-level eval runs for 15+ minutes, the agent gets a `system/SupervisorNotice` suggesting it check whether it's making progress. At 1 hour, the kernel force-interrupts as a circuit breaker.
- **Low token efficiency** (advisory, starts after 120s): token reports from Lisp (`(system/report-tokens N)`) are tracked in a 30-minute sliding window. If elapsed time exceeds 6x the expected time at 10 tok/s, the agent receives a notice suggesting it optimize its approach — batch bash calls, reduce redundant testing, etc. If zero tokens are reported for 5+ minutes, a notice suggests the agent may be stuck in a blocking tool call.

**Tail call optimization.** Single-expression function bodies reuse the current frame instead of pushing a new one, enabling unbounded recursion without stack growth.

**Closures only capture frames, not namespaces.** Only lexical frames are serialized into closures. Namespaces are shared by reference at call time, avoiding O(n) serialization cost per closure.

**Subagents.** `(agent/call 'name request)` pushes a child frame, evaluates the request, pops the frame, and delivers the result to the parent. Only one evaluation runs at a time.

**Human interaction.** Messages are queued as interrupts to every active frame. Current work is suspended at the next safepoint, the interaction runs, and the agent returns `control/Continue` or `(control/CancelCurrent ...)`.

**Snapshots.** Every snapshot serializes the full kernel state to JSON. Recovery loads the saved image directly — it never replays execution. Snapshots are checksummed (SHA256) and rotated. After recovery, every active frame receives a `(system/Restarted :kind :unclean :downtime ...)` notice.

**Wake timers.** `(wake ms action)` schedules a Lisp expression to be evaluated as a message after `ms` milliseconds. Timers are checked once per cognition loop iteration.

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

## Native Functions

| Category | Functions |
|---|---|
| Arithmetic | `+` `-` `*` `/` `<` `=` `>` |
| Lists | `cons` `car` `cdr` `list` |
| I/O | `display` `println` `read` |
| Type predicates | `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?` |
| Control | `control/Continue` `control/CancelCurrent` `control/Error` |
| System | `system/clock` `system/version` `system/interrupt` `system/clear-interrupt` |
| Persistence | `system/snapshot` `system/report-tokens` |
| Inspection | `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/source` `inspect/history` |
| Subagents | `agent/call` |
| Shell | `bash` |
| Scheduling | `wake` |
| Utilities | `map/get` `vector/get` `kernel/error` |
