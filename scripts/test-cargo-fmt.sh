#!/usr/bin/env bash
# Self-test for the `cargo fmt --all -- --check` gate (ROADMAP M0: deliberately
# breaking formatting turns the check red).
#
# The layering and matrix checks each ship a self-test that proves the gate can
# actually fail; formatting needs the same proof, otherwise "Format" being green
# only shows that nobody ever wrote badly formatted code, not that rustfmt is
# wired to the gate at all.
#
# Every case runs the REAL `cargo fmt --all -- --check` against a COPY of the
# tree (manifests + sources), so a failing case cannot leave the checkout dirty
# or broken. Nothing here writes to the repository: the pristine copy is the
# only state the real tree is ever in, and the temp dir is removed on exit even
# on interrupt.
#
#   1. clean copy                                  → must pass
#   2. badly spaced fn in a src/lib.rs              → must fail
#   3. a line longer than rustfmt.toml's max_width  → must fail
#   4. badly spaced fn in a tests/ spec             → must fail (--all covers
#                                                       test targets too)
#   5. `cargo fmt --all` repairs the copy           → check goes green again
#   6. pruning the probe from the copy (restore)    → check goes green again
#
# Usage: bash scripts/test-cargo-fmt.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)

if ! command -v cargo >/dev/null 2>&1; then
	echo "test-cargo-fmt.sh: cargo not found; cannot run the formatting gate" >&2
	exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() {
	printf 'FAIL: %s\n' "$1" >&2
	fail=1
}

# Copy only what rustfmt reads: the workspace manifest, the formatting config,
# and each crate's manifest plus its Rust source directories. target/ and .git
# are never copied, so this stays cheap.
seed_copy() {
	local dest=$1 d n
	mkdir -p "$dest/crates"
	cp "$root/Cargo.toml" "$dest/Cargo.toml"
	cp "$root/rustfmt.toml" "$dest/rustfmt.toml"
	for d in "$root"/crates/*; do
		[ -d "$d" ] || continue
		n=$(basename "$d")
		mkdir -p "$dest/crates/$n"
		cp "$d/Cargo.toml" "$dest/crates/$n/Cargo.toml"
		local s
		for s in src tests benches examples; do
			[ -d "$d/$s" ] && cp -R "$d/$s" "$dest/crates/$n/$s"
		done
	done
	return 0
}

# Run the gate in a fixture; sets $out and $rc.
run_gate() {
	local dest=$1
	set +e
	out=$(cd "$dest" && cargo fmt --all -- --check 2>&1)
	rc=$?
	set -e
}

expect_fail() {
	local name=$1 dest=$2 needle=$3
	run_gate "$dest"
	if [ "$rc" -eq 0 ]; then
		bad "$name: expected a non-zero exit, got 0; output: $out"
		return
	fi
	if printf '%s\n' "$out" | grep -qF "$needle"; then
		ok "$name"
	else
		bad "$name: output missing '$needle'; got: $out"
	fi
}

expect_pass() {
	local name=$1 dest=$2
	run_gate "$dest"
	if [ "$rc" -ne 0 ]; then
		bad "$name: expected exit 0, got $rc; output: $out"
		return
	fi
	if [ -n "$out" ]; then
		bad "$name: expected silence on success, got: $out"
		return
	fi
	ok "$name"
}

# A deliberately mis-formatted function: no spaces around `:`, a cramped
# signature, and the body on the same line as the brace.
BAD_FN='pub fn reverse_verification_probe(  a:i32,b:i32 )->i32{ a+b }'

# --- 1) sanity: the untouched copy is already green -------------------------
# This doubles as a check that the tree at HEAD really is rustfmt-clean; if the
# pristine copy is red the self-test is meaningless, so say so rather than
# quietly reporting every later case as green.
clean=$work/clean
seed_copy "$clean"
run_gate "$clean"
if [ "$rc" -ne 0 ]; then
	bad "clean copy is not rustfmt-clean (run 'cargo fmt --all' first): $out"
	printf 'test-cargo-fmt.sh: FAILED\n' >&2
	exit 1
fi
ok "clean copy passes"

# --- 2) a mis-formatted fn in a product source file -------------------------
case1=$work/bad-src
seed_copy "$case1"
printf '\n%s\n' "$BAD_FN" >> "$case1/crates/core/src/lib.rs"
expect_fail \
	"mis-formatted fn in src/lib.rs exits non-zero" \
	"$case1" \
	"Diff in"

# --- 3) a line past rustfmt.toml's max_width --------------------------------
# max_width = 100 (rustfmt.toml). Well past it, so the gate must rewrap.
case2=$work/bad-width
seed_copy "$case2"
pad=''
i=0
while [ "$i" -lt 80 ]; do
	pad="${pad}x"
	i=$((i + 1))
done
{
	printf '\npub fn reverse_verification_probe_width() {\n'
	printf '    let _probe_aaaa = "%s";\n' "$pad"
	printf '    let _probe_aaaa = "%s";\n' "$pad"
	printf '}\n'
} >> "$case2/crates/core/src/lib.rs"
expect_fail \
	"a line over max_width=100 exits non-zero" \
	"$case2" \
	"Diff in"

# --- 4) a mis-formatted test target ------------------------------------------
# `cargo fmt --all` must reach tests/ as well; a violation only in a spec is
# still a violation.
case3=$work/bad-test
seed_copy "$case3"
spec=$case3/crates/core/tests/reverse_verification_probe_spec.rs
printf '%s\n' "$BAD_FN" > "$spec"
expect_fail \
	"mis-formatted test target exits non-zero" \
	"$case3" \
	"reverse_verification_probe_spec.rs"

# --- 5) the repair path: cargo fmt --all turns the red gate green -----------
# A gate that can only ever say "red" is not a usable gate. This asserts the
# documented fix actually restores green, on the already-violating fixture 2.
set +e
repair_out=$(cd "$case1" && cargo fmt --all 2>&1)
repair_rc=$?
set -e
if [ "$repair_rc" -ne 0 ]; then
	bad "'cargo fmt --all' failed on the violating copy: exit $repair_rc: $repair_out"
else
	ok "'cargo fmt --all' repairs the violating copy"
	expect_pass "the gate is green again after cargo fmt --all" "$case1"
fi

# --- 6) the restore path: removing the probe returns to green --------------
# Proves the fixture is back to the committed state, i.e. the self-test cannot
# leave a stale violation behind for whatever runs next. Re-seed from the real
# tree, the same way a `git checkout -- .` would in the checkout itself.
restored=$work/restored
seed_copy "$restored"
expect_pass "a freshly seeded copy is green again (no residue)" "$restored"

if [ "$fail" -ne 0 ]; then
	echo "test-cargo-fmt.sh: FAILED" >&2
	exit 1
fi
echo "test-cargo-fmt.sh: all cases passed"
