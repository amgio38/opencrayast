#!/usr/bin/env bash
# Self-test for check-grammar-provenance.sh.
#
# A gate that has never been observed to go red is not known to be a gate. This
# plants, on copies of the tree, one instance of each drift the check exists to
# catch, and requires a non-zero exit AND the right diagnostic each time. The last
# case requires an unmodified copy to pass, so the check cannot be "always red"
# either. Nothing here writes to the repository; every case runs against a
# throwaway copy under mktemp -d that is removed on exit.
#
# Drifts planted, one per case:
#   1. a grammar crate in Cargo.lock with no row in the provenance table (A)
#   2. a table row whose version no longer matches Cargo.lock        (B)
#   3. a table row for a grammar that left Cargo.lock               (B, other dir)
#   4. a grammar package resolved from a git source with no commit fragment (C)
#   5. a git source whose commit fragment names no upstream project  (C)
#   6. a grammar dependency pinned with branch = in a manifest      (C)
#   7. a grammar dependency pinned with a short rev =               (C)
#   8. the whole '## Grammar crates' section deleted                (A)
#   9. unmodified                                               (must pass)
#
# Each planted drift is a shape a real change could take: a grammar added to the
# workspace, a Dependabot bump that landed the manifest but not the table, a
# grammar dropped from the workspace, and someone swapping a crates.io grammar
# for a fork on a branch.
#
# Usage: bash scripts/test-check-grammar-provenance.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
checker=$here/check-grammar-provenance.sh

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() {
	printf 'FAIL: %s\n' "$1" >&2
	printf '%s\n' "$2" >&2 || true
	fail=1
}

# Copy only what the checker reads: the lockfile, the provenance document, the
# manifests, and this script. A case must not be affected by anything else.
seed_copy() {
	local dest=$1 d
	mkdir -p "$dest/scripts" "$dest/crates/lang"
	cp "$root/Cargo.lock" "$dest/Cargo.lock"
	cp "$root/THIRD-PARTY-LICENSES.md" "$dest/THIRD-PARTY-LICENSES.md"
	cp "$root/Cargo.toml" "$dest/Cargo.toml"
	cp "$root/crates/lang/Cargo.toml" "$dest/crates/lang/Cargo.toml"
	cp "$here/check-grammar-provenance.sh" "$dest/scripts/check-grammar-provenance.sh"
}

expect_fail() {
	local name=$1 dest=$2 needle=$3
	local out rc=0
	out=$(cd "$dest" && bash scripts/check-grammar-provenance.sh 2>&1) || rc=$?
	if [ "$rc" -eq 0 ]; then
		bad "$name: expected a non-zero exit, got 0; output: $out" ""
		return
	fi
	if printf '%s\n' "$out" | grep -qF "$needle"; then
		ok "$name (exit $rc)"
	else
		bad "$name: diagnostic did not mention '$needle'; got: $out" ""
	fi
}

expect_pass() {
	local name=$1 dest=$2
	local out rc=0
	out=$(cd "$dest" && bash scripts/check-grammar-provenance.sh 2>&1) || rc=$?
	if [ "$rc" -ne 0 ]; then
		bad "$name: expected exit 0, got $rc; output: $out" ""
		return
	fi
	if ! printf '%s\n' "$out" | grep -qE '^grammar provenance check passed: '; then
		bad "$name: unexpected success message: $out" ""
		return
	fi
	ok "$name"
}

echo "check-grammar-provenance.sh"

# --- 1) a grammar in the lockfile with no row in the table ----------------------
case1=$work/unlisted-grammar
seed_copy "$case1"
python3 - "$case1/Cargo.lock" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
# A realistic new grammar: registry-sourced, checksummed, and the only thing
# missing is the provenance row. Nothing else about the lockfile changes.
s += '''
[[package]]
name = "tree-sitter-selftest-planted"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0000000000000000000000000000000000000000000000000000000000000000"
dependencies = [
 "tree-sitter-language",
]
'''
p.write_text(s, encoding="utf-8")
PY
expect_fail "a grammar in Cargo.lock with no provenance row is caught" \
	"$case1" "tree-sitter-selftest-planted 0.1.0 is in Cargo.lock but has no row"

# --- 2) a table version that no longer matches the lockfile ---------------------
# This is what a Dependabot bump looks like when only the manifest moved. The
# mutation is anchored on the GRAMMAR table's row (the one with a Source column),
# not the crate inventory's row for the same crate: both tables list the version,
# and only the grammar table is what this check reconciles.
case2=$work/version-drift
seed_copy "$case2"
python3 - "$case2/THIRD-PARTY-LICENSES.md" <<'PY'
import re
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
row = [l for l in s.splitlines()
       if l.startswith("| `tree-sitter-rust` |") and " crates.io " in l][0]
assert "0.24.2" in row, f"planted case assumes the current grammar row: {row}"
p.write_text(s.replace(row, row.replace("0.24.2", "0.24.9", 1), 1), encoding="utf-8")
PY
expect_fail "a listed grammar whose version drifts from Cargo.lock is caught" \
	"$case2" "tree-sitter-rust version drifted: THIRD-PARTY-LICENSES.md says 0.24.9"

# --- 3) a table row for a grammar that is no longer in the lockfile -------------
case3=$work/dropped-grammar
seed_copy "$case3"
python3 - "$case3/THIRD-PARTY-LICENSES.md" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
row = [l for l in s.splitlines()
       if l.startswith("| `tree-sitter-go` |") and " crates.io " in l][0]
p.write_text(s.replace(row, row.replace("tree-sitter-go", "tree-sitter-dropped-grammar", 1), 1),
             encoding="utf-8")
PY
expect_fail "a listed grammar absent from Cargo.lock is caught" \
	"$case3" "tree-sitter-dropped-grammar is listed in the '## Grammar crates' section"

# --- 4) a grammar resolved from git with no commit fragment ---------------------
case4=$work/git-no-commit
seed_copy "$case4"
python3 - "$case4/Cargo.lock" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
old = '''name = "tree-sitter-go"
version = "0.25.0"
source = "registry+https://github.com/rust-lang/crates.io-index"'''
assert old in s
new = '''name = "tree-sitter-go"
version = "0.25.0"
source = "git+https://github.com/tree-sitter/tree-sitter-go"'''
p.write_text(s.replace(old, new, 1), encoding="utf-8")
PY
expect_fail "a git-sourced grammar with no commit fragment is caught" \
	"$case4" "comes from git source git+https://github.com/tree-sitter/tree-sitter-go with no commit fragment"

# --- 5) a git source whose commit names no upstream project ---------------------
# The dangerous shape: pinned to a commit, but a commit of WHAT is unrecorded, so
# the provenance table cannot say which upstream the C came from.
case5=$work/git-no-project
seed_copy "$case5"
python3 - "$case5/Cargo.lock" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
old = '''name = "tree-sitter-go"
version = "0.25.0"
source = "registry+https://github.com/rust-lang/crates.io-index"'''
new = '''name = "tree-sitter-go"
version = "0.25.0"
source = "git+https://example.invalid/parser-fork?rev=1#deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"'''
p.write_text(s.replace(old, new, 1), encoding="utf-8")
PY
expect_fail "a git source with no upstream project in its commit fragment is caught" \
	"$case5" "does not start its commit fragment with 'tree-sitter'"

# --- 6) a grammar dependency pinned with branch = -------------------------------
# cargo resolves `branch =` to a moving ref and writes the resolved commit into
# Cargo.lock, so the lockfile alone looks perfectly pinned. Only the manifest says
# otherwise, which is why the checker reads both.
case6=$work/branch-float
seed_copy "$case6"
python3 - "$case6/Cargo.toml" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
old = 'tree-sitter-go = "0.25"'
assert old in s, "planted case assumes the current workspace grammar requirement"
s = s.replace(old,
              'tree-sitter-go = { git = "https://github.com/tree-sitter/tree-sitter-go", branch = "master" }',
              1)
p.write_text(s, encoding="utf-8")
PY
expect_fail "a grammar dependency pinned with branch = is caught" \
	"$case6" "uses branch = / tag ="

# --- 7) a grammar dependency pinned with a short rev = --------------------------
case7=$work/short-rev
seed_copy "$case7"
python3 - "$case7/Cargo.toml" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
old = 'tree-sitter-go = "0.25"'
s = s.replace(old,
              'tree-sitter-go = { git = "https://github.com/tree-sitter/tree-sitter-go", rev = "1547678" }',
              1)
p.write_text(s, encoding="utf-8")
PY
expect_fail "a grammar dependency pinned with a non-commit rev = is caught" \
	"$case7" "is not a full 40-character commit id"

# --- 8) the whole grammar section deleted ---------------------------------------
case8=$work/no-section
seed_copy "$case8"
python3 - "$case8/THIRD-PARTY-LICENSES.md" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1])
s = p.read_text(encoding="utf-8")
start = s.index("## Grammar crates")
end = s.index("## Test-only / oracle dependencies")
p.write_text(s[:start] + s[end:], encoding="utf-8")
PY
expect_fail "a document with no grammar section at all is caught" \
	"$case8" "has no '## Grammar crates' section"

# --- 9) an unmodified copy passes -----------------------------------------------
clean=$work/clean
seed_copy "$clean"
expect_pass "an unmodified copy passes" "$clean"

if [ "$fail" -ne 0 ]; then
	echo "test-check-grammar-provenance.sh: FAILED" >&2
	exit 1
fi
echo "test-check-grammar-provenance.sh: all cases passed"
