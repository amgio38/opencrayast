# Pre-release checklist

Walk this list before tagging a release. Paste command output (or CI run URLs)
into the release PR so the check is evidence, not intention.

There is **no** `make ci` target in this repository. The gates below are the
steps in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), plus the
extra reconcile and documentation builds maintainers run locally.

Suggested environment on a shared host:

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
```

Do not run more than one `cargo` process against the same `target/` at a time.

## CI gates (mirror of the workflow)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked --no-fail-fast`
- [ ] `sh scripts/check-docs.sh`
- [ ] `sh scripts/check-matrix.sh`
- [ ] `bash scripts/check-layering.sh`
- [ ] `bash scripts/test-check-layering.sh`
- [ ] `bash scripts/check-undeclared-src.sh`
- [ ] `bash scripts/test-check-undeclared-src.sh`
- [ ] `bash scripts/check-coverage.sh` (line-coverage floor; needs `llvm-tools-preview`)
- [ ] `bash scripts/test-check-coverage.sh` (proves the coverage gate can go red)
- [ ] `bash scripts/verify-release.sh` — the runnable checklist in
      [`RELEASE-CHECKLIST.md`](RELEASE-CHECKLIST.md), with each item recorded
- [ ] `bash scripts/verify-release.sh --require-manual` — same, plus every manual
      sign-off; exits non-zero while any is open
- [ ] `cargo deny check`
- [ ] GitHub Actions green on `ubuntu-latest`, `macos-latest`, and
      `windows-latest` for the merge commit

## Workspace tests (explicit)

Even if CI already ran them, re-run once on the release machine so the log is
attached to the release record:

```sh
export CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4
cargo test --workspace --locked --no-fail-fast
```

- [ ] Output shows every suite `ok` and no `FAILED`

## Clippy with warnings denied

```sh
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- [ ] Exit code 0

## Documentation build

```sh
cargo doc --workspace --no-deps --document-private-items
```

- [ ] Exit code 0 (warnings from dependencies are avoided via `--no-deps`)

Optional prose check (also a CI gate on Linux):

```sh
sh scripts/check-docs.sh
```

- [ ] Prints `documentation check passed`

## Licence and Cargo metadata

`Cargo.toml` `[workspace.package] license` must agree with the root licence
file. Example check (MIT today):

```sh
python3 - <<'PY'
from pathlib import Path
import re
cargo = Path("Cargo.toml").read_text(encoding="utf-8")
field = re.search(r'(?m)^license\s*=\s*"([^"]+)"', cargo).group(1)
first = Path("LICENSE").read_text(encoding="utf-8").splitlines()[0]
assert field == "MIT" and first.startswith("MIT"), (field, first)
print(f"ok: Cargo.toml license={field!r}; LICENSE starts with {first!r}")
PY
```

- [ ] Prints `ok:` and exits 0
- [ ] `Cargo.toml` `version` matches the intended tag without the leading `v`
- [ ] `CHANGELOG.md` has a dated section for this version (see
      [`MAINTENANCE.md`](MAINTENANCE.md#changelog-whats-new-sync))

## Documents versus implementation

- [ ] Every user-visible behaviour change in the tag is named in `CHANGELOG.md`
- [ ] Tool / security / test catalogue docs that the change touches were updated
      in the same PR (`docs/TOOLS.md`, `docs/SECURITY-MODEL.md`,
      `docs/TESTING.md`, ADRs as needed)
- [ ] `scripts/check-matrix.sh` is green (threat rows and named tests still line up)
- [ ] No document describes a flag, subcommand or script path that does not exist
      (search the docs you edited; `scripts/check-docs.sh` covers `scripts/…`
      paths and Markdown links)

## Artefacts (when shipping binaries)

- [ ] Linux static build:
      `cargo build --release --workspace --locked --target x86_64-unknown-linux-musl`
      (and aarch64 musl when publishing that arch)
- [ ] `file` on `opencrayast` / `opencrayast-mcp` reports `static-pie linked`
- [ ] Checksums recorded next to the artefacts
- [ ] Supported-version sentence in [`SECURITY.md`](../SECURITY.md) still true

## Stop conditions

Do **not** tag if any box above is unchecked, if CI is red on any OS, or if the
licence reconcile fails. Fix forward; do not move an existing tag.
