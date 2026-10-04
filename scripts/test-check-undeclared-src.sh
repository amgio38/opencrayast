#!/usr/bin/env bash
# Self-test for check-undeclared-src.sh: a .rs that no `mod` reaches must go red.
#
# Copies crates/*/src plus manifests into a temp tree, then:
#   1. plants an undeclared sibling of lib.rs              → must fail, naming the file
#   2. plants an undeclared file under a nested module dir → must fail, naming the file
#   3. unmodified copy                                     → must pass
#
# Usage: test-check-undeclared-src.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
checker=$here/check-undeclared-src.sh

if [ ! -x "$checker" ] && [ -f "$checker" ]; then
	chmod +x "$checker"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=1; }

seed_copy() {
	local dest=$1
	mkdir -p "$dest/crates"
	cp "$root/Cargo.toml" "$dest/Cargo.toml"
	local d
	for d in "$root"/crates/*; do
		[ -d "$d" ] || continue
		name=$(basename "$d")
		mkdir -p "$dest/crates/$name"
		cp "$d/Cargo.toml" "$dest/crates/$name/Cargo.toml"
		if [ -d "$d/src" ]; then
			cp -R "$d/src" "$dest/crates/$name/src"
		fi
	done
}

expect_fail() {
	local name=$1 dest=$2 needle=$3
	local out rc=0
	out=$("$checker" --root "$dest" 2>&1) || rc=$?
	if [ "$rc" -eq 0 ]; then
		bad "$name: expected exit 1, got 0; output: $out"
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
	local out rc=0
	out=$("$checker" --root "$dest" 2>&1) || rc=$?
	if [ "$rc" -ne 0 ]; then
		bad "$name: expected exit 0, got $rc; output: $out"
		return
	fi
	if printf '%s\n' "$out" | grep -qE '^undeclared-src check passed: [0-9]+ crates, [0-9]+ src files$'; then
		ok "$name"
	else
		bad "$name: unexpected success message: $out"
	fi
}

# --- 1) undeclared sibling of a crate root -----------------------------------
case1=$work/orphan-root
seed_copy "$case1"
printf '// not in any mod\n' > "$case1/crates/cli/src/orphan_dead.rs"
expect_fail "undeclared sibling of lib.rs" "$case1" "crates/cli/src/orphan_dead.rs"

# --- 2) undeclared nested file -----------------------------------------------
case2=$work/orphan-nested
seed_copy "$case2"
printf '// not in outline/mod.rs\n' > "$case2/crates/query/src/outline/orphan_dead.rs"
expect_fail "undeclared nested module file" "$case2" "crates/query/src/outline/orphan_dead.rs"

# --- 3) clean copy -----------------------------------------------------------
case3=$work/clean
seed_copy "$case3"
expect_pass "unmodified tree" "$case3"

if [ "$fail" -ne 0 ]; then
	echo "test-check-undeclared-src.sh: FAILED" >&2
	exit 1
fi
echo "test-check-undeclared-src.sh: all cases passed"
