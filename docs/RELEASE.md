# Release chain

How a release artefact is built, laid out under `dist/`, checked, and installed
from a **local** tree. Day-to-day contributor commands stay in
[`CONTRIBUTING.md`](../CONTRIBUTING.md). Maintainer version / changelog rules stay
in [`MAINTENANCE.md`](MAINTENANCE.md).

## Current status (honest)

| Piece | Status today |
|---|---|
| Version single source | In force: `[workspace.package].version` in the root `Cargo.toml` |
| Static musl build | `make release-static` (see below) |
| `dist/` layout | Produced by `make release-static`; directory is gitignored |
| Local install + checksum | `install.sh` (default) |
| Download from a GitHub release | **Not enabled.** There is no public tagged release yet. The download entry point in `install.sh` refuses with a clear message until one exists |
| Release workflow | `.github/workflows/release.yml` exists and is tag-triggered, with an all-or-nothing three-platform matrix and `dist/SHA256SUMS` generation. **It has never been executed** — see below |
| Dependency advisory audit | `.github/workflows/audit.yml` runs `cargo audit` on a weekly schedule, plus on demand and on release tags |
| Supply-chain gate | `cargo deny check` against committed `deny.toml` (no extra allows without an ADR) |

## 1. Version: one place to edit

Sole literal: root [`Cargo.toml`](../Cargo.toml) `[workspace.package] version`.
Every member crate must use `version.workspace = true` and must not write
`version = "…"`.

The released tree carries the same string in `dist/VERSION` (one line, no `v`
prefix). That file is written by `make release-static` from the workspace
manifest; it is not a second source of truth.

Locked by **REL1-01 … REL1-03** (`make test-rel1`).

## 2. Reproducible static build

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
make release-static
```

What the target does:

1. `cargo build --release --workspace --locked --target x86_64-unknown-linux-musl`
2. Stages binaries and sidecar files into `dist/` (layout below)
3. Writes `dist/SHA256SUMS` for the binaries

`CARGO_BUILD_JOBS=8` and `RUST_TEST_THREADS=4` are the shared-host caps from
[`CONTRIBUTING.md`](../CONTRIBUTING.md#why-the-job-caps). `--locked` refuses a
lockfile drift. The workspace `[profile.release]` already sets `lto`,
`codegen-units = 1`, and `strip`.

### Reproducibility note

On the maintainer Linux host used for REL1, two consecutive clean
`cargo build --release --workspace --locked --target x86_64-unknown-linux-musl`
runs produced **byte-identical** binaries (same SHA-256) **without** setting
`SOURCE_DATE_EPOCH`. If a future toolchain or dependency makes hashes diverge,
find the non-deterministic input first; do not paper over it with
`SOURCE_DATE_EPOCH` until the cause is known. `make repro-check` runs the
two-build comparison.

Locked by **REL1-06**.

## 3. Fixed `dist/` layout

After `make release-static` the tree **must** be exactly:

```text
dist/
  VERSION
  LICENSE
  THIRD-PARTY-LICENSES.md
  README.md
  SHA256SUMS
  x86_64-unknown-linux-musl/
    opencrayast
    opencrayast-mcp
```

| Path | Content |
|---|---|
| `VERSION` | Workspace Cargo version, one line (example `0.20261002.1`) |
| `LICENSE` | Copy of the repository root `LICENSE` |
| `THIRD-PARTY-LICENSES.md` | Copy of the repository root third-party licence notices. **Required in the artefact, not only in the repository**: the binaries statically link five tree-sitter grammars and the Rust dependency tree, so a tarball without these notices redistributes licensed work with no notice attached |
| `README.md` | Short use notes for this artefact tree (generated) |
| `SHA256SUMS` | `sha256sum` lines for the two binaries, paths relative to `dist/` |
| `x86_64-unknown-linux-musl/opencrayast` | Static CLI binary |
| `x86_64-unknown-linux-musl/opencrayast-mcp` | Static MCP server binary |

Do not add other top-level names without updating this document and the Makefile
in the same change. Multi-arch triples, when they ship, get sibling directories next
to `x86_64-unknown-linux-musl/` and matching lines in `SHA256SUMS`.

Locked by **REL1-05**.

## 4. `install.sh` (local only for now)

```sh
# From the repository root, after make release-static:
./install.sh
# or:
DIST_DIR=./dist PREFIX="$HOME/.local" TARGET=x86_64-unknown-linux-musl ./install.sh
```

Behaviour:

1. Resolve `PREFIX`: `--prefix` / `PREFIX`, else `$HOME/.local`. If both `PREFIX`
   and `HOME` are unset, exit non-zero with an install.sh message (not a bare
   shell "unbound variable") and install nothing.
2. Require that **each binary about to be installed**
   (`<target>/opencrayast` and `<target>/opencrayast-mcp`) has a line in
   `SHA256SUMS`. A truncated sums file that omits one of them is refused even if
   `sha256sum -c` would succeed on the remaining lines.
3. Run `sha256sum -c SHA256SUMS` so every listed digest matches (**before**
   copying).
4. On any failure: print the error, **exit non-zero**, leave `PREFIX` unchanged.
5. On success: install the two binaries into `PREFIX/bin/`.

Download / URL install is **disabled**. Passing `--from-release` (or setting
`INSTALL_FROM_RELEASE=1`) exits non-zero with a message that the path waits for
the first tagged public release. That is intentional, not a bug.

Locked by **REL1-07** (good tree), **REL1-08** (tampered digest), and **REL1-09**
(truncated `SHA256SUMS` that omits a binary we would install).

## 4a. `install.ps1` (Windows)

```powershell
# From the repository root, after a Windows release build:
.\install.ps1
# or:
.\install.ps1 -DistDir .\dist -Prefix $env:LOCALAPPDATA\opencrayast -Target x86_64-pc-windows-msvc
```

PowerShell 5.1 or later. **No elevation is requested**: it writes only under the chosen
prefix, and on Windows the default prefix is `$env:LOCALAPPDATA\opencrayast` because there
is no `$HOME` and `%USERPROFILE%\.local` is not on `PATH`.

It enforces **the same four gates in the same order** as `install.sh`, and the order is the
contract rather than an implementation detail:

1. both install paths (`<target>/opencrayast.exe`, `<target>/opencrayast-mcp.exe`) must be
   **listed** in `SHA256SUMS` — a truncated sums file is refused even if every remaining
   digest would verify;
2. **every** listed digest must verify, before `PREFIX` is touched at all;
3. both binaries must **exist**;
4. only then is anything copied, and only those two files — the copy list is a whitelist,
   whatever `SHA256SUMS` happens to name.

Two Windows-specific details that are not incidental:

- **A UTF-8 BOM in `SHA256SUMS` is stripped before parsing.** Otherwise the first digest
  field becomes `<BOM>deadbeef` and a clean tree is refused for a reason that has nothing to
  do with tampering — a false refusal, which is the same bug class as a false pass.
- **Download-from-release is disabled here too.** `-FromRelease` (or `INSTALL_FROM_RELEASE=1`)
  exits non-zero with the same message `install.sh` gives. Inventing a URL for a release that
  does not exist would be worse than refusing.

### What is verified, and what is not

`scripts/tests/rel2_install_ps1_spec.sh` splits its cases, and the split is the point:

| Cases | What they check | Where they run |
|---|---|---|
| **REL2-01 … 08** | the properties that decide whether the installer is *safe*: gate order, "nothing touches `PREFIX` before verification", the two-file whitelist, BOM handling, no elevation, no invented URL | **everywhere**, including CI — they are properties of the script's text and need no PowerShell |
| **REL2-09 … 11** | **execution**: a good tree installs, a tampered digest refuses and leaves the prefix empty, a truncated sums file refuses | **a Windows or PowerShell runner only** |

**REL2-09/10/11 have never been executed.** They are written and wired, and on a machine
without `pwsh` they print `SKIP` with the reason and the run still exits zero. **A green
`rel2` self-test is therefore not end-to-end proof of the Windows installer**, and this
document says so where someone deciding whether to trust the installer will read it. The
same statement appears in the script's own output, so it cannot be missed by someone who
only reads the test log.

Making those three run for real needs the `x86_64-pc-windows-msvc` leg of the release matrix,
which has not executed either (see §7).

## 5. Supply chain: `cargo deny`

### 5a. Advisory audit: `cargo audit`

`ci.yml` runs `cargo-deny` on every push. Advisories are re-checked separately by
`.github/workflows/audit.yml`, which runs `cargo audit` **weekly** (Monday 03:17 UTC),
on manual dispatch, and on any `v*` tag.

The RustSec advisory database is fetched over the network at run time; no snapshot is
committed. That is deliberate — a vendored snapshot would report "no known
vulnerabilities" for anything published after it was taken, which is worse than an
obviously-live fetch. `deny.toml` stays the policy file for `cargo-deny` and is
unchanged by this.

**What the schedule does not promise.** A GitHub Actions `schedule:` trigger is
best-effort: it fires only on the default branch, GitHub suspends scheduled workflows
in a public repository with no activity for 60 days, and the timer is not exact. The
defensible claim is "advisories are re-checked weekly while the repository is
active", not "a vulnerability is caught within N days". The per-commit floor is
`cargo-deny`, which runs on every push.

### 5b. Licence and source policy: `cargo deny`

```sh
make deny
# equivalent:
cargo deny check
```

Policy file: [`deny.toml`](../deny.toml). CI runs the same check on Linux. Do
**not** add licence or advisory allows without a maintainer decision (ADR).

`cargo deny` may print `license-not-encountered` warnings for entries on the
allow list that no current dependency uses (today that includes `Zlib`, and may
include others such as `BSD-2-Clause` / `ISC` / `CC0-1.0` depending on the
lockfile). Those rows are an **intentional foresight allowlist**: licences we
already decided are acceptable if a future, reviewed dependency brings them in.
They are not silent exceptions for crates already in the tree. Removing one is a
deliberate policy shrink; adding one still needs an ADR. Document any future
exception here and in `docs/DECISIONS.md` together.

Locked by **REL1-04**.

## REL1 test catalogue

Run all of them with:

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
make test-rel1
```

| Id | Claim |
|---|---|
| REL1-01 | Root `Cargo.toml` has exactly one `[workspace.package]` `version = "…"` |
| REL1-02 | No `crates/*/Cargo.toml` contains a literal `version = "…"` |
| REL1-03 | Every `crates/*/Cargo.toml` contains `version.workspace = true` |
| REL1-04 | `cargo deny check` exits 0 |
| REL1-05 | `make release-static` produces the layout in §3 |
| REL1-06 | `make repro-check`: two clean static builds, identical SHA-256 for both binaries |
| REL1-07 | `install.sh` against a good `dist/` exits 0 and installs both binaries |
| REL1-08 | `install.sh` against a `dist/` with a corrupted `SHA256SUMS` exits non-zero and installs neither binary |
| REL1-09 | `install.sh` against a `dist/` whose `SHA256SUMS` omits one install path (truncated file) exits non-zero and installs neither binary, even if the listed digests still match |
| REL1-10 | Every workspace path dependency in the root `Cargo.toml` pins the same version as `[workspace.package].version` |

### Publish metadata

Each crate carries `description`, `keywords` (5), `categories`, `homepage`,
`documentation`, `repository`, `license` and `readme`, so the only thing standing
between this workspace and `cargo publish` is the `publish = false` flag.

Two facts a maintainer should know before flipping it:

1. **`publish = false` is inherited from `[workspace.package]`.** It is a single
   deliberate pre-release choice, not seven separate ones, and it has **not** been
   changed here. Flipping that one line is the whole change.
2. **The workspace path dependencies carry a literal `version`.** Cargo has no
   inheritance mechanism for a *dependency's* version requirement — `version.workspace`
   applies to the package's own version only — so `cargo package` fails with "all
   dependencies must have a version requirement specified when packaging" without it.
   Those five literals are the one place the version can drift from the single source
   in §1, which is exactly what **REL1-10** exists to catch.

## Artifact attestation

**Not used before 1.0 (ADR-019).** The release workflow does not request
`id-token` and does not publish GitHub artifact attestations. Installers trust
`SHA256SUMS` verified locally (`install.sh` / `install.ps1`). Adding attestations later
is a separate decision: it needs a public repository and a conscious grant of
`id-token` write power on the publish job.

## Release workflow: current status

`.github/workflows/release.yml` is **written but never run.** It was authored without a
GitHub runner, so treat it as a reviewed proposal, not a proven pipeline.

What is verified locally:

- the YAML parses, and every action is pinned to a 40-hex commit SHA;
- the pinning check (`clean_facet6_supply_chain_shape` in
  `crates/edit/tests/sec_audit_poc.rs`) now walks **every** workflow, not just
  `ci.yml`, and fails if a pin is not a full SHA;
- the `dist/` layout it produces is the layout `install.sh` verifies (§3), and its
  assembly step refuses to publish a `SHA256SUMS` that does not cover every binary.

What is **not** verified and needs a real runner before the first tag:

- that the three-platform matrix builds — in particular the static musl leg, which is
  the only one whose "statically linked" assertion has ever been exercised anywhere;
- the cross-compiled `aarch64-apple-darwin` and `x86_64-pc-windows-msvc` legs, whose
  `.exe`/no-`.exe` staging path is untested;
- **`install.ps1` has never been executed.** REL2-09/10/11 (good tree, tampered digest,
  truncated sums) are written and wired into `scripts/tests/rel2_install_ps1_spec.sh`, and
  they print `SKIP` on any machine without PowerShell. The nine contract cases around them do
  run everywhere, but "the Windows installer refuses a bad tree" is currently a **claim about
  the script's text**, not an observation. See §4a;
- that `softprops/action-gh-release` publishes what this workflow passes it;
- the end-to-end claim behind SUP-07 (artefacts verifying against published checksums
  on a clean machine), which has no test at all.

The intended first exercise is a manual `workflow_dispatch` run: it builds every
platform and assembles the tree, but the `publish` job is gated on a tag ref and will
not create a release.
