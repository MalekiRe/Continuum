# Continuum

A continuously existing agent whose computer is a persistent Lisp world.

```text
Model
  ↕
Persistent Lisp environment
  ↕
Immutable Rust kernel
  ↕
Bash and the external world
```

Rust owns irreducible semantics, authority, persistence, and scheduling. An embedded immutable Lisp prelude supplies derived public functions. The agent extends the system by defining persistent namespaced Lisp code.

## Language

Continuum implements a small Scheme-like Lisp with:

| Area | Forms and values |
|---|---|
| Core forms | `define`, `undefine`, `lambda`, `if`, `begin`, `let`, `let*`, `letrec`, `set!`, `quote`, `quasiquote` |
| Macros | Pattern macros through `define-syntax` and the small `syntax-rules` matcher |
| Tagged data | `define-data` constructors and `match` destructuring |
| Values | nil, booleans, integers, floats, strings, symbols, keywords, lists, vectors, maps, functions, macros, and tagged values |

Evaluation is deterministic and left-to-right. Actual tail calls use a trampoline, so tail recursion does not grow the Rust stack.

## Runtime layers

### Rust kernel

The kernel contains the reader, transactional evaluator, lexical cell arena, static builtin dispatcher, typed trap boundary, scheduler, snapshots, Bash executor, function hooks, and message/agent-stack machinery. Evaluation records only changed cells, allocations, context entries, memories, hooks, timers, and history events; failed forms unwind that compact transaction instead of cloning the entire months-old agent.

Rust builtins are declared once in a static specification that generates their names, arities, dispatch, installation, and runtime fingerprint. There is no mutable native-function registry.

### Immutable Lisp prelude

`src/prelude.lisp` is embedded in the binary and evaluated once when a fresh kernel is created. It defines derived functions such as:

```lisp
(define (bash command)
  (kernel/trap :bash command))

(define (agent/call name request)
  (kernel/trap :agent-call [name request]))

(define (string? value)
  (= (kernel/type-of value) :string))
```

Prelude bindings are individually immutable, but their namespaces remain extensible. The agent cannot replace `agent/call`, but it may define `agent/helper`.

### Agent library

Ordinary definitions are persistent and mutable:

```lisp
(define (git/status repository)
  (bash (string-append "git -C " repository)))
```

The namespace tree is the reusable library the agent grows over time.

## Scheduler and external operations

Rust owns the continuous loop:

```text
assemble active-frame context
→ model emits one Lisp form
→ evaluate it
→ record its value or execute its trap
→ schedule the next turn
```

`kernel/trap` converts a keyword and Lisp payload immediately into a typed Rust operation. Evaluation returns either:

```text
Value(value)
Trap(request)
```

Traps are never stored in agent frames. The scheduler owns Bash, nested model calls, subagent calls, human waiting, and replies from the moment evaluation returns them. Cancellation therefore cannot leave a stale operation that is accidentally executed later.

An external operation must be the final action of an evaluation. Tail-position wrappers are valid:

```lisp
(begin
  (define build/attempt 3)
  (bash "make test"))
```

Using an external operation where a synchronous value is still required is rejected:

```lisp
(+ 1 (model/call "return a number"))
```

## Agents and interaction

`agent/call` pushes a serialized child frame. The parent is suspended by stack position; no separate frame-status field is needed. `agent/return` pops the child and returns one string to the parent. Only the top frame and one model invocation run at a time.

Human messages and both automatic watchers invoke the same serialized control model. The control model may inspect with Bash, reply, advise, continue, or cancel; a message never cancels work by itself. Lisp pauses at a safepoint, model generation restarts from the same prompt after a control review, and blocking Bash keeps running while the control model inspects it. A shared notice log tracks which frames have observed each message. Replies are explicit:

```lisp
(message/reply "message-id" "Understood; changing course.")
```

`human/wait` sets only the top frame's `waiting_for_human` bit. A new message clears it.

## Binding history

The live binding stores its executable value, source, and origin. The kernel also keeps an append-only raw event history and each frame keeps an O(log n) chronological spine over compacted turns. Bounded history stores source-oriented definition events rather than old executable closures:

```text
defined: source + short value preview
undefined
```

This preserves useful code history without keeping obsolete lexical environments and captured heaps alive. `set!` changes current state but does not create a new definition version. Whole-agent snapshots preserve the exact current value graph.

## Context construction

A budget-aware context builder assembles sections in priority order:

1. human messages and notices;
2. active frame stack;
3. hooks and selected memory;
4. recent exact transcript;
5. earlier compacted context;
6. library discovery.

One recent-first renderer is shared by transcript and compacted-history views, so constrained contexts retain the newest work.

## Snapshots

A snapshot is one checksummed JSON envelope containing the persistent kernel state. Writes use a synced temporary file, atomic rename, directory sync, and bounded rotation. Recovery tries current-format snapshots newest-first and falls back across corrupt files; execution is never replayed.

There is deliberately no snapshot backward compatibility. Every snapshot contains a runtime fingerprint derived from:

```text
snapshot format
kernel revision
static builtin specification
immutable prelude source
```

There is one frozen snapshot shape and no migration path. Checkpoints are ordered by a monotonic sequence and validated only for corruption.

External operations belong to the scheduler and are not serialized. A snapshot captures only the persistent Lisp machine at a quiescent boundary.

## Bash

`bash` blocks the logical agent frame by default and has no fixed deadline. The host control plane remains responsive, so the fifteen-minute and token/wait-ratio watchers can inspect and deliberately continue or cancel it. It runs in a fixed agent working directory with a scrubbed environment, bounded returned output, process-group ownership, progress inspection, and cancellation. Explicit background work uses `bash/start`, then `bash/status`, `bash/collect`, or `bash/cancel`; the agent can use `wake` to schedule its own next inspection. The working directory is not an OS-level filesystem sandbox.

File access, Git, web clients, compilers, packages, databases, clocks beyond the minimal kernel clock, and service CLIs are composed through Bash and persistent Lisp functions.

## Quick start

```bash
# Run a local model server implementing POST /generate, then:
export CONTINUUM_MODEL_URL=http://127.0.0.1:8081/generate
cargo run
```

Continuum acquires an exclusive lock on `CONTINUUM_STATE_ROOT` (default `data`) so one persistent identity cannot be forked by two processes. Snapshots live under its `snapshots` directory and the default workspace under `workspace`. The HTTP interface binds to `127.0.0.1:8080` by default. Set `CONTINUUM_HTTP_ADDR` to override it. Type a message in the terminal or web UI; the model responds with one Lisp form per turn. Use `!!exit` or `!!quit` for a clean snapshot and shutdown.

Examples:

```lisp
(+ 1 2)
(bash "git status --short")

(define-data result/Result
  (Ok value)
  (Err problem))

(match (result/Result/Ok 42)
  ((result/Result/Ok value) (+ value 1))
  ((result/Result/Err problem) -1))
```

## Public Lisp surface

| Category | Functions |
|---|---|
| Arithmetic | `+` `-` `*` `/` `<` `=` `>` |
| Lists and collections | `cons` `car` `cdr` `list` `append` `nth` `length` `map/get` `vector/get` |
| Strings | `string-append` `string-search` `substring` |
| Predicates | `nil?` `number?` `symbol?` `string?` `list?` `function?` `keyword?` |
| Output | `display` `println` |
| External operations | `bash` `bash/start` `bash/status` `bash/collect` `bash/cancel` `bash/list` `model/call` `agent/call` `agent/return` `human/wait` `message/reply` |
| Hooks | `hook/add` `hook/remove` `hook/list` `hook/run` |
| Memory and context | `memory/note` `memory/remember` `memory/view` `memory/recall` `memory/forget` `memory/list` `context/inject` `context/remove` `context/list` `transcript/recent` |
| Source and inspection | `history/read` `history/find` `history/spine` `source/get` `source/list` `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/history` |
| System | `system/clock` `system/version` `wake` |

`kernel/trap` and `kernel/type-of` are low-level immutable primitives used by the prelude; normal agent code should use the public wrappers.

## Intelligent supervision

Ordinary cognition, subagents, and control turns share one serialized model slot. A human message, each fifteen-minute wall-clock review, and the generated-token/tool-wait ratio review enter the same control loop. The control model may repeatedly run diagnostic Bash commands before choosing:

```lisp
(control/continue "optional human reply")
(control/advice "advice for the owning frame" "optional human reply")
(control/cancel "reason" "optional human reply")
```

Blocking Bash remains blocking from the agent's point of view. Explicit `bash/start` is the opt-in escape hatch when the agent wants to think while a process runs; `wake` can schedule its next inspection.

## Indefinite memory

Every completed action is recorded in an append-only raw event log. Exact recent turns remain in the active transcript; older turns enter an eight-way chronological summary spine whose number of nodes grows logarithmically. Raw events remain available through `history/read` and `history/find`.

Selective memory has no fixed entry-count limit. The root frame owns permanent memory; children read the root view and add frame-local memories. `memory/forget` removes the underlying record and rebuilds its derived index. Context construction shows exact recent memories plus increasingly coarse older nodes, while `memory/recall` searches the complete active records.

## Function hooks

Hooks attach to a named Lisp or builtin call in deterministic insertion order:

```lisp
(hook/add "audit-bash" 'bash :before 'audit/before)
(hook/add "record-bash" 'bash :after 'audit/after)
```

Before callbacks receive `(target arguments-vector)`; after callbacks receive `(target arguments-vector result)`. They cannot replace arguments, skip the call, or replace its result. A hook ID cannot recursively invoke itself, hook traps are forbidden, and any hook failure aborts and rolls back the whole top-level evaluation. Special forms, macros, `hook/*`, `context/*`, and the low-level `kernel/trap` boundary are not hookable. The scheduler invokes `stage/before-context`, `stage/after-generation`, and `stage/after-restart` through the same mechanism.

## Shutdown and checkpoint retries

`!!exit`, `!!quit`, SIGINT, and SIGTERM enter the same serialized shutdown path. Active blocking work and explicit background jobs are cancelled, then the newest quiescent state is snapshotted before exit. Hourly checkpoint failures do not postpone durability for another hour; they retry after one minute and resume hourly scheduling only after a successful commit.
