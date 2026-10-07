# Contributing

Thanks for helping. This project writes to people's source trees on behalf of AI
agents, so it is held to a higher bar than most tools of its size. The bar is not
meant to discourage you; it is written down so it is predictable.

The design documents under [`docs/`](docs) are normative. Code lands milestone by
milestone as laid out in the [roadmap](ROADMAP.md). When behaviour and documents
disagree, fix them together in the same change.

## Ground rules

1. **Spec first.** A change to a guarantee, the layering, the plan or tool contract,
   or the dependency policy starts as a change to the documents and, where it is a
   decision, an ADR in [`docs/DECISIONS.md`](docs/DECISIONS.md). Code follows.
2. **Every claim has a test.** If you add or change a security-relevant behaviour,
   add or change the row in the threat model, the test in the catalogue
   ([`docs/TESTING.md`](docs/TESTING.md)) and the test itself.
   `scripts/check-matrix.sh` fails CI when they drift apart.
3. **A negative test for every check.** Show that removing your safety check turns a
   test red.
4. **One boundary.** File access goes through `Boundary`. There is no second way to
   open a path, and a pull request that adds one will not be merged.
5. **Layering.** Dependencies point downward only
   ([`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)). `unsafe` is forbidden
   workspace-wide (`[workspace.lints.rust] unsafe_code = "forbid"` in the root
   `Cargo.toml`). If an exception is ever required, only a dedicated safety-boundary
   crate may hold it, and only after an ADR; no such crate exists today.
6. **Hostile input everywhere.** Source files, file names, symlinks, plans on disk
   and MCP messages are all untrusted. Bound them; never panic on them.
7. **Cross-platform means the three of them.** Linux, macOS and Windows. A feature
   that cannot work on one says so in code and in the docs.
8. **English, plainly.** Code, comments and documentation are English. No personal
   paths, secrets, private tracker links or private data in the repository. Opaque
   work-item ids in commit messages are allowed (see Commit messages below); do not
   turn them into links to a private tracker.

## Development setup

Toolchain is pinned in `rust-toolchain.toml` (currently 1.95.0 with `rustfmt` and
`clippy`).

```sh
git clone https://github.com/amgio38/opencrayast
cd opencrayast

# Throttle parallel work on shared builders (see "Why the job caps" below).
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4

cargo build --workspace --locked
cargo test --workspace --locked
```

Do **not** run two `cargo` invocations at the same time against the same
`target/` directory. Each clone or worktree keeps its own `target/`; do not point
`CARGO_TARGET_DIR` at a directory shared with another checkout.

### Why the job caps

`CARGO_BUILD_JOBS=8` limits rustc codegen units Cargo schedules in parallel.
`RUST_TEST_THREADS=4` limits how many libtest threads run at once. Together they
keep peak RAM and load predictable on machines that also host other worktrees and
CI jobs. Uncapped `cargo test --workspace` on this repository fans out many
integration binaries (hostile fuzz-style suites, undo, boundary) and can thrash a
shared host. The caps are a local / shared-host convention; CI does not require
them.

### Release and static builds

The workspace `[profile.release]` enables LTO, a single codegen unit, and strip.
A normal optimised build:

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
cargo build --release --workspace --locked
```

Linux prebuilt binaries are intended to be **fully static** (roadmap M7). On a
host with the musl target installed (`rustup target add x86_64-unknown-linux-musl`):

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
cargo build --release --workspace --locked --target x86_64-unknown-linux-musl
```

The resulting `opencrayast` and `opencrayast-mcp` binaries under
`target/x86_64-unknown-linux-musl/release/` should report as statically linked
(`file` shows `static-pie linked`; `ldd` reports `statically linked`). The same
shape applies for `aarch64-unknown-linux-musl` when that target is installed.

### Checks that match CI

Before opening a pull request, run the same gates as
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) (there is no separate Make
target; copy these commands):

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
cargo deny check
sh scripts/check-docs.sh
sh scripts/check-matrix.sh
bash scripts/check-layering.sh
bash scripts/test-check-layering.sh
bash scripts/check-undeclared-src.sh
bash scripts/test-check-undeclared-src.sh
```

`cargo deny` needs the [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)
binary on your `PATH` (`cargo install cargo-deny` once). CI installs it via the
pinned `cargo-deny-action`.

Documentation HTML (optional locally; listed on the pre-release checklist):

```sh
cargo doc --workspace --no-deps --document-private-items
```

Full maintainer release steps: [`docs/MAINTENANCE.md`](docs/MAINTENANCE.md).
Pre-release checklist: [`docs/PRERELEASE.md`](docs/PRERELEASE.md).

## Commit messages

Use a short imperative subject, optionally with an area prefix:

```text
edit: refuse apply when the journal envelope is truncated

Explain the why in the body when the subject is not enough.
Name the tests or commands you ran.
```

- Prefer linking a **public GitHub issue** with `Fixes #123` or `Refs #123` in the
  body when one exists.
- Do not put personal paths, secrets, private tracker links or private data into
  the subject, body, or history. Internal work-item ids (for example
  `#PROJECT/REQ-.../ISSUE-...`) may appear; they must stay opaque — never a link
  to a private tracker.

## Pull request checklist

Use the GitHub pull request template. In short:

- [ ] Single-purpose change; the description says what and why.
- [ ] `make preflight` passed on the exact tree you are pushing (it runs what CI runs on
      Linux: format, clippy, the whole test suite, docs check, gate self-tests, coverage).
- [ ] Commands you ran and what they showed (not only "tested").
- [ ] Documents and ADRs updated in the same PR when a guarantee, contract,
      layering rule or dependency policy changed.
- [ ] Security-relevant change: threat-model row, test-catalogue entry and test
      updated; `scripts/check-matrix.sh` passes.
- [ ] A safety check was shown to be tested (remove it, see a named test fail,
      restore it).
- [ ] All file access goes through `Boundary`; `unsafe` stays forbidden
      (`unsafe_code = "forbid"`). Any future exception needs an ADR and a dedicated
      safety-boundary crate that does not exist today.
- [ ] Documentation claims match the tree: for doc-only PRs, search the Markdown
      for the claim (not one keyword) and list every hit; do not leave the same
      false present-tense fact in a second file.
- [ ] Linux, macOS and Windows considered (CI green on all three).
- [ ] `CHANGELOG.md` updated for user-visible changes.
- [ ] No secrets, personal paths, private tracker links or private data. Opaque
      work-item ids in commits are fine; do not link a private tracker.

## Adding a language, a tool or an edit kind

Follow the checklists in [`docs/LANGUAGES.md`](docs/LANGUAGES.md#adding-or-updating-a-grammar)
and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md#extension-points-and-their-limits).
There is deliberately no plugin API.

## Reporting problems

Bugs and feature requests: use the issue forms. **Security problems: do not open a
public issue** — see [`SECURITY.md`](SECURITY.md).

## Code of conduct and licence

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md). By
contributing you agree that your contributions are licensed under the
[MIT licence](LICENSE).
