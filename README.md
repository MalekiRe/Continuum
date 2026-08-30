# Continuum

Continuum is a persistent Lisp world driven by a model-owned action loop:

```text
persistent frame context
  → model emits exactly one Lisp form
  → kernel evaluates it transactionally
  → form and result enter the same frame transcript
  → next model turn
```

The model is not called by `agent/step`. Rust's scheduler owns model invocation, context assembly, tool traps, child-frame scheduling, transcript compaction, and snapshots.

## Quick start

```bash
export OPENROUTER_API_KEY=...
cargo run
```

Optional configuration:

```bash
export CONTINUUM_MODEL=deepseek/deepseek-v4-flash
export CONTINUUM_AGENT_ROOT=$PWD/data/workspace
```

The HTTP UI listens on `http://localhost:8080`.

## Turn model

Each active frame persists:

- immutable-ish agent instructions;
- its own recent Lisp transcript;
- chronological compacted context;
- pending human/task messages;
- selected key/value memory;
- context hooks;
- retained definition source;
- pending top-level suspension traps.

Model output must parse as exactly one Lisp form. Multiple operations can be grouped with `(begin ...)` only when none needs external suspension.

Human messages are injected into the active frame context and answered explicitly with:

```lisp
(message/reply "message-id" "answer")
```

## External operations

These forms must be top-level until the VM has a fully general serializable continuation stack:

```lisp
(bash "git status")
(model/call "focused model subtask")
(agent/call "researcher" "inspect the build")
(agent/return value)
(message/reply "message-id" "text")
```

The scheduler handles the resulting trap and puts the external result in the frame transcript. The next model turn sees that result.

`bash` uses actual Bash with a fixed working directory, one owned process group, bounded concurrently-drained output, live progress, a timeout, and external cancellation. Concurrent runs are rejected and background descendants are killed and reaped. Human input can interrupt an active Lisp evaluation, model request, or shell command.

The working directory is an execution root, not a filesystem sandbox: commands can still address absolute host paths. Use an OS/container sandbox when running untrusted agents.

## Lisp VM guarantees

- Top-level evaluation is transactional across namespaces, frames, wake timers, and lexical-heap allocation.
- Tail calls are optimized only in actual tail position; caller lexical frames are restored after the trampoline completes.
- Closures store stable lexical-heap IDs rather than serialized JSON environments.
- Maps have deterministic insertion/evaluation order and order-independent canonical hashing.
- `system`, `control`, `inspect`, and `kernel` namespaces are protected by exact identity.
- Definition source is retained in the namespace and exposed by `source/get`.

## Subagents

A top-level `agent/call` marks the parent Waiting and pushes a child frame with independent instructions, messages, transcript, compaction, and selected memory. The scheduler runs the child until top-level `agent/return`, then pops it, records the result in the parent transcript, and wakes the parent.

## Memory and context

```lisp
(memory/remember "project" "Continuum")
(memory/forget "project")
(memory/list)
(context/add-hook "Prefer tests before edits")
(context/clear-hooks)
(transcript/recent 10)
```

Transcript compaction is chronological and bounded. There is deliberately no end-to-end event log.

## Snapshots

Snapshots use one strict versioned envelope with an embedded SHA-256 checksum. They are written through a synced temporary file, atomic rename, and directory sync. Recovery tries snapshots newest-first, verifies checksums, falls back across corrupt/truncated files, restores transcripts/frame stacks/closure heap/pending traps, and re-registers native functions without polluting binding history. The writer retains the newest 48 snapshots; recovery remains compatible with v2 full/incremental envelopes.

If snapshot files exist and none is valid, startup exits with a continuity violation instead of silently creating a fresh kernel.

## Tests

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Behavioral coverage includes the model→Lisp feedback loop with a fake model, human replies, subagent scheduling, source rollback, transcript compaction, shell timeout/cancellation/process groups/output bounds, real snapshot recovery/fallback/checksums, closure recovery, map hash contracts, tail-call correctness, and transactional evaluation.
