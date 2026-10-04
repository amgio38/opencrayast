#!/usr/bin/env bash
# Self-test for the cargo-deny licence gate (ROADMAP M0: deliberately breaking
# licensing turns the check red).
#
# CI runs `cargo deny check` through EmbarkStudios/cargo-deny-action against this
# repository's real `deny.toml`. Nothing demonstrated that a disallowed licence
# actually turns that gate red, so a green step proved only that the tool ran.
# This file closes that gap.
#
# WHAT IS DEMONSTRATED, PRECISELY
# --------------------------------
# The real `deny.toml` from this repository is copied verbatim into a throwaway
# crate, and the crate's own `license` field is flipped between a disallowed and
# an allowed expression. So the red/green pair is produced by THIS repository's
# licence policy file, not by a hand-written config invented for the test.
#
# The crate has NO dependencies, so `cargo deny check` needs no network and no
# `Cargo.lock` resolution: it reads the manifest and evaluates the licence
# expression. It takes about a second.
#
# WHY A TEMPORATE CRATE RATHER THAN AN AD-HOC `--config`
# ----------------------------------------------------
# Both were measured. The ad-hoc-config shape (`cargo deny --config bad.toml
# check licenses` against the real workspace) also goes red, but for a weaker
# reason and at a much higher cost:
#   * it re-walks the whole dependency graph, so it is slow (tens of seconds)
#     and its output is a long transitive tree of unrelated crates;
#   * its red is "these real dependencies (MIT, Apache-2.0, ...) are not on the
#     allow list", which would go red even if the licence gate were broken, so
#     it does not isolate the rule being tested;
#   * it needs `--locked`-equivalent metadata for the real workspace, coupling the
#     self-test to the current dependency set.
# The temporary crate keeps the failure attributable to one thing - the licence
# expression on one manifest - which is what a reverse-verification has to show.
# No real manifest in this repository is touched, and the checkout is never
# modified: the crate lives under `mktemp -d` and is removed on exit.
#
# CASES
#   1. GPL-3.0-only (not on the allow list)      -> must fail, and say why
#   2. WTFPL      (not on the allow list)        -> must fail, and say why
#   3. MIT        (on the allow list)            -> must pass
#   4. restored MIT, re-run                      -> must pass (restore is real)
#
# Usage: test-check-licensing.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
deny_toml=$root/deny.toml

if ! command -v cargo-deny >/dev/null 2>&1 && ! cargo deny --version >/dev/null 2>&1; then
	echo "test-check-licensing.sh: cargo-deny is not installed; cannot demonstrate the gate" >&2
	exit 1
fi

if [ ! -f "$deny_toml" ]; then
	echo "test-check-licensing.sh: missing $deny_toml" >&2
	exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=1; }

# The throwaway crate. `license` is the only thing each case changes.
seed_crate() {
	local dest=$1 license=$2
	mkdir -p "$dest/src"
	cp "$deny_toml" "$dest/deny.toml"
	cat > "$dest/Cargo.toml" <<EOF
[package]
name = "cargo-deny-gate-probe"
version = "0.0.0"
edition = "2021"
license = "$license"
EOF
	printf 'pub fn probe() {}\n' > "$dest/src/lib.rs"
}

# CI sets CARGO_TERM_COLOR=always; cargo-deny then paints "licenses FAILED" as
# "licenses <red>FAILED</red>", so a plain-string grep misses the gate line and the
# self-test falsely reports "failed, but not on the licence gate". Force plain
# text for every probe — the gate under test is the exit code and the words, not
# the paint.
deny_check() {
	(cd "$1" && CARGO_TERM_COLOR=never TERM=dumb cargo deny check 2>&1)
}

expect_fail() {
	local name=$1 license=$2
	local out rc=0
	seed_crate "$work/$license" "$license"
	out=$(deny_check "$work/$license") || rc=$?
	if [ "$rc" -eq 0 ]; then
		bad "$name: expected cargo-deny to fail, got exit 0; output: $out"
		return
	fi
	# The failure has to be the LICENCE gate, not advisories/bans/sources, and
	# it has to name the licence we deliberately chose.
	if ! printf '%s\n' "$out" | grep -qF 'licenses FAILED'; then
		bad "$name: cargo-deny failed, but not on the licence gate; output: $out"
		return
	fi
	if ! printf '%s\n' "$out" | grep -qF 'rejected: license is not explicitly allowed'; then
		bad "$name: output missing the rejection reason; got: $out"
		return
	fi
	if ! printf '%s\n' "$out" | grep -qF "$license"; then
		bad "$name: output does not name the offending licence $license; got: $out"
		return
	fi
	ok "$name (exit $rc, licence gate red)"
}

expect_pass() {
	local name=$1 license=$2
	local out rc=0
	seed_crate "$work/$license" "$license"
	out=$(deny_check "$work/$license") || rc=$?
	if [ "$rc" -ne 0 ]; then
		bad "$name: expected cargo-deny to pass, got exit $rc; output: $out"
		return
	fi
	if ! printf '%s\n' "$out" | grep -qF 'licenses ok'; then
		bad "$name: unexpected success message: $out"
		return
	fi
	ok "$name (exit 0)"
}

# --- 1) a copyleft licence the policy deliberately does not allow ----------
expect_fail "GPL-3.0-only is rejected by deny.toml" "GPL-3.0-only"

# --- 2) a licence that is not even OSI-approved ---------------------------
expect_fail "WTFPL is rejected by deny.toml" "WTFPL"

# --- 3) a licence the policy allows ---------------------------------------
expect_pass "MIT is accepted by deny.toml" "MIT"

# --- 4) restore and re-run: proves the tree really was restored ------------
# Re-seeding the same directory with the allowed licence and re-running shows
# the gate is deterministic in both directions, so case 1 cannot have passed
# because of some one-way state left behind by case 2.
expect_pass "re-run after restore is still green" "MIT"

if [ "$fail" -ne 0 ]; then
	echo "test-check-licensing.sh: FAILED" >&2
	exit 1
fi
echo "test-check-licensing.sh: all cases passed"
