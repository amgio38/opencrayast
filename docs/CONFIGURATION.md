# Configuration

Everything an operator can set, where it is read from, and what the defaults are.

## The central rule, and the one honest exception

**Configuration comes from the operator's launch line and user file, not from the
workspace.** A repository an agent opens is untrusted; it must not be able to enable
writing, raise limits or change behaviour in any way. There is no project-level
configuration file in this product — neither shell reads `.mcp.json`,
`.vscode/mcp.json` or anything else from inside `--workspace`.

**The exception is `--config PATH`, and it is deliberate.** If the operator (or a client
that launches the server on their behalf) passes a path that happens to point **inside the
workspace**, that file **is** read, and after it passes the owner and 0600 checks its
`[limits]` and `[policy]` are in force. This is not a hole being exploited; it is a choice,
and the reason is that a shared, checked-in configuration is a reasonable thing for a team
to want, and a path is a path.

What that choice obliges us to provide is **visibility**, and it is what the rest of this
section is about:

- **The file in force is always named.** `ast_info` prints a `config:` line saying which file
  it is running on and which of the two routes chose it; `doctor` prints the same as its
  first check. There is no state in which a process is running on a configuration whose
  origin you cannot discover from inside the process.
- **`doctor` warns when that file is inside the workspace.** A warning, never a refusal: the
  operator is told in the one place they already look when something looks wrong, and
  nothing stops them. A `warn` still exits 0.
- **The permissions check still applies.** Being inside the workspace buys a file nothing:
  it must still be `0600` and owned by the user, or it is refused.

Files such as `.opencrayast.toml` inside a workspace are otherwise ignored — silently
ignored files are reported once by `doctor`.

## Sources and precedence

**What exists today, highest first:**

1. **Command-line flags** of the server (see the table below).
2. **The user configuration file:**
   - Linux/macOS: `$XDG_CONFIG_HOME/opencrayast/config.toml` (default `~/.config/…`)
   - Windows: `%APPDATA%\opencrayast\config.toml`
3. **Built-in defaults.**

There is **no environment-variable layer.** The ladder rung this document used to
promise at position 2 — a set of `OPENCRAYAST_*` variables — is not implemented, and
there is no code path that reads one: the only environment reads anywhere in `crates/`
are `HOME`, `XDG_CONFIG_HOME` / `APPDATA` (to locate the config file itself) and an
undocumented debug switch. So there is nothing to set, and a variable you export has no
effect on the server. The `[limits]` variables in the old table below have been removed
for that reason rather than left to imply a feature that does not exist.

Similarly, a **policy** key can only ever *restrict* what the flags offer, and policy
always wins. For writing the rule is stricter still: it is *opt-in at the user level* —
`policy.allow_write = true` **and** `--allow-write` are both required, so a
project-level client config that launches the server cannot turn writing on by itself.

**Provenance of the file is reported; provenance of each key is not.** This document used to
claim that `ast_info` and `doctor` "show, for every effective setting, which source supplied it".
Neither does. What they now show is **which file** is in force — `ast_info` prints a `config:`
line naming it and saying whether it came from `--config` or the user-level location, and
`doctor` prints that as its first check, adding a warning when the file is inside the
workspace. **Which key inside that file** set a given limit is still not reported: `ast_info`
prints the effective limits and the source of the file they all came from, nothing finer. Until
a value carries its own source there is no ladder above file-versus-default to report on, which
is a second reason the environment layer is not a documented gap but an unbuilt feature.

A user file that exists but is owned by another user, or is writable by group or
others, is refused and the server will not start. A configuration error never falls
back to a looser setting: the error says what is wrong and exits.

### The user configuration file

The file is read once at startup, before any request. What is accepted:

| Property | Accepted | Refused, and why |
|---|---|---|
| Owner | You | Somebody else's file is a way to have your limits edited by another account |
| Mode | `0600`, `0400` — owner read (and write), nothing else | Any group or other access, **and any owner-execute bit** |

The mask is `mode & 0o177`, so a mode of `0700` is refused. It used to be
`0o077`, which accepted `0700` and `0400`: the configuration file is never executed, so
that was not a security hole, but a `0700` mode is nearly always a `chmod -R` that caught
more than was meant, and accepting it quietly meant a file nobody intended to be
executable was accepted. The refusal is `[config_untrusted]`, not `[invalid_args]` — the
file is not wrong, the machine is — and it says so.

### Duplicate keys

A key may appear **once per section**. A file that sets the same key twice is refused
with both line numbers named, because the same key in two different sections is
perfectly legal and only the repeat is a problem:

```toml
[limits]
max_results = 200     # refused: `max_results` was already set on line 2
max_results = 900
```

Real TOML rejects a duplicate key. This reader does too, and the reason is not
pedantry: `allow_write` is what turns writing on, and a file that reads as
`allow_write = false` to a person, in scrollback and to `grep` must not enable write
mode because the same key was written twice with the last one winning. A duplicate is a
typo of the same kind as an unknown key, which is also refused, and it was the one case
that was not.

### Exit codes

`opencrayast-mcp` exits with a distinct status per failure class. The CLI has its own
table (1 user, 2 environment) in its `--help`; these digits differ because this
binary's status is also the status of a server a client launched.

| Status | Class | Means |
|---|---|---|
| 0 | success | The server ran, or `--help` printed usage on stderr |
| 1 | unexpected | The transport failed or something unforeseen; the message says what |
| 2 | usage | Bad or missing arguments, including an unlisted flag |
| 3 | user | A well-formed request the server refuses — and a **malformed** configuration file, which is your own text being wrong and which no retry fixes |
| 4 | environment | The machine cannot do it: a **configuration file that cannot be trusted**, an unusable state directory, an unreadable workspace |

The classification — is this the operator's fault or the machine's — is made once, in
`opencrayast-core`, and both shells ask the same function for it, so they cannot drift
apart again. `crates/mcp/tests/exit_code_parity_spec.rs` compares them file by file.

## Flags

These are the flags `opencrayast-mcp` accepts — the complete set, from
`crates/mcp/src/main.rs`. Anything else is refused at startup with
`unknown argument`, so a client that passes an unlisted flag gets a server that will not
start.

| Flag | Default | Meaning |
|---|---|---|
| `--workspace DIR` | *(required)* | The root all paths must stay inside. Must exist; `/` and a bare home directory are refused |
| `--read-root DIR` | — | An extra **read-only** root, repeatable. A file under one is readable and is displayed as `@root1/…`, `@root2/…` in the order the flags were given; it is **never** writable (BND-19), however the path is spelled. Each root is validated by the same check as `--workspace`, plus the read-root-only refusals: `/`, a drive root, the home directory itself, and key/credential directories are refused (CFG-07) and the process exits non-zero. `opencrayast doctor` prints one line per root with its `@root<N>` label. Roots come from the command line, not from the configuration file, so a file inside a repository cannot widen what an agent may read |
| `--allow-write` | off | Request write mode. Has **no effect** unless the user file also sets `policy.allow_write = true` (default `false`), so a project-level client config that launches the server cannot turn writing on by itself |
| `--config PATH` | `$XDG_CONFIG_HOME/opencrayast/config.toml`, else `~/.config/opencrayast/config.toml` (Windows: `%APPDATA%\opencrayast\config.toml`) | Read the user configuration from here instead of the default location. **The path is honoured wherever it points, including inside `--workspace`** — see the central rule above; a file inside the workspace is still subject to the 0600 and owner checks, and `doctor` warns when one is. A path that does not exist is not an error; a path that exists and is malformed, world-writable or owned by another user is refused. `ast_info` and `doctor` both report which file is in force and which route chose it |
| `--help`, `-h` | — | Print usage on **stderr** and exit 0, so a curious client cannot poison the protocol wire |

### Flags that are documented elsewhere but not implemented

These appear in older revisions of this document and in the "Isolation modes" and
"State directory" sections below. **None of them is accepted**, and passing one aborts
startup:

| Flag | Status | Notes |
|---|---|---|
| `--state-dir DIR` | **not implemented** | `BoundaryConfig::state_dir` is a real, enforced field — the write policy refuses any target inside it (BND-15), and `Settings::boundary_config` populates it — but no command-line flag sets it. The location is the platform user-state directory: `$XDG_STATE_HOME/opencrayast`, `~/.local/state/opencrayast`, or `%LOCALAPPDATA%\opencrayast`. An operator who needs a different one can set `XDG_STATE_HOME` for the process |
| `--languages LIST` | **not implemented** | Language selection is not yet configurable |
| `--isolation MODE` | **not implemented** | See [Isolation modes](#isolation-modes): there is no `--isolation` flag to set, and no mode to select |
| `--log-level LEVEL` | **not implemented** | Logs go to stderr; there is no level to choose |

The same holds for `[protect] extra` on the configuration side: extra protected globs
are an enforced `BoundaryConfig` field (`extra_protected`), but the server builds its
boundary through `Settings::boundary_config`, which sets none of them.

## User file

The parser accepts **exactly two sections**, `[policy]` and `[limits]`, and exactly one
policy key. Anything else — an unknown section or an unknown key — is a **refusal, not a
warning**: the server exits with the error rather than starting on defaults. So the file
you can actually copy is this one, and it is the whole of what is accepted:

<!-- accepted-example:start -->
```toml
# ~/.config/opencrayast/config.toml
#
# Every key below is at its default. Delete what you do not want to change; every
# [limits] entry is optional. This file parses as written.

[policy]
# Default false. Write mode needs BOTH this key set to true AND --allow-write.
# When false, write mode cannot be enabled by any flag or variable.
allow_write = false

[limits]
max_file_bytes            = 4194304    # 4 MiB    (hard max 16777216)
max_output_bytes          = 65536      # 64 KiB   (hard max 262144)
max_results               = 200        #          (hard max 1000)
max_scan_files            = 5000       #          (hard max 50000)
parse_timeout_ms          = 2000       #          (hard max 30000)
parse_max_depth           = 512        #          (hard max 4096)
parse_max_nodes           = 2000000    #          (hard max 20000000)
call_timeout_ms           = 10000      #          (hard max 120000)

# Path policy. path_max_depth also bounds the directory walk's ignored-depth count.
# TUNABLE: a value above the hard max is clamped to it, not refused (see below).
path_max_bytes            = 4096       #          (hard max 4096)
path_max_depth            = 64         #          (hard max 256)

# Edit plans. These are read from this file and stored as flat [limits] keys; the
# [plan] section an earlier revision of this document used is NOT accepted.
plan_ttl_minutes          = 15         #          (hard max 1440)
plan_max_files            = 50         #          (hard max 500)
plan_max_edits            = 500        #          (hard max 5000)
plan_max_changed_bytes    = 1048576    # 1 MiB    (hard max 8388608)
plan_max_store_mib        = 64         #          (hard max 1024)
plan_max_plans            = 100        #          (hard max 1000)
plan_max_plans_per_process = 25        #          (hard max 200)

# Journal retention.
journal_max_plan_mib      = 64         # originals per plan (hard max 128)
journal_retention_days    = 7          #          (hard max 90)
journal_max_total_mib     = 256        #          (hard max 4096)

note_max_bytes            = 1024       # plan note length in bytes (hard max 4096)
```
<!-- accepted-example:end -->

All 21 `[limits]` keys above are the complete set — there is one setter per `Limits`
field, so there is no key this parser knows that the block does not name, and no key
named here that the parser does not know. Both directions are asserted by
`crates/tools/tests/config_doc_example_spec.rs` against the real `Settings::parse`, so
the block cannot drift from the parser without failing the test suite.

### Not yet accepted — pasting this refuses to start

The sections below read as if they configured something. **They do not, and the parser
refuses the entire file** with `Unknown section` — the refusal is not a warning and not
an ignore, so a file containing any of these prevents startup. They are kept here as a
record of the settings the design intends, and as the list to check when the parser
grows:

<!-- not-accepted-example:start -->
```toml
# NONE OF THE SECTIONS BELOW ARE ACCEPTED. Settings::parse refuses this file.

[plan]
# The intent: plan lifetime and size. Today these are flat [limits] keys
# (plan_ttl_minutes, plan_max_files, ...) and the [plan] table is refused.
ttl_minutes = 15

[journal]
# The intent: journal retention. Today journal_retention_days and
# journal_max_total_mib are flat [limits] keys and this table is refused.
retention_days = 7

[protect]
# The intent: extra never-written globs. Not read from this file; the built-in
# protected-path list is compiled in and cannot be changed from configuration.
extra = ["secrets/**", "*.pfx"]

[languages]
# The intent: select and tier languages. Not read from this file.
enabled = ["rust", "typescript", "javascript", "python", "go"]
experimental_edits = []

[ignore]
# The intent: walk configuration. Not read from this file.
respect_gitignore = true
extra = ["vendor/**", "node_modules/**", "target/**"]
```
<!-- not-accepted-example:end -->

Two consequences worth stating plainly, because an earlier revision of this document got
both wrong:

- **The unwired sections are refused, not ignored.** This document used to say the
  unwired sections were "not read" and "have no effect", which understated a refusal
  into a falsehood: an operator who pasted them got a server that would not start.
- **`pattern_step_budget` is not a limit.** It appeared in the old example as though
  it were one; it is not a `Limits` field, and the parser refuses it as an unknown key.

### Ignore semantics of directory walks

**The `[ignore]` section is not accepted by the parser** (see the not-yet-accepted block
above); this describes the built-in behaviour of directory walks (outlines, search, get),
which does not read it. `respect_gitignore` and `extra` are not configurable today.

Directory walks behave as follows:

- **Which ignore files:** `.gitignore` files are honoured, each directory contributing
  one, read through the Boundary (`open_read`). A
  symlink, FIFO or other non-regular ignore file is not followed out of the root;
  a failed read is counted and its rules are not applied.
  contribute a file named `.gitignore`, read through the Boundary (`open_read`). A
  symlink, FIFO or other non-regular ignore file is not followed out of the root;
  a failed read is counted and its rules are not applied.
- **Size and encoding:** an ignore file larger than **1 MiB**, or any file that is
  **not valid UTF-8**, is unusable — **none** of its rules apply (no silent prefix).
- **Pattern subset** (gitignore-inspired, not full git): blank lines and `#`
  comments; `!` negation; leading `/` anchors to the directory holding the file; a
  `/` in the middle also anchors; trailing `/` matches directories only; `*` and `?`
  stay inside one path component; `**/` (leading), `/**` (trailing) and `/**/`
  (middle) match any number of components; patterns with no `/` match at any depth;
  the **last matching rule wins**; matching is case-sensitive. Character classes
  `[…]` and backslash escapes are **not** supported (treated as literal characters).
- **Degenerate `**`:** a pattern that is only two or more asterisks (`**`, `****`,
  …) matches **nothing** (avoids "ignore the whole tree" from a malformed rule).
- **Nesting:** rules of a nested `.gitignore` apply only beneath that directory;
  deeper files win when they match the same path.
- **`extra`:** the configured globs use the same subset, relative to the walk start.
- **Always skipped (counted as ignored):** built-in VCS directories `.git`, `.hg`,
  `.svn`, `.bzr` at any depth; paths whose workspace-relative component count would
  exceed `path_max_depth` (default **64**, from `[limits]`).
- **Never silent:** every deliberate skip is counted on the walk result (and shown
  in tool footers such as `ast_outline`'s `[skipped: …]`).

## What is wired, and what is not

This section exists because the sentence above it used to be a promise with no code path
behind it. `BoundaryConfig` gained a `limits` field and both path checks read it through
`Boundary::limits()` — and nothing in `crates/*/src` ever filled it in, so every caller
took the default while this file said the setting bound (SEC-FIX 5 CR: *"the container was
forged and nobody poured the water"*).

**Wired, end to end, tested (`crates/core/tests/config_limits_spec.rs`):**

| From the file | To | Test |
|---|---|---|
| `[limits]` → `path_max_depth`, `path_max_bytes` | `Boundary::resolve_read` | `CFG1-01` |
| `[limits]` → `path_max_depth` | the directory walk's depth ceiling | `CFG1-02` |
| `[policy]` → `allow_write` | `Settings::policy` | `CFG1-04` (parse), shells read it |

Every other `[limits]` key above is parsed into `Limits` as well — there is one setter per
field, so a rename in `limits.rs` has to be reflected in the parser rather than silently
ignored. What is **not** yet wired is the consumers: a limit that nothing reads yet is
parsed and validated but has no effect, and this file will say so rather than let you
believe otherwise.

**Not wired yet — read this before trusting a value:**

- The `[plan]`, `[journal]`, `[protect]`, `[languages]` and `[ignore]` sections are **not
  accepted by the parser at all.** A file containing one is refused outright with
  `Unknown section`, so "setting them here has no effect" understates it: pasting them
  stops the server from starting. See the
  [not-yet-accepted block](#not-yet-accepted--pasting-this-refuses-to-start) above for
  the full text and the list of settings each was meant to carry.
- The plan and journal stores are still constructed with `Limits::default()` by their
  callers, so the flat `plan_*` and `journal_*` keys above are parsed and validated but
  have no consumer yet. A limit that nothing reads yet has no effect.
- `pattern_step_budget` is **not a `Limits` field** and the parser refuses it as an unknown
  key rather than ignoring it.

A key this parser does not know is a **refusal, not a warning** (CFG1-04): a typo in a
safety limit must not leave the default quietly in place while the file reads as though it
were configured.

**The parser is deliberately small.** It reads `[section]` headers, `key = <integer>` lines,
`#` comments and blank lines — not nested tables, arrays, floats, strings or datetimes, and
a line it cannot parse is refused rather than guessed at. Adding a real TOML dependency is
a maintainer decision, not something to slip in. `Settings::load` also refuses a file that
is group- or world-readable or owned by another user (T-18 / CFG-05).

**One decision point.** `Settings::boundary_config(root)` is the only function that decides
which limits a `Boundary` is held to. `BoundaryConfig` has **no `Default`** — re-adding the
derive would break every construction site in the tree, which is a stronger guarantee than
any test: `bench.rs` used to build its measuring boundary from `Limits::default()` while
handing the tools the real ones, so it measured a boundary that was not the one it thought.

Every limit has a compiled-in hard maximum (a limit without one is a defect). Hard maxima are compiled in. They exist so that even a mistaken or tampered user file
cannot turn a safety limit into a resource-exhaustion path.

### Which ceilings are tunable, and what happens above the maximum

**The split is deliberate, and it is the whole content of this section.** The limits are not
all treated alike, and a value above a hard maximum does not always get the same answer.

| | Which limits | Above the hard maximum |
|---|---|---|
| **Tunable** | `path_max_depth` only | **Clamped** to `PATH_MAX_DEPTH_HARD` (256). The file still loads; the effective ceiling is 256 and the refusal message names 256. |
| **Fixed** | every other limit — `max_file_bytes`, `max_output_bytes`, `max_results`, `max_scan_files`, `parse_*`, `call_timeout_ms`, all `plan_*`, all `journal_*`, `note_max_bytes`, `path_max_bytes` | **Refused.** The configuration does not load and the shell reports `limits.<name> is <value>, above the hard maximum <max>`. |

**Why depth is the exception.** `path_max_depth` is not a budget an operator spends by
raising it — nothing gets more expensive for them. It is a statement about how deeply a
legitimate repository nests, and an operator who meets a pathological tree needs to widen it.
Refusing to start a shell over that would make the one knob that helps unusable. The guard is
**not** removed by the clamp: the depth ceiling stays bound at 256, and that value is
readable back off the boundary.

**Why the resource ceilings stay fixed.** Those are the ceilings that actually bound work —
bytes read, bytes returned, results, plan and journal sizes. There is no switch that turns
one off and there is no spelling of the file that raises one past its maximum. They are
**refused rather than clamped** on purpose: a ceiling that quietly became a smaller ceiling
is a guard the operator cannot see, and a shell that starts on a silently reduced ceiling is
worse than one that refuses to start and says why.

**Reading the effective value.** The requested number stays in the file's parse result, and
the *effective* one is `Limits::clamped_path_max_depth()` — the same value the resolver and
the directory walk enforce. A boundary reports it through `Boundary::limits()`.

**Zero is refused for every limit, depth included.** `0` is not "unlimited" and not
"clamp me down"; it is a mistake, and the refusal says so.

## Built-in protected paths

Writes are always refused for these, after canonicalisation and case folding. The list
is compiled in and is currently **not extendable from configuration** — the `[protect]`
section is not accepted by the parser (see above). The extra-glob mechanism itself is
real (`BoundaryConfig::extra_protected`, added to the built-ins); what is missing is
any way for an operator to populate it.

- Version-control metadata: `.git/**`, `.hg/**`, `.svn/**`, `.bzr/**`
- Secret-like names: `.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.kdbx`,
  `id_rsa*`, `id_ed25519*`, `.netrc`, `.npmrc`, `.pypirc`, `credentials*`,
  `*.keystore`
- The tool's own state directory and configuration file
- Anything outside the workspace

Reads of these are not blocked by this list (an agent may legitimately need to read
a config); reads are governed by the boundary and the ignore rules alone.

## Isolation modes

**There is no isolation mode to select today, and no `--isolation` flag to select it
with.** Only in-process parsing exists. The table below is the design, not the current
behaviour; until M6 lands, a server is always in-process.

| Mode | Meaning |
|---|---|
| `auto` (planned) | Use the isolated worker process if the platform supports the required restrictions; otherwise run in-process and say so. **Not implemented.** In write mode `auto` is intended to refuse to start without a worker unless `inproc` is chosen explicitly, and a worker that exceeds its restart bound is intended to make requests fail with a worker-unavailable error rather than fall back to in-process parsing |
| `process` (planned) | Always use the worker; fail to start if it cannot be restricted. **Not implemented** |
| `inproc` | Never use a worker (tests, debugging, platforms without support). **This is the only mode that exists** |

Until milestone M6, only `inproc` exists. What each mode is intended to guarantee on
each operating system is listed in [`SECURITY-MODEL.md`](SECURITY-MODEL.md) (T-08).
Note that the worker-restart-bound error described above is **not an `ErrorCode`
variant** — `[worker_unavailable]` appears in this document and in
[`TESTING.md`](TESTING.md) as a marker for a planned behaviour, not as a code the server
can currently emit, and `crates/core/src/error.rs` has no such variant. `ast_info` does
not report the executor or the restrictions in force either: it emits five fixed lines
(version and mode, workspace, languages, four limit summaries, write state) and no
isolation information.

## State directory contents and permissions

**Not yet configurable.** There is no `--state-dir` flag, so the location below is
whatever the platform default is; the `OPENCRAYAST_STATE_DIR` variable this section
used to mention is not read by anything.

Created `0700` and re-verified on every start (owner is the current user, mode has no
group/other access, the path is not a symlink). If verification fails the server
refuses to start rather than adopting a directory it does not control. See
[`ARCHITECTURE.md`](ARCHITECTURE.md#state-on-disk) for the layout.

## Client examples

Read-only (recommended default; humans apply plans with the CLI):

```json
{ "mcpServers": { "ast": { "command": "opencrayast-mcp", "args": ["--workspace", "/path/to/project"] } } }
```

Write mode (needs `allow_write = true` in your user file as well; use a client that asks you to approve destructive tool calls). Do not put this in a project-level client config of a repository you do not control:

```json
{ "mcpServers": { "ast": { "command": "opencrayast-mcp",
                            "args": ["--workspace", "/path/to/project", "--allow-write"] } } }
```

## `doctor`

`opencrayast doctor` prints the effective configuration, the state directory check, any
non-terminal journals, and ignored workspace-local config files. It exits non-zero if
the server would refuse to start.

It also reports a **`reclaimable`** line: how many stored plans and journals are past
their retention, with the command that would remove them (`opencrayast plan gc`).
`doctor` removes nothing — deleting a journal makes that plan's edit permanently
unundoable, so it is an operator decision, and `doctor` is a diagnostic.

If the state directory location cannot be determined at all (no `XDG_STATE_HOME` and
no `HOME` on unix, no `LOCALAPPDATA` on Windows), `doctor` reports that as a `fail`
and exits non-zero. There is no fallback to a directory inside the workspace.

It does **not** print, for each setting, which source supplied it, and it does not
report the isolation mode in use — there is only in-process parsing and no per-setting
provenance exists. (This paragraph corrects an earlier claim in this document that it did
both.)
