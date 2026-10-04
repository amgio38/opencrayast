# Release readiness checklist

Every item below is either **mechanically verified** or an **explicit manual sign-off**. There
is no third category: an item that cannot be checked by a script is listed as a manual line and
is reported as `PENDING` until a person acknowledges it, so it cannot pass by being ignored.

## How to use it

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4

# Mechanical checks. Reports manual items as PENDING but does not fail on them;
# this is the mode CI runs in.
bash scripts/verify-release.sh

# See what still needs a human.
bash scripts/verify-release.sh --list-manual

# The release mode: mechanical checks AND every manual item signed off.
# Exits non-zero while any manual item is open.
bash scripts/verify-release.sh --require-manual

# After a person has actually confirmed each manual item.
bash scripts/verify-release.sh --ack-manual M1,M2,M3,M4,M5,M6,M7
```

### Why `--require-manual` is opt-in

CI runs the default mode on every PR, and CI cannot sign for a human. If unacknowledged manual
items failed the default run, every PR would be red forever and the check would be deleted
rather than obeyed. So the default run **reports** each open item as `PENDING` on stdout and
does not fail on it — it is still never silently passed, which is the actual requirement. The
release process, where a person is present, passes `--require-manual` and the open items do
fail the run. `scripts/tests/verify_release_spec.sh` pins both halves of this, because a
checklist that ignores manual items and one that is permanently red are both failures.

The manual items are recorded in the release PR, together with this script's output, so the
checklist is evidence rather than intention. This document replaces the unticked boxes in
[`PRERELEASE.md`](PRERELEASE.md) as the runnable version of the same walk-through; that document
keeps the prose rationale and the artefact steps.

Do not tag if `scripts/verify-release.sh` is non-zero, if any manual item is unacknowledged, or
if CI is red on any OS. Fix forward; do not move an existing tag.

## Mechanical checks

Each is asserted with an exit status by `scripts/verify-release.sh`. The `observed` column is
from the run recorded in [`RELEASE-READINESS.md`](RELEASE-READINESS.md).

| Id | What is checked | Observed |
|---|---|---|
| `PKG-01` | `LICENSE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CHANGELOG.md`, `ROADMAP.md` exist and are non-empty | pass |
| `PKG-02` | bug, feature and config issue forms plus the PR template exist and are non-empty | pass |
| `PKG-03` | `.github/dependabot.yml` declares at least one package ecosystem | pass |
| `LIC-01` | `Cargo.toml` `license` and the `LICENSE` file agree (MIT) | pass |
| `LIC-02` | `LICENSE` contains the MIT grant text, not a placeholder | pass |
| `VER-01` | exactly one `workspace.package` version, matching the documented `N.YYYYMMDD.N` scheme, and every crate inherits it with no literal version | pass — `0.20261002.1`, 7 crates |
| `VER-03` | `rust-toolchain.toml` pins a channel and agrees with `rust-version` | pass — `1.95.0` / `1.95` |
| `DOC-01` | `scripts/check-docs.sh` passes: links, anchors, personal paths, CJK, internal ids, script paths | pass — 25 files |
| `DOC-02` | `scripts/check-matrix.sh` passes: every threat has a named test | pass — 34 threats, 91 targets resolved |
| `DOC-03` | `scripts/check-layering.sh` passes | pass — 7 crates |
| `DOC-04` | `scripts/check-undeclared-src.sh` passes: every `crates/*/src/**/*.rs` is reached from a crate root by a `mod` (or `#[path]` `mod`) | added |
| `DENY-01` | `cargo deny check` passes | pass — advisories, bans, licences, sources |
| `GIT-01` | every `uses:` in the workflow is pinned to a full 40-hex SHA | pass — 4 actions |
| `CI-01` | the coverage gate and its self-test are wired into the workflow | pass |
| `CI-02` | the undeclared-src gate and its self-test are wired into the workflow | added |
| `GIT-02` | the working tree has no uncommitted changes | **fail at the time of writing** — see below |
| `HIST-01` | `CHANGELOG.md` has an `Unreleased` or versioned section | pass |

`DENY-01` is reported as `SKIP`, not `PASS`, when `cargo-deny` is not installed. A skipped check
is never counted as a passed one.

### On `GIT-02` failing on its own commit

The first run of this script failed `GIT-02` with `?? scripts/verify-release.sh`, because the
script was inspecting a tree that contained itself as an untracked file. That is the check
working correctly — it refused to certify a release from a dirty tree — and it is recorded
here rather than quietly re-run until it went green. On a clean checkout of the committed tree
it passes.

## Manual sign-off

These cannot be automated. The script prints every one of them as `PENDING` on every run — they
are never silently passed — and `--require-manual` or `--ack-manual` turns an open one into a
failure. `M5` in particular cannot be mechanised at all: scanning git history for secrets needs a
decision about what counts as a secret.

| Id | Item | Why a script cannot decide it |
|---|---|---|
| `M1` | CI green on `ubuntu-latest`, `macos-latest` and `windows-latest` for the merge commit | Only the hosted runners can run the matrix; a local green run is not the same claim |
| `M2` | Release artefacts built and checksums recorded. **Covers `install.sh` on all three platforms; the Windows installer `install.ps1` exists and its nine contract cases run, but its three execution cases (REL2-09/10/11) need a PowerShell runner and have never run** | Requires the cross-compile toolchain, a place to publish, and — for the Windows installer — a Windows or PowerShell runner |
| `M3` | `CHANGELOG.md` names every user-visible change in the tag | Requires knowing what is user-visible; `HIST-01` only proves a section exists |
| `M4` | The supported-version sentence in `SECURITY.md` is still true | A judgement about what is still supported |
| `M5` | History scanned for secrets, personal data and internal references | Needs a policy decision, not a pattern match |
| `M6` | A maintainer reviewed the tag before it was pushed | A human act |
| `M7` | The private vulnerability-reporting path was rehearsed once end to end: a synthetic report received through the channel `SECURITY.md` names, walked against the 7/14-day SLA, and answered | A human act. The documents are self-consistent and the mechanical checks prove that; what has never been tested is a person holding the maintainer role working the clock. Until this is done, the SLA in `SECURITY.md` is an untested commitment, and `SECURITY.md` says so |

## What this checklist does not prove

- **That the build works on an OS you did not run.** `M1` is a manual line precisely because a
  local pass says nothing about macOS or Windows.
- **That the artefacts are correct.** `M2` is manual; the script does not build, sign or
  publish anything.
- **That the code is safe.** Every check here is a consistency and hygiene check. The security
  claims are held by the threat-to-test matrix (`DOC-02`) and by the per-test suites, not by
  this file.
- **That the history is clean.** `M5` is manual, and a clean working tree (`GIT-02`) says
  nothing about what was committed in the past.