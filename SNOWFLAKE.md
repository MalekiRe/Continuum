# Snowflake bytecode harness

This directory is the compile-checked scaffold for the replacement harness. It
is intentionally disconnected from the legacy runtime until the center works.
No legacy image or API compatibility is planned.

## Invariants

- `World::State` is the only serialized state.
- `Task`, transactions, futures, continuations, and processes are memory-only.
- The VM calls hosts through `Value::Host`; bytecode has no effect opcode.
- Snapshot pauses never cancel external work.
- Lisp and external cancellation are separate controls.
- An accepted external effect commits the preceding Lisp segment.
- The compiler emits directly from reader `Value`s; there is no AST or IR.

## Snowflake order

Each ring must compile, stay inside its LOC budget, and have focused tests before
the next ring is implemented.

1. **Crystal:** IDs, `Value`, `Op`, chunks, symbols.
2. **First ring:** iterative reader and literal/global bytecode emission.
3. **Second ring:** VM constants, globals, calls, returns, and branches.
4. **Third ring:** lexical slots, closure captures, cells, and tail calls.
5. **Fourth ring:** transactions and transparent pause/snapshot.
6. **Fifth ring:** synchronous hosts, then nested model and Bash effects.
7. **Sixth ring:** durable agents plus memory-only parked parent tasks.
8. **Shell:** model loop, HTTP interaction, metrics, cancellation, shutdown.

## Explicitly absent

Fuel, serialized continuations, operation IDs, compatibility readers, native
machine-code JIT, optimizer passes, general managed heap, environment arena,
binding history, context hooks, wake timers, macros, quasiquote, tagged data,
and pattern matching are not part of the initial implementation.

## Physical LOC gates

- `src/snowflake/compile.rs`: at most 800 nonblank, non-comment lines.
- All other files under `src/snowflake`: at most 2,000 such lines combined.

The gate is a warning against architectural growth, not permission to compress
readable statements onto single lines. A feature that exceeds the budget needs
an explicit design discussion rather than code golf.
