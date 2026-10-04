#!/usr/bin/env bash
# Self-test for check-layering.sh (ROADMAP M0: deliberately breaking layering
# turns the check red).
#
# Copies the real workspace Cargo.toml tree into a temp directory, then:
#   1. core → lang          (forbidden upward edge)     → must fail
#   2. lang → edit          (forbidden upward edge)     → must fail
#   3. path outside workspace                           → must fail
#   4. unmodified clean copy                            → must pass
#
# Usage: test-check-layering.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
checker=$here/check-layering.sh

if [ ! -x "$checker" ] && [ -f "$checker" ]; then
	chmod +x "$checker"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=1; }

# Copy only the manifests the checker reads (root + crates/*/Cargo.toml).
seed_copy() {
	local dest=$1
	mkdir -p "$dest/crates"
	cp "$root/Cargo.toml" "$dest/Cargo.toml"
	local d
	for d in "$root"/crates/*; do
		[ -d "$d" ] || continue
		mkdir -p "$dest/crates/$(basename "$d")"
		cp "$d/Cargo.toml" "$dest/crates/$(basename "$d")/Cargo.toml"
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
	if printf '%s\n' "$out" | grep -qE '^layering check passed: [0-9]+ crates$'; then
		ok "$name"
	else
		bad "$name: unexpected success message: $out"
	fi
}

# --- 1) core depends on lang -------------------------------------------------
case1=$work/core-depends-lang
seed_copy "$case1"
cat > "$case1/crates/core/Cargo.toml" <<'EOF'
[package]
name = "opencrayast-core"
description = "opencrayast core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
sha2.workspace = true
data-encoding.workspace = true
opencrayast-lang.workspace = true

[target.'cfg(unix)'.dependencies]
rustix.workspace = true

[dev-dependencies]
tempfile.workspace = true
EOF
expect_fail \
	"core → lang is rejected" \
	"$case1" \
	"VIOLATION: opencrayast-core -> opencrayast-lang"

# --- 2) lang depends on edit -------------------------------------------------
case2=$work/lang-depends-edit
seed_copy "$case2"
cat > "$case2/crates/lang/Cargo.toml" <<'EOF'
[package]
name = "opencrayast-lang"
description = "opencrayast lang"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
opencrayast-core.workspace = true
opencrayast-edit.workspace = true
EOF
expect_fail \
	"lang → edit is rejected" \
	"$case2" \
	"VIOLATION: opencrayast-lang -> opencrayast-edit"

# --- 3) path dependency outside the workspace --------------------------------
case3=$work/path-outside
seed_copy "$case3"
cat > "$case3/crates/core/Cargo.toml" <<'EOF'
[package]
name = "opencrayast-core"
description = "opencrayast core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
sha2.workspace = true
data-encoding.workspace = true
external-thing = { path = "/opt/external-thing" }

[target.'cfg(unix)'.dependencies]
rustix.workspace = true

[dev-dependencies]
tempfile.workspace = true
EOF
expect_fail \
	"path outside workspace is rejected" \
	"$case3" \
	"VIOLATION: opencrayast-core -> external-thing (path dependency outside the workspace)"

# --- 4) clean copy passes ----------------------------------------------------
clean=$work/clean
seed_copy "$clean"
expect_pass "clean workspace copy passes" "$clean"

if [ "$fail" -ne 0 ]; then
	echo "test-check-layering.sh: FAILED" >&2
	exit 1
fi
echo "test-check-layering.sh: all cases passed"
