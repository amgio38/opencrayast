#!/usr/bin/env bash
# Self-test for verify-release.sh: proves the pre-release checks can fail.
#
# A release checklist whose checks have never been seen to go red is a checklist
# that cannot stop a bad release. This runs the REAL verifier against a copy of the
# repository tree with exactly one thing broken, and requires the right check to go
# red -- asserting on the check id, because "it failed" and "it failed for the right
# reason" are different claims.
#
# Cases (each breaks exactly one thing on a copy):
#   LIC-01  Cargo.toml licence changed to a non-MIT value
#   VER-01  a crate manifest given a literal version
#   VER-03  rust-toolchain.toml channel unpinned / disagreeing
#   GIT-01  a workflow action pinned to a tag instead of a SHA
#   HIST-01 CHANGELOG's only section header removed
#   PKG-01  a community file emptied
#   M-ack   manual items acknowledged           → must pass
#   no-ack  manual items NOT acknowledged       → must exit non-zero
#
# DOC-01/02/03/04 and DENY-01 are NOT mutated here: they shell out to other scripts and
# to cargo-deny, and mutating those would be testing the wrong program. Those have
# their own reverse verification (scripts/tests/check_matrix_spec.sh,
# scripts/test-check-layering.sh, and scripts/test-check-undeclared-src.sh).
#
# Usage: scripts/tests/verify_release_spec.sh
set -uo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
script_rel=scripts/verify-release.sh

pass=0
fail=0
ok() {
	printf 'ok: %s\n' "$1"
	pass=$((pass + 1))
}
bad() {
	printf 'FAIL: %s\n' "$1" >&2
	fail=$((fail + 1))
}

# A copy of the tree the verifier can read: manifests, docs, workflows, licence.
# A real copy of the tree, so the verifier reads exactly the files that are here.
#
# The copy must be COMPLETE. DOC-01 (link resolution), DOC-02 (every threat's named
# test file) and DENY-01 (which reads every crate manifest) all read the real tree,
# so a partial fixture makes them fail for the wrong reason. An earlier version of
# this script copied only manifests, and DOC-01/02/DENY-01 duly went red — on the
# fixture, not on the release. Hence this comment.
#
# It is a copy of the WORKING TREE rather than of `git archive HEAD`, because the
# script under test is itself usually uncommitted when this self-test is first run,
# and archiving HEAD would then test a repository that does not contain the verifier
# at all — which is what the first run of this script did. Copying the working tree
# keeps the self-test meaningful before and after the commit. target/ and .git are
# excluded: the first is huge and irrelevant, the second would make GIT-02 describe
# the fixture's own history rather than the tree's cleanliness.
fixture() {
	d=$(mktemp -d)
	tar -C "$root" \
		--exclude=./target --exclude=./.git --exclude=./dist \
		-cf - . | tar -x -C "$d"
	# A git repo with a clean tree, so GIT-02 is deterministic and does not fail
	# merely because the fixture is untracked.
	git -C "$d" init -q 2>/dev/null
	git -C "$d" add -A 2>/dev/null
	git -C "$d" -c user.name=t -c user.email=t@e.invalid commit -qm fixture 2>/dev/null
	echo "$d"
}

run_verifier() {
	local d=$1
	shift
	(cd "$d" && bash "$script_rel" "$@" 2>&1)
}

# expect_red <name> <check-id> <fixture>
expect_red() {
	local name=$1 id=$2 d=$3
	local out
	out=$(run_verifier "$d")
	if [ -z "$out" ]; then
		bad "$name: verifier produced no output"
		return
	fi
	if printf '%s\n' "$out" | grep -qE "FAIL[[:space:]]+$id[[:space:]]"; then
		ok "$name"
	else
		bad "$name: expected check $id to FAIL; got:"$'\n'"$out"
	fi
}

# expect_green <name> <fixture>
expect_green() {
	local name=$1 d=$2
	local out
	out=$(run_verifier "$d")
	if printf '%s\n' "$out" | grep -q "FAIL"; then
		bad "$name: expected no FAIL lines; got:"$'\n'"$out"
	else
		ok "$name"
	fi
}

printf 'verifying the baseline copy is green on mechanical checks...\n'
base=$(fixture)
expect_green "baseline mechanical checks pass" "$base"
rm -rf "$base"

# --- LIC-01: licence mismatch -------------------------------------------------
d=$(fixture)
python3 - "$d/Cargo.toml" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
s = re.sub(r'(?m)^license\s*=\s*"MIT"', 'license = "Apache-2.0"', s)
open(p, "w").write(s)
PY
expect_red "LIC-01 goes red when the licence field disagrees" LIC-01 "$d"
rm -rf "$d"

# --- VER-01: literal version in a crate --------------------------------------
d=$(fixture)
python3 - "$d/crates/core/Cargo.toml" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
s = re.sub(r'(?m)^version\.workspace\s*=\s*true', 'version = "9.9.9"', s)
open(p, "w").write(s)
PY
expect_red "VER-01 goes red when a crate pins a literal version" VER-01 "$d"
rm -rf "$d"

# --- VER-03: toolchain unpinned ----------------------------------------------
d=$(fixture)
printf '[toolchain]\n' >"$d/rust-toolchain.toml"
expect_red "VER-03 goes red when the toolchain channel is unpinned" VER-03 "$d"
rm -rf "$d"

# --- GIT-01: action pinned to a tag ------------------------------------------
d=$(fixture)
python3 - "$d/.github/workflows/ci.yml" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
s = s.replace("actions/checkout@11d5960a326750d5838078e36cf38b85af677262 # v4.4.0",
              "actions/checkout@v4 # unpinned on purpose")
open(p, "w").write(s)
PY
expect_red "GIT-01 goes red when an action is pinned to a tag" GIT-01 "$d"
rm -rf "$d"

# --- HIST-01: no current changelog section -----------------------------------
d=$(fixture)
python3 - "$d/CHANGELOG.md" <<'PY'
import re, sys
p = sys.argv[1]
s = open(p).read()
s = re.sub(r'(?m)^## \[Unreleased\].*$', '## (removed by the self-test)', s)
s = re.sub(r'(?m)^## \[\d[^\]]*\].*$', '', s)
open(p, "w").write(s)
PY
expect_red "HIST-01 goes red when the CHANGELOG has no current section" HIST-01 "$d"
rm -rf "$d"

# --- PKG-01: empty community file --------------------------------------------
d=$(fixture)
: >"$d/SECURITY.md"
expect_red "PKG-01 goes red when a community file is empty" PKG-01 "$d"
rm -rf "$d"

# --- ART-01: the artefact must carry the third-party notices ------------------
# `dist/` is excluded from the fixture, so ART-01 SKIPs there. Staging a tree without the
# notices has to be red — that is the whole point of ART-01, and a check that only ever
# skips would be a check that never ran.
d=$(fixture)
mkdir -p "$d/dist/x86_64-unknown-linux-musl"
printf '0.20261002.1\n' >"$d/dist/VERSION"
printf 'deadbeef  x86_64-unknown-linux-musl/opencrayast\n' >"$d/dist/SHA256SUMS"
expect_red "ART-01 goes red when dist/ has no third-party licence notices" ART-01 "$d"
rm -rf "$d"

# ...and staging it WITH them must turn it green, so the case above is not passing for
# some unrelated reason.
d=$(fixture)
mkdir -p "$d/dist/x86_64-unknown-linux-musl"
printf '0.20261002.1\n' >"$d/dist/VERSION"
printf '# Third-party licenses\n' >"$d/dist/THIRD-PARTY-LICENSES.md"
printf 'deadbeef  x86_64-unknown-linux-musl/opencrayast\n' >"$d/dist/SHA256SUMS"
out=$(run_verifier "$d")
if printf '%s\n' "$out" | grep -q "FAIL[[:space:]]\+ART-01"; then
	bad "ART-01 stayed red with the notices present; got:"$'\n'"$out"
else
	ok "ART-01 goes green once dist/ carries THIRD-PARTY-LICENSES.md"
fi
rm -rf "$d"

# --- ART-02: the artefact must carry a checksum file --------------------------
d=$(fixture)
mkdir -p "$d/dist/x86_64-unknown-linux-musl"
printf '0.20261002.1\n' >"$d/dist/VERSION"
printf '# Third-party licenses\n' >"$d/dist/THIRD-PARTY-LICENSES.md"
expect_red "ART-02 goes red when dist/ has no SHA256SUMS" ART-02 "$d"
rm -rf "$d"

# --- GIT-02: dirty tree -------------------------------------------------------
d=$(fixture)
echo "stray" >>"$d/README.md"
expect_red "GIT-02 goes red when the working tree is dirty" GIT-02 "$d"
rm -rf "$d"

# --- the red-light line -------------------------------------------------------
# The defect this gate was opened for: with a mechanical check failing, the run still
# printed "release readiness: mechanical checks passed." A person reading the tail of a
# release script is told the wrong thing, and the exit code alone is easy to miss in a
# CI log. Both the FAIL wording and the absence of the green wording are asserted — one
# without the other would pass if the script printed both.
d=$(fixture)
echo "stray" >>"$d/SECURITY.md"
out=$(run_verifier "$d")
if printf '%s\n' "$out" | grep -q "mechanical checks passed"; then
	bad "a failing run must NOT say 'mechanical checks passed'; got:"$'\n'"$out"
elif printf '%s\n' "$out" | grep -q "release readiness: FAILED"; then
	ok "a failing run says FAILED and never claims the mechanical checks passed"
else
	bad "a failing run did not print either readiness line; got:"$'\n'"$out"
fi
rm -rf "$d"

# ...and the same must hold when every manual item IS acknowledged, which is the mode a
# real release uses. The old unconditional line lived on this path too.
d=$(fixture)
echo "stray" >>"$d/SECURITY.md"
out=$(run_verifier "$d" --ack-manual M1,M2,M3,M4,M5,M6,M7)
if printf '%s\n' "$out" | grep -q "mechanical checks passed"; then
	bad "an acknowledged but failing run must NOT say 'mechanical checks passed'; got:"$'\n'"$out"
else
	ok "an acknowledged run with a failing check also refuses the green readiness line"
fi
rm -rf "$d"

# --- the manual gate ----------------------------------------------------------
d=$(fixture)
out=$(run_verifier "$d" --require-manual)
if [ $? -eq 0 ]; then
	bad "unacknowledged manual items must NOT exit 0 under --require-manual; got:"$'\n'"$out"
else
	if printf '%s\n' "$out" | grep -q "PENDING M1"; then
		ok "unacknowledged manual items exit non-zero under --require-manual and are listed"
	else
		bad "exited non-zero but did not report M1 as PENDING; got:"$'\n'"$out"
	fi
fi

d=$(fixture)
out=$(run_verifier "$d" --ack-manual M1,M2,M3,M4,M5,M6,M7)
if printf '%s\n' "$out" | grep -q "all checks passed"; then
	ok "acknowledging every manual item lets the run pass"
else
	bad "acknowledged run did not pass; got:"$'\n'"$out"
fi

# ...and acknowledging only some of them must NOT pass. A checklist that accepts a
# partial sign-off is worse than one that accepts none.
d=$(fixture)
out=$(run_verifier "$d" --ack-manual M1,M2)
if [ $? -eq 0 ]; then
	bad "partial manual sign-off must NOT exit 0; got:"$'\n'"$out"
else
	if printf '%s\n' "$out" | grep -q "not acknowledged"; then
		ok "a partial manual sign-off is refused and names the missing ids"
	else
		bad "partial sign-off exited non-zero without naming the missing ids; got:"$'\n'"$out"
	fi
fi
rm -rf "$d"

# --- the CI-relevant default: report PENDING, do not fail ---------------------
# This is the mode CI runs in. It must exit 0 on a clean tree with manual items
# open, while still printing every open item, so the check is visible rather than
# either ignored or permanently red.
d=$(fixture)
out=$(run_verifier "$d")
rc=$?
if [ "$rc" -ne 0 ]; then
	bad "the default run must exit 0 with manual items merely pending; got rc=$rc:"$'\n'"$out"
else
	if printf '%s\n' "$out" | grep -q "PENDING M1"; then
		ok "the default run reports manual items as PENDING without failing"
	else
		bad "default run exited 0 but did not report PENDING items; got:"$'\n'"$out"
	fi
fi

# ...and --require-manual must fail in exactly that situation.
out=$(run_verifier "$d" --require-manual)
rc=$?
if [ "$rc" -ne 0 ]; then
	ok "--require-manual fails while manual items are open"
else
	bad "--require-manual exited 0 with manual items open; got:"$'\n'"$out"
fi
rm -rf "$d"

echo
if [ "$fail" -ne 0 ]; then
	echo "release-check self-test FAILED ($fail of $((pass + fail)))" >&2
	exit 1
fi
echo "release-check self-test passed: $pass/$((pass + fail)) cases, including every check going red on its own break."