#!/usr/bin/env bash
# Self-test for check-coverage.sh: proves the coverage gate can go red.
#
# A gate nobody has seen fail is indistinguishable from a gate that cannot fail.
# This script feeds the REAL `scripts/check-coverage.sh` a set of synthetic
# llvm-cov JSON reports and asserts on exit status AND on the reason printed,
# because "it failed" and "it failed because coverage was below the floor" are
# different claims.
#
# Cases:
#   1. all crates above the floor                        → pass (exit 0)
#   2. one crate below its floor                         → fail, names the crate
#   3. a crate exactly AT the floor                     → pass (the floor is a floor)
#   4. an empty report (no first-party files)            → fail, never 100%
#   5. a report that is not llvm-cov JSON                → fail, not silently green
#   6. only test/ files present (everything ignored)     → fail, never 100%
#
# Same reverse-verification convention as scripts/test-check-layering.sh and
# scripts/tests/check_matrix_spec.sh. No cargo, no network: this is about the
# gate's arithmetic, not about the measurement.
#
# Usage: scripts/test-check-coverage.sh
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/.." && pwd)
checker=$here/check-coverage.sh

if [ ! -x "$checker" ] && [ -f "$checker" ]; then
	chmod +x "$checker"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail=0
ok() { printf 'ok: %s\n' "$1"; }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=1; }

# Build a synthetic llvm-cov report in the shape cargo-llvm-cov actually emits
# (data[].files[].summary.lines.{count,covered}) - verified against a real
# collected report, not from memory. `write_report <path> <crate>=<covered>/<total>...`
# where a crate of "none" contributes no files at all.
write_report() {
	local path=$1
	shift
	python3 - "$path" "$@" <<'PY'
import json
import sys

path, specs = sys.argv[1], sys.argv[2:]
files = []
for spec in specs:
    crate, counts = spec.split("=")
    if counts == "none":
        continue
    covered, total = (int(n) for n in counts.split("/"))
    files.append(
        {
            "branches": {"count": 0, "covered": 0, "notcovered": 0, "percent": 0.0},
            "mcdc_records": [],
            "expansions": [],
            "filename": f"/build/wt/crates/{crate}/src/lib.rs",
            "segments": [[0, 1, 1, 1, 0]],
            "summary": {
                "branches": {"count": 0, "covered": 0, "notcovered": 0, "percent": 0.0},
                "mcdc": {"count": 0, "covered": 0, "notcovered": 0, "percent": 0.0},
                "functions": {"count": 1, "covered": 1, "percent": 100.0},
                "instantiations": {"count": 1, "covered": 1, "percent": 100.0},
                "lines": {"count": total, "covered": covered, "percent": 100.0 * covered / total},
                "regions": {"count": 1, "covered": 1, "notcovered": 0, "percent": 100.0},
            },
        }
    )
json.dump(
    {"data": [{"files": files, "functions": {}, "totals": {}}],
     "type": "llvm.coverage.json.export", "version": "2.0.1"},
    open(path, "w", encoding="utf-8"),
)
PY
}

# expect <name> <expected: pass|fail> <needle-or-> <report-path>
expect() {
	local name=$1 want=$2 needle=$3 report=$4
	local out rc=0
	out=$("$checker" --report "$report" 2>&1) || rc=$?
	if [ "$want" = pass ]; then
		if [ "$rc" -ne 0 ]; then
			bad "$name: expected exit 0, got $rc; output: $out"
			return
		fi
		ok "$name"
		return
	fi
	if [ "$rc" -eq 0 ]; then
		bad "$name: expected a non-zero exit, got 0; output: $out"
		return
	fi
	if [ "$needle" != "-" ]; then
		if printf '%s\n' "$out" | grep -qF "$needle"; then
			ok "$name"
		else
			bad "$name: exit was non-zero but output lacks '$needle'; got: $out"
		fi
		return
	fi
	ok "$name"
}

# 1. Everything comfortably above the floor.
write_report "$work/green.json" core=900/1000 edit=850/1000 query=880/1000
expect "all crates above the floor pass" pass - "$work/green.json"

# 2. `query` deliberately below its floor. This is the mutation: it is the same
#    report as case 1 with one number changed.
write_report "$work/red.json" core=900/1000 edit=850/1000 query=100/1000
expect "a crate below the floor turns the gate red" fail "crate query" "$work/red.json"

# 3. Exactly at the floor is still a pass: the floor is a floor, not "strictly
#    above". Guards against an off-by-one that would make the gate flappy.
write_report "$work/edge.json" core=800/1000 edit=800/1000 query=800/1000
expect "exactly at the floor passes" pass - "$work/edge.json"

# 4. An empty report is the dangerous one. If the run produced nothing, reading
#    it as 100% would turn a broken toolchain into a green build.
write_report "$work/empty.json" core=none edit=none query=none
expect "an empty report is not 100% coverage" fail "no first-party files" "$work/empty.json"

# 5. Garbage input must not be silently accepted.
printf 'this is not a coverage report\n' >"$work/junk.json"
expect "a non-report file turns the gate red" fail "not a cargo-llvm-cov" "$work/junk.json"

# 6. A report containing only test/ files is the same failure mode as case 4,
#    reached the realistic way: someone narrows --ignore-filename-regex.
python3 - "$work/tests-only.json" <<'PY'
import json
import sys
base = {
    "data": [
        {
            "files": [
                {
                    "branches": {}, "mcdc_records": [], "expansions": [],
                    "filename": "/build/wt/crates/core/tests/boundary_spec.rs",
                    "segments": [], "summary": {"lines": {"count": 10, "covered": 10}},
                }
            ],
            "functions": {},
            "totals": {},
        }
    ],
    "type": "llvm.coverage.json.export",
    "version": "2.0.1",
}
json.dump(base, open(sys.argv[1], "w", encoding="utf-8"))
PY
expect "a report with only test/ files is not 100% coverage" fail "no first-party files" \
	"$work/tests-only.json"

if [ "$fail" -ne 0 ]; then
	echo >&2
	echo "coverage gate self-test FAILED: the gate is not proven to go red." >&2
	exit 1
fi

echo "coverage gate self-test passed: the gate is proven to turn red on a drop, on an"
echo "empty report, on a test-only report and on garbage input."