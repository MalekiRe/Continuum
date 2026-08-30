# Diagnosis

I reviewed the current public `master` of `MalekiRe/Continuum`. I do not have your uncommitted changes or runtime logs, but the implementation itself explains the likely behavior.

The fundamental problem is not prompting or model choice:

> **Continuum is currently a Lisp interpreter with a stateless chatbot called from inside it. It is not yet a model inhabiting and operating a persistent Lisp world.**

The control direction is reversed.

```text
Current:

Rust loop
  → evaluate (agent/step)
  → Lisp calls model/chat
  → model returns a string
  → print the string
  → repeat
```

What the design needs is:

```text
Rust scheduler
  → assemble the active frame's persistent context
  → model emits one Lisp form
  → evaluate that form
  → append the form and result to the frame
  → invoke the model again
```

That difference accounts for most of the disappointing behavior.

## 1. The model never sees or controls the Lisp world

The autonomous prompt asks the model to generate "a single brief thought" that is "one sentence." Human messages are passed directly to `model/chat`. `model/chat` receives one user message, no persistent transcript, no stack, no namespace contents, no recent Lisp results, and no memory. It is capped at 200 output tokens. The returned text is printed but never evaluated as Lisp.

You implemented `extract-lisp` and `eval-code`, and there are many tests for them, but the production cognition loop never calls either function. The tests verify a pipeline that is disconnected from the running agent.

That predicts exactly this behavior:

* endless generic "curious" thoughts;
* little or no accumulation of purpose;
* no spontaneous Bash or Lisp use;
* no definition of useful namespaced tools;
* no continuation of prior investigations;
* generic chat replies that forget immediately.

The model is doing what the prompt requests.

## 2. Persistence exists below the level where cognition happens

The Lisp namespace can persist definitions, but the model is never shown those definitions or their results. There is no persistent model transcript or context assembler.

`LOG_BUF` and `CHAT_HISTORY` are process-global in-memory collections. The thought log retains only 1,000 lines, neither is part of the kernel state, and neither participates in model context construction. On restart they disappear.

The current repository also has no implemented:

```text
context injection
hooks
OptMem
chronological compaction
raw history API
```

The `history/*` section in the native-function source is currently comments.

So the VM may be persistent, but the **mind is repeatedly instantiated without its past**.

## 3. The subagents are not model agents

The current `agent/call`:

1. Creates a child frame.
2. Treats the request string as Lisp source.
3. Evaluates that source directly.
4. Pops the frame and returns the value.

The agent name is effectively only a frame label. It does not resolve `agents/<name>`, load a child prompt, assemble an independent child context, or invoke a model. After pushing the child, the implementation also marks the current top frame as waiting—which is the child, not the parent.

I found no registered `agent/return` native in the current implementation; the documented native table lists only `agent/call`.

So this:

```lisp
(agent/call "researcher" "Investigate the build failure")
```

does not ask a researcher model to investigate anything. It attempts to evaluate the request as Lisp.

## 4. Human interruption and supervision cannot run while they are needed

The main loop is synchronous. Both `model/chat` and `bash` block the main thread.

Human messages are placed into a channel, but the channel is checked only between cognition turns. A message arriving during a 60-second model request or an indefinitely blocked shell command cannot suspend it. `human_message` queues a notice but does not set the VM interrupt flag.

The supervision code is effectively unreachable:

* `eval_started_at` is set during `kernel.eval`.
* The main thread is blocked inside that evaluation.
* `check_supervision` runs only before the next evaluation.
* By then, `eval_started_at` has been cleared.

The Lisp safepoint flag is checked only while evaluating Lisp expressions. It cannot interrupt `reqwest::blocking` or `Command::output`.

The token-efficiency detector also depends on explicit calls to `system/report-tokens`, but `model/chat` does not report its own usage.

Consequently:

* the 15-minute supervisor does not execute during a 15-minute call;
* the efficiency supervisor receives no reliable token stream;
* humans cannot truly interrupt ongoing work;
* a stuck shell process can hold the entire agent indefinitely.

## 5. Snapshot recovery is currently broken

Snapshot writing serializes this shape:

```json
{
  "kernel": ...,
  "env": ...
}
```

Recovery tries to deserialize the same bytes as the `Snapshot` struct, which expects fields such as `id`, `timestamp`, `kernel`, `checksum`, and `kind`. Those formats do not match.

`load_or_create_kernel` catches recovery failure and starts a fresh kernel, so a failed recovery can look like a normal clean start instead of a fatal continuity violation.

There are additional durability problems:

* snapshots are written directly, not through temporary file + flush + atomic rename;
* write errors are ignored;
* the checksum is recorded but not verified;
* recovery always prefers the newest full snapshot, even when a newer incremental snapshot exists;
* the continuation object is descriptive metadata, not an executable VM continuation;
* no model context or token prefix is stored;
* the agent core is re-evaluated after recovery, redefining its bindings.

The snapshot test does not call `Kernel::snapshot` or `recover_from_latest`. It manually extracts the `kernel` property from JSON, which bypasses the incompatible production recovery path.

## 6. The current tail-call implementation changes program meaning

This is the most serious VM correctness issue.

Every interpreted function with a single-expression body is treated as a tail call, regardless of whether the function call itself occurs in tail position.

For example:

```lisp
(define (double x) (* x 2))
(+ 1 (double 2))
```

should return `5`.

In the current evaluator, `(double 2)` raises the internal `TailCall` signal, which escapes through argument evaluation and causes the outer `(+ 1 ...)` expression to be discarded. The result will follow the body of `double`, producing `4`.

The implementation also replaces the active lexical frames with the closure's captured frames and does not restore the original frames on the `TailCall` path.

This can cause:

* nested calls to skip their callers;
* lexical environments to become corrupted;
* expressions to behave differently based solely on whether a function has one or multiple body expressions.

Remove this optimization entirely until the evaluator has an explicit continuation stack and can prove that a call is in tail position.

## 7. Closures are expensive and brittle

Creating a function serializes all active lexical frames to JSON:

```text
lambda creation
  → serialize lexical frames to a JSON string
```

Calling it deserializes that JSON, swaps the kernel's current lexical-frame vector, evaluates the body, and swaps it back.

This is likely a meaningful source of poor Lisp performance. It also makes closure behavior dependent on serialization details.

A Lisp VM should represent a closure as:

```text
function code reference
+
lexical environment reference
```

not as a JSON document.

Use stable heap object IDs or arena indices. Serialize the object graph only when taking a whole-agent snapshot.

## 8. Evaluation is not atomic

`Kernel::eval` mutates the live environment directly.

This:

```lisp
(begin
  (define broken/example 42)
  (undefined-function))
```

returns an error, but the definition may remain installed. There is no working environment, undo log, or commit step.

That contradicts the intended rule that invalid Lisp aborts the active top-level evaluation.

The simplest correct implementation is:

```text
clone/COW namespace root
→ evaluate against working root
→ on success, install root
→ on failure, discard root
```

The heap need not be copied eagerly; a persistent map or copy-on-write namespace root is enough.

## 9. Maps are nondeterministic and violate Rust's hash contract

Lisp maps use `HashMap<Value, Value>`.

The evaluator walks map entries in hash iteration order, so map expressions are not evaluated deterministically left-to-right. Display order also varies.

More seriously, `Value::Map` equality uses `HashMap` equality, which ignores insertion/iteration order, while its `Hash` implementation iterates entries in arbitrary order. Two equal maps can therefore produce different hashes. That violates the `Eq`/`Hash` contract and can make maps used as keys behave incorrectly.

Use an ordered persistent map or define a canonical sorted-key hashing and evaluation order.

## 10. Kernel namespace protection has holes

Namespace protection checks whether a namespace name starts with `"system/"` or `"control/"`.

The actual namespace names are `"system"` and `"control"`, without the slash. Therefore those namespaces are not marked protected. Agent code can potentially replace or undefine control and system functions.

Protection should be based on exact namespace identity, not string-prefix conventions.

## 11. `bash` is neither Bash nor a sandbox

The function named `bash` launches:

```text
sh -c <command>
```

on the host. It has no agent-rooted working directory, no timeout, no process group, no output limit, no progress stream, and no cancellation channel. List arguments are joined with spaces, losing shell-safe argument boundaries. All output is buffered into memory before returning.

That causes both correctness and performance problems:

* a command can hang the whole harness;
* large output can consume arbitrary memory;
* a supervisor cannot inspect progress;
* user/model values can become shell injection;
* behavior differs from Bash;
* the process can access the host rather than an agent sandbox.

## 12. Introspection is not yet sufficient for self-extension

`inspect/source` formats the current value. For a Lisp function, that yields something like:

```text
#<fn (arg1 arg2)>
```

rather than its source. `inspect/find` only performs substring search over qualified binding names.

Even after the model begins using Lisp correctly, it will struggle to discover, understand, repair, and reuse its own growing library unless function source, documentation, and versions are preserved explicitly.

# Why the tests did not catch this

The test suite is mostly interpreter happy-path tests.

The "AI pipeline" tests exercise `extract-lisp` and `eval-code` directly, but not the production cognition loop. Several tests accept a returned error string as an acceptable outcome. The snapshot test bypasses the actual snapshot/recovery API. There are no end-to-end tests for persistent model context, model-generated Lisp execution, a real subagent, interruption during Bash, supervision, compaction, or kill-and-recover continuity.

# What to change first

Do not add OptMem, hooks, compaction, or more native functions yet. They would be built on the wrong execution loop.

## Phase 1: Build one correct vertical slice

Remove `model/chat` from ordinary Lisp code and invert control:

```text
1. Select top agent frame.
2. Assemble that frame's context.
3. Ask the model for exactly one Lisp form.
4. Parse and evaluate the form.
5. Append the source form and result to the frame transcript.
6. Invoke the same frame again.
```

The assembled context initially needs only:

```text
immutable agent instructions
active frame name and purpose
active agent stack
pending human messages
recent Lisp forms and their results
compact list of visible namespaces
```

The model should produce a structurally constrained Lisp form, not prose containing optional `<lisp>` tags.

A human response is also Lisp:

```lisp
(message/reply "message-id" "Here is what I found.")
```

The important inversion is:

```text
model → Lisp
```

not:

```text
Lisp → stateless model/chat
```

## Phase 2: Make the evaluator correct before optimizing it

Immediately:

* delete the current tail-call optimization;
* store closure environments by heap reference, not JSON;
* make top-level evaluation transactional;
* use deterministic ordered maps;
* repair namespace protection;
* retain source ASTs with definitions.

A slower correct evaluator is more useful than a fast evaluator that silently changes control flow.

## Phase 3: Introduce explicit VM suspension

The desired `agent/call`, human interrupt, and supervisor semantics require an explicit serializable continuation.

A recursive Rust evaluator cannot cleanly suspend in the middle of:

```lisp
(analyze
  (agent/call "researcher" request))
```

Implement the evaluator as an explicit machine:

```text
instruction/expression
value stack
continuation stack
lexical environment references
```

Then a native function can return a trap:

```text
VmTrap::CallAgent
VmTrap::RunBash
VmTrap::Reply
VmTrap::Cancel
```

The kernel saves the continuation, performs the operation, and resumes with the returned value.

Until that exists, restrict `agent/call` to a top-level operation rather than pretending nested suspension works.

## Phase 4: Replace `bash` with an executor

Run commands outside the scheduler thread.

The executor must provide:

```text
actual Bash
fixed agent-owned working root
argument-safe invocation
process-group ownership
timeout and cancellation
bounded/streamed output
status and progress inspection
```

This unlocks real human interruption and supervision.

## Phase 5: Fix whole-agent snapshots

Use one serialization envelope for writing and reading:

```text
format version
snapshot ID and timestamp
kernel state
VM heap
frames and transcripts
model contexts/token prefixes
checksum
```

Write:

```text
temporary file
→ flush file
→ atomic rename
→ flush directory
```

Recovery must verify the checksum and fall back to the previous valid snapshot. Add tests that call the real public snapshot and recovery methods, including truncating the newest file to simulate power loss.

## Phase 6: Add memory only after context exists

Once model turns actually have persistent frame transcripts:

1. Add context injection.
2. Add chronological compaction.
3. Add OptMem.
4. Add hooks last.

Without a functioning persistent model context, none of these mechanisms can improve cognition.

# The minimum end-to-end tests

The next tests should be behavioral:

1. A model is asked to inspect the working directory, emits a Bash Lisp form, receives the result, and uses it in the next turn.
2. The model defines `git/status`, and successfully discovers and calls it ten turns later.
3. `(+ 1 (double 2))` returns `5`.
4. A child model receives `agents/researcher`, returns a string, and the parent resumes at the call site.
5. A human message arriving during a long Bash process receives a control turn before that process completes.
6. The 15-minute supervisor can inspect and cancel a running process.
7. A real snapshot restores definitions, frame transcript, child stack, and model context.
8. A truncated newest snapshot falls back to the preceding snapshot.
9. A failed top-level evaluation leaves no definitions or mutations behind.

# Bottom line

The parser, tagged values, basic macros, namespaces, and much of the reader are useful prototype work.

The current agent behavior will not improve through prompt tweaking because the running system lacks the core feedback loop:

```text
persistent context
→ model chooses Lisp action
→ Lisp result
→ same persistent context
```

Until that loop exists, Continuum is repeatedly asking a stateless model for a sentence while a separate Lisp machine sits beside it unused.