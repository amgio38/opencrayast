# opencrayast

**Structural code reading and safe structural editing for AI coding agents and the people who supervise them.**

> **Status: pre-1.0, not yet released.** The project is real and working — see
> [What works today](#what-works-today) — but it has no public release, no stable
> tool-catalogue freeze, and a version scheme of `0.YYYYMMDD.N` (see
> [ROADMAP.md](ROADMAP.md)). The design documents under `docs/` are normative and
> have passed one independent adversarial review (see ADR-017). The architecture,
> the security model, the test strategy and the release process are designed for a
> 1.0 that can be trusted with write access to a codebase, and they are not yet
> finished: the remaining gaps are listed below rather than hidden.
>
> The project is published as **`opencrayast`** at
> <https://github.com/amgio38/opencrayast> (the first public history starts
> when the milestones below reach a public preview).

## What it is

`opencrayast` is an [MCP](https://modelcontextprotocol.io) server (plus a
command-line tool built on the same core) that lets an agent work with source
code as a **syntax tree** instead of as text:

- **Read cheaply.** Ask for the outline of a file, fetch exactly one function,
  or search by syntactic shape (`foo($A, $B)`) instead of reading whole files.
  The goal is a large, *measured* reduction in the tokens an agent spends on code
  exploration. The method is public and the numbers will be reproducible.
- **Edit safely.** Every modification is a two-step operation. First a
  **preview** produces a *plan*: the exact edits, a diff, and a content-addressed
  plan id. Then an **apply** step writes precisely what was previewed — and only
  if the files are still what they were, the result still parses, and every limit
  holds. Every apply is journaled and **undoable**.
- **Stay inside the lines.** One path policy guards every file access. Writes are
  **off by default**; in read-only mode the write tools do not even appear in the
  tool list. A human can review a plan the agent produced and apply it with the
  CLI, so the agent never needs write access at all.

## Why another tool

Several MCP servers expose tree-sitter or [ast-grep](https://ast-grep.github.io)
to agents, and they are good at *searching*. This project exists for the part that
is harder to get right: **letting an agent change code without trusting the
agent**. The design goal is that you can read the threat model
([`docs/SECURITY-MODEL.md`](docs/SECURITY-MODEL.md)), find every claim backed by a
named test ([`docs/TESTING.md`](docs/TESTING.md)), and decide for yourself.

It is a companion to a language-server MCP such as
[opencraylsp](https://github.com/amgio38/opencraylsp), not a replacement for one:

| | language server (semantic) | opencrayast (structural) |
|---|---|---|
| Knows | types, definitions, references across files | the shape of the syntax tree |
| Good at | "who calls this?", "what is the type?" | "list every function with this signature", "rewrite every call of this form" |
| Writes files | never | only when explicitly enabled, only through a reviewed plan |

A typical combined flow: find candidates by shape here → confirm meaning with the
language server → preview and apply the structural edit here → check the result
with the language server's diagnostics.

## Design principles

1. **Read-only by default; capability is explicit.** Write tools exist only when
   the operator launches the server with writing enabled, and a user-level policy
   can forbid it outright.
2. **Reviewed, then applied.** What gets written is byte-for-byte what was
   previewed. Apply does not re-run the search; it applies the recorded edits.
3. **One boundary.** Every path goes through one small, heavily tested component.
4. **Source files are hostile input.** Size, time and depth are bounded. Parsing is
   *designed* to run in an isolated worker with no filesystem or network access; that
   worker is not implemented yet, so today the budgets are the only bound.
5. **Fail closed, say why.** Errors carry a stable `[code]` and a next step.
6. **No daemon, no shell, no network.** There is no socket to authenticate and
   nothing is ever executed. Native Windows support follows from this — but the
   reading tools are what works there today; see
   [Platform support](#platform-support).
7. **Deterministic and bounded output.** Sorted, capped, and explicit about
   truncation — good for agents, good for diffs, good for tests.
8. **Claims are tested.** A guarantee in the documentation without a test that
   fails when the guarantee breaks is a bug in the documentation.

## What works today

All seven workspace crates exist and are populated. What that means concretely:

- **MCP server** (`opencrayast-mcp`): a stdio JSON-RPC server with `initialize`,
  `tools/list` and `tools/call`, strict input validation and size caps, and a read-only
  mode in which the write tools are absent from `tools/list` entirely.
- **CLI** (`opencrayast`): `doctor`, `plan list`, `plan show`, `plan gc`, and the six `edit`
  subcommands `preview`, `show`, `list`, `apply`, `undo`, `recover`. The three mutating
  subcommands (`apply`, `undo`, `recover`) require either an interactive confirmation or an
  explicit `--yes`, and refuse in a non-interactive run without it.
- **Tier 1 languages**: Rust, TypeScript, TSX, JavaScript, Python and Go, each behind an
  on-demand Cargo feature, with budgeted parsing behind a pure engine interface.
- **Edit engine**: preview produces a content-addressed plan; apply/undo/recover are
  reachable both as library handlers and, behind the human confirmation gate, as CLI
  subcommands, with journal, atomic multi-file write and crash recovery.

### The tool catalogue, honestly

Eleven tools are registered in the catalogue (`crates/tools`) and **all eleven are
routed** by the MCP `tools/call` dispatch table (`crates/mcp/src/dispatch.rs`). A
bidirectional catalogue↔dispatch test fails if either side drifts. Write-mode tools
(`ast_edit_apply`, `ast_undo`, `ast_recover`) appear in `tools/list` only when the
server is started with write enabled; in read-only mode they are absent from the
list (calling them is an unknown-tool error).

| Tool | Mode | Callable over MCP |
|---|---|---|
| `ast_info` | read | yes |
| `ast_outline` | read | yes |
| `ast_get` | read | yes |
| `ast_search` | read | yes |
| `ast_explain_pattern` | read | yes |
| `ast_plan_list` | read | yes |
| `ast_plan_show` | read | yes |
| `ast_edit_preview` | read (persists a plan) | yes |
| `ast_edit_apply` | **write** | yes, write mode only |
| `ast_undo` | **write** | yes, write mode only |
| `ast_recover` | **write** | yes, write mode only |

The catalogue is the single source of truth for the tool contract
([`docs/TOOLS.md`](docs/TOOLS.md)).

Initial language targets (Tier 1) — see [`docs/LANGUAGES.md`](docs/LANGUAGES.md).

### Where state lives, and how to delete it

Plans, journals (which hold the only copy of your original files) and the apply
lock live in your **user state directory**, never inside the workspace you are
editing:

| Platform | Path |
|---|---|
| Linux / unix | `$XDG_STATE_HOME/opencrayast`, or `~/.local/state/opencrayast` when `XDG_STATE_HOME` is unset |
| Windows | `%LOCALAPPDATA%\opencrayast` |

Inside it, each workspace gets `ws-<id>/`, where the id is derived from the
workspace's canonical path and device+inode — so two checkouts never collide and a
copied or renamed tree gets a fresh one.

Nothing ever writes state into your repository, and nothing in your repository is
needed to find it. If `XDG_STATE_HOME` and `HOME` are both unset, opencrayast
refuses to run rather than falling back to a directory inside the workspace.

To reclaim the disk:

```sh
rm -rf "${XDG_STATE_HOME:-$HOME/.local/state}/opencrayast"
```

That deletes every stored plan and every journal, so **every applied edit becomes
permanently unundoable**. To see what would go first:

```sh
opencrayast plan gc     # applies retention now: expired plans, aged journals
opencrayast doctor      # reports what gc would remove, and removes nothing
```

Neither runs on a schedule. Deleting undo history is your decision, not a side
effect of someone else's apply.

## Platform support

Reading works everywhere; **writing does not**, and the difference is not a
configuration flag.

| | Linux | macOS | Windows |
|---|---|---|---|
| `ast_outline` / `ast_get` / `ast_search` / `ast_explain_pattern` | yes | yes | yes |
| `ast_edit_preview` (stores a plan, writes no file) | yes | yes | yes |
| `ast_edit_apply` / `ast_undo` / `ast_recover` | yes | yes | yes (temp+rename; no directory fsync) |

On Windows, writes use a same-directory temp file then `rename` over the target
(NTFS rename is atomic). There is no directory fsync, and file identity is
`creation_time` rather than unix `dev`/`ino` — see
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) Platform notes. The isolated
parse worker is still unported, so on every platform today the parse budgets
(size, wall clock, depth, node count) are the only bound on a hostile source
file.

## Install in 30 seconds

```sh
curl -fsSL https://raw.githubusercontent.com/amgio38/opencrayast/main/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/amgio38/opencrayast/main/install.ps1 | iex
```

That installs the CLI (`opencrayast`) and the MCP server (`opencrayast-mcp`) —
into `~/.local/bin` on Linux and macOS, into `%LOCALAPPDATA%\opencrayast\bin` on
Windows. Each installer resolves in the same order: **a prebuilt release asset**
(verified against its published SHA-256), then **a local `dist/` tree** (verified
against its `SHA256SUMS`), then **a cargo build** from a checkout or a shallow
clone.

There is no tagged release yet, so the first path finds nothing today and the
third one runs. When the first tag is pushed the same script prefers the
prebuilt binary, with no edit here.

Register the MCP server with your agent:

```sh
claude mcp add opencrayast -- opencrayast-mcp --workspace .
```

### Platforms

| Platform | How |
|---|---|
| Linux, macOS | The command above clones and builds with cargo. Needs a Rust toolchain (1.95.0, see [`rust-toolchain.toml`](rust-toolchain.toml)) and a C compiler — on macOS run `xcode-select --install` first. |
| Windows | The reading tools work; **every edit is refused** because the atomic-replace primitive is unported. Build from a checkout with the MSVC toolchain (see [Platform support](#platform-support)). |

## Install details

### From a checkout

There is no crates.io entry yet (`publish = false` until the first release), so
`cargo install opencrayast` does not work and is not intended to:

```sh
git clone https://github.com/amgio38/opencrayast
cd opencrayast

# The CLI and the MCP server are two separate packages.
cargo install --locked --path crates/cli --root ~/.local
cargo install --locked --path crates/mcp --root ~/.local
```

Add `~/.local/bin` to your `PATH` afterwards. This builds every crate including the
five grammars, so it takes a few minutes. To leave languages out, pass the grammar
features explicitly — `--no-default-features --features lang-rust,lang-go` builds
two instead of five:

```sh
cargo install --locked --path crates/cli --root ~/.local \
  --no-default-features --features lang-rust,lang-go
```

### From a local artefact tree

If you already have a `dist/` tree — built with `make release-static`, or unpacked
from a release archive — install straight from it. The scripts verify
`SHA256SUMS` before copying anything and refuse a truncated sums file, so a
tampered or partial artefact cannot be installed:

```sh
make release-static                          # produces dist/ (Linux x86-64 musl)
./install.sh --dist dist --prefix "$HOME/.local"
```

```powershell
.\install.ps1 -DistDir .\dist -Prefix "$env:LOCALAPPDATA"
```

`install.sh` / `install.ps1` install from a **local** tree only and deliberately
download nothing. If `dist/` is absent they fail with that exact instruction
rather than guessing a URL — see [`docs/RELEASE.md`](docs/RELEASE.md) for what
changes when a tagged release exists.

## Build

Requires a Rust toolchain pinned by [`rust-toolchain.toml`](rust-toolchain.toml)
(1.95.0, edition 2024).

```sh
# Build everything.
cargo build --workspace --locked

# Run the whole test suite (add -- --ignored for the adversarial audit PoCs).
cargo test --workspace --locked

# Formatting and lints — both are enforced in CI, and clippy runs with -D warnings.
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Run the server over stdio:

```sh
cargo run --locked -p opencrayast-mcp -- --workspace .
cargo run --locked -p opencrayast -- doctor
```

Day-to-day contributor commands, job caps and the full pre-submission checklist are in
[`CONTRIBUTING.md`](CONTRIBUTING.md). The release and install path is in
[`docs/RELEASE.md`](docs/RELEASE.md).

## Documents

| Document | What it settles |
|---|---|
| [`ROADMAP.md`](ROADMAP.md) | Milestones, exit criteria, non-goals |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Crates, layering, process model, data flow, platforms |
| [`docs/SECURITY-MODEL.md`](docs/SECURITY-MODEL.md) | Assets, adversaries, threats, mitigations, residual risk |
| [`docs/EDIT-MODEL.md`](docs/EDIT-MODEL.md) | Plans, apply algorithm, journal, undo, recovery, invariants |
| [`docs/TOOLS.md`](docs/TOOLS.md) | Tool catalogue: arguments, output, errors |
| [`docs/PATTERNS.md`](docs/PATTERNS.md) | The pattern and rule language |
| [`docs/LANGUAGES.md`](docs/LANGUAGES.md) | Language tiers and how a language is added |
| [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) | Config, flags, defaults and limits |
| [`docs/TESTING.md`](docs/TESTING.md) | Test strategy; the threat-to-test matrix; benchmarks |
| [`docs/AGENT-GUIDE.md`](docs/AGENT-GUIDE.md) | How an agent should use the tools |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | Architecture decision records |

## Contributing and security

Please read [`CONTRIBUTING.md`](CONTRIBUTING.md) and the
[`Code of Conduct`](CODE_OF_CONDUCT.md). Report vulnerabilities privately; see
[`SECURITY.md`](SECURITY.md).

## License

[MIT](LICENSE).
