# Agent task suite: baseline and method

The scoring method and the task list are public here, and the results are recorded, so a
revision can be compared against a revision. This document is what the task-set acceptance
criterion asks for.

**Read this before quoting any number below.** The suite runs deterministic tool calls. It
does not run an agent. The first-call-success rate the requirement mentions is *not measured
here* — see [What this does not measure](#what-this-does-not-measure).

## The suite

`crates/tools/tests/ux_taskset_spec.rs` — 25 tasks, each worth 1 point, scored pass/fail on
an expectation that is a fact about the fixture rather than a snapshot of current output.
Run it:

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
cargo test --locked -p opencrayast-tools --test ux_taskset_spec -- --nocapture
```

The run prints the per-task table and the score. The score is `passed / total`.

**Why it lives in `cargo test` and not in a shell script.** It calls the tool handlers
in-process, so it is deterministic, needs no binary, no fixture checkout and no network, and
it cannot be affected by which OS ran it. That makes it comparable across revisions in a way
an end-to-end harness over pipes would not be.

## Fixture and ground truth

The suite builds a small workspace whose correct answers are known by construction:

| File | Contents |
|---|---|
| `src/lib.rs` | `struct Config` with a doc comment and two fields, an `impl` with `new` and `load` methods, a `pub(crate) hidden_helper`, `const DEFAULT_RETRIES`, `trait Greeter`, `struct EnglishGreeter`, `fn double`, `fn build` |
| `src/app.py` | `class Service` with `start`/`stop`, `fn build`, `const CONST_LIMIT` |

`build` exists in **both** files on purpose, so the ambiguous-symbol case is real by
construction rather than simulated.

## Tasks, by tool

25 tasks across the five read tools. Grouped so a regression names a tool:

| Tool | Tasks | What they pin |
|---|---|---|
| `ast_info` | 3 | The session-opening call reports the mode, workspace id and write availability it was actually given |
| `ast_outline` | 9 | Top-level symbols, traits and methods are all found; `depth` bounds nesting (a method inside an `impl` is hidden at depth 1, visible at depth 2); the `kinds` filter selects one kind; an unknown kind is `invalid_args`; two identical calls return identical bytes |
| `ast_get` | 6 | A body is returned; a qualified name (`Config::load`) resolves; a bare name resolves in a second language with no `path`; a field name is not a symbol; a missing symbol is `not_found` pointing at `ast_outline`; an ambiguous name lists its candidates |
| `ast_search` | 5 | A call shape is found; a mixed-language directory is refused with a next step; the same call succeeds once `language` is named; an unparseable pattern is `invalid_pattern`; an empty pattern is refused |
| `ast_explain_pattern` | 2 | A valid pattern is explained; an unparseable one is refused |
| (cross-cutting) | 2 | A traversal escape from the workspace is refused by `ast_outline` *and* by `ast_get` |

Seven of the tasks assert on the **error path**, and every one of them also asserts that the
error carries a **non-empty next step**. A refusal that cannot say what to do next is a dead
end, and this is where the "error messages carry a next step" part of the requirement becomes
something checkable rather than a claim.

## Baseline result

Recorded on the tree as of this commit, Rust `1.95.0`, `cargo test --locked -p
opencrayast-tools --test ux_taskset_spec`:

```
UX task suite (tools only, no LLM): 25/25 tasks passed
```

**Baseline to compare against: 25/25.** A revision that scores below 25 has regressed a tool;
the failing task names which one. `ux_tasks_score_is_stable` exists so that adding a task
without updating this number is visible rather than silent.

### The self-proof that the score is not always 25

A suite that has only ever printed 25/25 is indistinguishable from one that cannot fail.
Changing one task's ground truth from `n * 2` to `n * 3` — a single string, with the
implementation untouched — gives:

```
UX task suite (tools only, no LLM): 24/25 tasks passed
  FAIL ast_get: returns the body of a known function
```

The scorer reports the failing task by name and prints the actual output it got. Restored
afterwards; the committed tree is byte-identical to the run that produced 25/25.

## What this does not measure

This is the part that matters, because the requirement asks for an empirical evaluation with
a real agent and this suite is not that.

**Measured here:**

- Whether a tool returns the **correct** answer for a correct call.
- Whether its output format is **stable** (the byte-identical repeat-call task).
- Whether it **refuses** what it must refuse, with the right error code.
- Whether its errors carry an **actionable next step**.
- Whether **arguments behave as documented** (depth, kinds, path, language) — the class of
  mistake that costs an agent a round trip.

**Not measured here:**

- **Agent success rate and first-call success rate.** These require running an agent on the
  tasks and watching what it does first. No model is invoked anywhere in this suite. Any
  figure for these metrics would be fabricated, so none is given.
- **Prompt or description quality.** The suite calls tools with correct arguments. It says
  nothing about whether a description *leads* an agent to those arguments — which is what
  the tool-description work is actually about.
- **Recovery behaviour.** A real agent's most interesting failures are the second and third
  attempt. This suite has no notion of an attempt.
- **Performance, cost, or token savings.** That is the separate benchmark in
  [`BENCHMARKS.md`](BENCHMARKS.md).

### A finding worth recording: the mixed-language round trip

Three of the five `ast_search` tasks exist because of one behaviour found while writing this
suite. Pointing `ast_search` at a directory containing both Rust and Python, without
`language`, is refused with `mixed languages: python, rust` and the next step `Pass
\`language\`.`. That is documented and correct, and it costs an agent a round trip on a
workspace with more than one language in it — which is most workspaces. Scoring the refusal
*and* the recovery (`the same call succeeds once \`language\` is named`) records the cost
without pretending it is free. Whether the default should change is a design question for the
tool-description work, not something this suite decides.

## Running real agents: what would be needed

For completeness, and to be explicit that it is **not done**: an agent run would need a fixed
model, a pinned version, a fixed system prompt, and a definition of "first call" that is
decided in advance rather than after seeing results. It would produce a second, separate
number that must not be conflated with the 25/25 above, because it has a different denominator
and a different failure model. That run is future work, and its absence is why this document
does not claim a success rate.