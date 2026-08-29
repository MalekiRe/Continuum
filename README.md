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

**Kernel** (Rust, ~4,300 lines) owns the Lisp VM, persistent namespaces with versioned history, model & subagent scheduling, snapshots & crash recovery, native functions, the event log & artifact store, context compaction, human interrupts, and execution supervision.

**Lisp** owns planning, workflows, memory policy, agent roles, and tool wrappers — everything is defined as Lisp functions and macros into descriptive namespaces. Definitions persist automatically with history; `undefine` removes the current binding without erasing history.

## Language

A small Scheme-like Lisp with familiar special forms (`define`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote`) and:
- `define-syntax` with `syntax-rules` (including ellipsis)
- `define-data` for tagged value families with automatic constructor functions
- `match` with constructor pattern destructuring
- Tagged values: `(Ok value)`, `(Err problem)`, `(Cancelled reason)`, `(Indeterminate problem)`
- Opaque kernel references (`#<process 12345>`, `#<file /tmp/x>`)

Values: nil, booleans, numbers, strings, symbols, keywords, lists, vectors, maps, functions, macros, tagged values, opaque kernel references.

## Snapshots & Recovery

Every top-level Lisp call commits a snapshot before evaluation. Recovery loads the saved image directly — it never replays Lisp, tool calls, model output, or history events. Snapshots are versioned, checksummed (SHA256), atomically committed, and rotated (hourly full snapshot). After recovery, every active frame receives a `(system/Restarted :kind :unclean :downtime ...)` notice before its next cognition turn.

## Subagents

`(agent/call 'name request)` spawns a child frame. The caller pauses, the child runs, and the child's return value is delivered to the caller. Only one model invocation runs at a time. Nested subagents form a normal stack.

## Human Interaction

Messages are queued as interrupts to **every active frame**. The current work is suspended at the next safepoint, the interaction turn runs, and returns `control/Continue` or `(control/CancelCurrent "reason")`. The message and decision become a stack notice visible to every frame before it next thinks.

## Supervision

- **Efficiency review**: compares generated tokens with time spent waiting in tool calls; suggests batching when patterns look wasteful.
- **15-minute review**: if a top-level call runs for 15 minutes without returning to cognition, it's reviewed. Returns `supervisor/NoAction`, `(supervisor/Advice "...")`, or `(supervisor/Cancel "...")`.

## Quick Start

```bash
cargo run
```

Starts a continuous agent REPL. The agent lives until `!!exit`. Type any Lisp expression:

```lisp
lisp> (+ 1 2)
3
lisp> (define-data result/Result (Ok value) (Err problem))
result/Result
lisp> (match (user/result/Result/Ok 42)
         ((result/Result/Ok n) (+ n 1))
         ((result/Result/Err msg) -1))
43
lisp> (model/invoke "Say hello")
(result/Ok "Hello!" "deepseek/deepseek-v4-flash" 5 0.000003)
```

Snapshots happen automatically on every evaluation. Recover from a crash:

```bash
cargo run  # auto-detects latest snapshot
```

## Built-in Tools

`web/search`, `fs/read`, `fs/write`, `fs/open`, `proc/run`, `proc/pid`, `message/reply`, `clock/wake`, `string/join`, `string/split`, `string/contains?`, `map/get`, `vector/get`.

Model inference via OpenAI-compatible API (OpenRouter or Prime Inference). Auto-detects API key from `~/.prime/config.json` or `~/.prime/agent/auth.json`.

## Inspect & Discover

```lisp
(inspect/find "query")     → ("user/my-func" "kernel/+")
(inspect/describe 'name)   → #<fn (x)>
(inspect/source 'name)     → ...source code...
(inspect/namespaces)       → ((kernel 34) (user 12) ...)
(inspect/history 'name)    → ((timestamp version value) ...)
(history/read 42)          → event details
(history/find "migration") → event IDs matching query
```

## Invariants

- One continuous agent identity; one model invocation at a time
- Completed cognition schedules more cognition unless the frame returns or waits
- The kernel can always interrupt Lisp (safepoint mechanism, checked every 1000 expressions)
- Interrupted calls are never automatically repeated (cancellation tokens tracked in frame state)
- Committed state survives application, process, and machine restarts
- Raw history is never replaced by summaries
- Code creates behavior, never authority
