# Continuum

A persistent, continuous autonomous agent framework powered by a Lisp VM.

Zero unsafe Rust. All native functions receive `&mut Kernel` and `&mut EnvRef` directly — no globals, no thread-local storage, no raw pointer dereferencing.

## Architecture

Two independent objects threaded through evaluation:

- **`Kernel`** — owns frames, storage, wake timers, event counter
- **`EnvRef`** — owns namespaces, lexical frames, binding history

```
Kernel::new() -> (Kernel, EnvRef)

eval(&mut self, source: &str, env: &mut EnvRef) -> Result<Value, EvalError>
```

Snapshots serialize both `Kernel` and `EnvRef` together.

## Design

- **Tail call optimization**: single-expression function bodies reuse the current frame instead of pushing a new one, enabling unbounded recursion.
- **Safepoint interruption**: a global atomic flag checked every N turns; the kernel can interrupt evaluation from another thread.
- **Closure capture**: only lexical frames are serialized into closures (not the entire namespace map), avoiding O(n) serialization cost.
- **Native functions**: `fn(&mut Kernel, &mut EnvRef, Vec<Value>) -> Result<Value, String>` — no hook system, no thread-local, no `unsafe`.
- **`flat_map`-style subagents**: `agent/call` pushes a child frame, evaluates the request, pops the frame, delivers the result to the parent.
- **Recovery from snapshot**: deserializes directly into the `Snapshot` struct, re-registers native function pointers, notifies all frames of restart.

## Running

```bash
# Run the continuous agent (reads stdin for human messages)
cargo run

# Run tests
cargo test -- --test-threads=1 -q
```

## Stats

- ~3,200 lines of Rust
- 0 `unsafe` blocks
- 0 compiler warnings
- 21 tests across 3 test suites
