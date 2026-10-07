#!/usr/bin/env bash
# Pre-push gate: runs, locally, what CI runs on Linux, in the order CI runs it.
#
# Why this exists: a version bump once went out with the MCP golden transcripts
# still holding the old version string, and CI went red on three platforms. The
# author had run "the relevant checks", not the suite. The rule this script
# encodes is simple: nothing is pushed until the same commands CI will run have
# passed on the exact tree being pushed.
#
#   bash scripts/preflight.sh            # everything, including the coverage gate
#   bash scripts/preflight.sh --quick    # skips the coverage gate (slowest step)
#
# Machine caps (docs/CONTRIBUTING.md "why the job caps"): the defaults below keep a
# shared host usable; override with CARGO_BUILD_JOBS / RUST_TEST_THREADS.
#
# What it does NOT cover: the macOS and Windows legs of CI. Those compile and run
# the same test suite on other platforms; a failure there that does not show up
# here is a platform difference, and the only place to see it is CI itself.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

QUICK=0
case "${1:-}" in
	--quick) QUICK=1 ;;
	'') ;;
	*) echo "usage: preflight.sh [--quick]" >&2; exit 2 ;;
esac

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"

steps=0
step() {
	steps=$((steps + 1))
	printf '\n== [%s] %s\n' "$steps" "$1"
}

# A dirty tree means "what passed" and "what gets pushed" can differ.
step "working tree is committed (what passes is what gets pushed)"
dirty="$(git -c core.fileMode=false status --porcelain --untracked-files=all | grep -v '^??' || true)"
untracked="$(git -c core.fileMode=false status --porcelain --untracked-files=all | grep '^??' || true)"
if [ -n "$dirty" ] || [ -n "$untracked" ]; then
	echo "preflight: the tree has uncommitted or untracked files; commit or remove them first:" >&2
	printf '%s\n%s\n' "$dirty" "$untracked" | sed '/^$/d' | head -20 >&2
	exit 1
fi
echo "  clean"

step "version is consistent: Cargo.lock matches Cargo.toml, tag-style version has no stale copy"
cargo metadata --locked --offline --format-version 1 >/dev/null
version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p}' Cargo.toml | head -1)"
[ -n "$version" ] || { echo "preflight: no [workspace.package] version found" >&2; exit 1; }
echo "  workspace version $version"

step "cargo fmt --all -- --check"
cargo fmt --all -- --check

step "cargo clippy --workspace --all-targets --locked -- -D warnings"
cargo clippy --workspace --all-targets --locked -- -D warnings

# The full suite, not a selection. The golden transcripts embed the server version, so
# a version bump that forgot `cargo test -p opencrayast-mcp --test mcp8_golden -- --ignored record`
# fails here instead of in CI.
step "cargo test --workspace --locked"
cargo test --workspace --locked

step "documentation check"
bash scripts/check-docs.sh

step "gate self-tests (each proves its gate can go red)"
for t in test-check-layering test-check-undeclared-src test-check-grammar-provenance test-install-posix; do
	echo "  -- $t"
	bash "scripts/$t.sh" >/dev/null
done

if [ "$QUICK" -eq 0 ]; then
	step "coverage gate"
	bash scripts/check-coverage.sh
else
	echo
	echo "(--quick: coverage gate skipped; CI will run it)"
fi

printf '\npreflight: all %s steps passed for %s at %s\n' "$steps" "$version" "$(git rev-parse --short HEAD)"
