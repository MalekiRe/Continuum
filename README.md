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

The kernel contains the reader, evaluator, lexical cell arena, static builtin dispatcher, typed trap boundary, scheduler, snapshots, Bash executor, and message/agent-stack machinery.

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

Human messages interrupt model, Lisp, or shell work. A shared notice log tracks which frames have observed each message. Replies are explicit:

```lisp
(message/reply "message-id" "Understood; changing course.")
```

`human/wait` sets only the top frame's `waiting_for_human` bit. A new message clears it.

## Binding history

The live binding stores its executable value, source, and origin. Bounded history stores source-oriented definition events rather than old executable closures:

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

A different runtime rejects the snapshot rather than attempting migration. Keep the matching binary with any snapshot whose continuity matters.

External operations belong to the scheduler and are not serialized. A snapshot captures only the persistent Lisp machine at a quiescent boundary.

## Bash

`bash` runs in a fixed agent working directory with a scrubbed environment, bounded output, timeout, process-group ownership, progress inspection, and cancellation. The working directory is not an OS-level filesystem sandbox.

File access, Git, web clients, compilers, packages, databases, clocks beyond the minimal kernel clock, and service CLIs are composed through Bash and persistent Lisp functions.

## Quick start

```bash
export OPENROUTER_API_KEY=...
cargo run
```

The HTTP interface binds to `127.0.0.1:8080` by default. Set `CONTINUUM_HTTP_ADDR` to override it. Type a message in the terminal or web UI; the model responds with one Lisp form per turn. Use `!!exit` or `!!quit` for a clean snapshot and shutdown.

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
| External operations | `bash` `model/call` `agent/call` `agent/return` `human/wait` `message/reply` |
| Memory and context | `memory/remember` `memory/forget` `memory/list` `context/add-hook` `context/clear-hooks` `transcript/recent` |
| Source and inspection | `source/get` `source/list` `inspect/namespaces` `inspect/bindings` `inspect/find` `inspect/history` |
| System | `system/clock` `system/version` `wake` |

`kernel/trap` and `kernel/type-of` are low-level immutable primitives used by the prelude; normal agent code should use the public wrappers.
