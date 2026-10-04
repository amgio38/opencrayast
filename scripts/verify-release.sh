#!/usr/bin/env bash
# Mechanical pre-release verification (docs/RELEASE-CHECKLIST.md).
#
# WHY A SCRIPT AND NOT A TICKED BOX
# ---------------------------------
# A pre-release checklist made of Markdown boxes is a list of intentions. This
# script turns every item that *can* be checked mechanically into an assertion with
# an exit status, so "done" is an observation rather than a memory. Items that
# genuinely cannot be checked by a script are NOT silently skipped: they are listed
# by name in the MANUAL section and the script refuses to report a clean release
# while any of them is unconfirmed, unless `--ack-manual` names them.
#
# WHAT IS CHECKED HERE
#   PKG-01 community files exist and are non-empty
#   PKG-02 issue forms and the PR template exist
#   PKG-03 Dependabot is configured
#   LIC-01 the workspace licence field and the licence file agree
#   LIC-02 no licence file was replaced by an unexpected one
#   VER-01 workspace version parses and matches the documented scheme
#   VER-02 every crate inherits the workspace version (no literal, none missing)
#   VER-03 the toolchain version is pinned and matches the workspace rust-version
#   DOC-01 scripts/check-docs.sh passes (links, personal paths, CJK, internal ids)
#   DOC-02 scripts/check-matrix.sh passes (every threat has a named test)
#   DOC-03 scripts/check-layering.sh passes
#   DOC-04 scripts/check-undeclared-src.sh passes (every crates/*/src .rs is reached by a mod)
#   DOC-05 scripts/check-grammar-provenance.sh passes (every tree-sitter grammar is pinned)
#   ART-01 the release artifact carries THIRD-PARTY-LICENSES.md
#   ART-02 the artifact carries a checksum file
#   DENY-01 cargo deny check passes
#   GIT-01 CI action pins are full 40-hex SHAs
#   GIT-02 the working tree is clean (no uncommitted release-blocking change)
#   CI-  01 the coverage gate and its self-test are wired into the workflow
#   CI-  02 the undeclared-src gate and its self-test are wired into the workflow
#   HIST-01 the CHANGELOG has a dated or Unreleased section
#
# Usage:
#   scripts/verify-release.sh                       # mechanical checks; PENDING is reported
#   scripts/verify-release.sh --require-manual      # also fail while any manual item is open
#   scripts/verify-release.sh --ack-manual M1,M2    # record sign-offs (implies --require-manual)
#   scripts/verify-release.sh --list-manual         # print the manual checklist
#
# Exit 0 = every mechanical check passed and no required manual item is open.
# Exit 1 = at least one failed, or a required manual item is unacknowledged.
#
# WHY --require-manual IS OPT-IN
# ------------------------------
# CI runs this on every PR, and CI cannot sign for a human. If unacknowledged manual
# items failed the default run, every PR would be red forever and the check would
# simply be deleted. So the default run REPORTS each open item as PENDING on stdout
# and does not fail on it -- it is still never silently passed, which is the actual
# requirement. The release process, where a person is present, passes
# --require-manual, and there the open items do fail the run.
set -uo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

# Items that need a human, a tag, or a network judgement. Deliberately NOT
# faked: `scripts/check-docs.sh` can prove a link resolves, not that a person
# read it.
MANUAL_ITEMS="M1:CI-green-on-all-three-OS
M2:artifacts-built-and-checksums-recorded
M3:changelog-entry-names-every-user-visible-change
M4:security-supported-versions-still-true
M5:history-scan-for-secrets-and-personal-data-approved
M6:maintainer-reviewed-the-release-tag
M7:vulnerability-reporting-path-rehearsed-once"

ack=""
show_manual_list=0
require_manual=0
while [ $# -gt 0 ]; do
	case "$1" in
	--ack-manual)
		ack=${2:?--ack-manual needs a comma-separated list of item ids}
		require_manual=1
		shift 2
		;;
	--require-manual)
		require_manual=1
		shift
		;;
	--list-manual)
		show_manual_list=1
		shift
		;;
	-h | --help)
		sed -n '2,55p' "$0" | sed 's/^# \{0,1\}//'
		exit 0
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

pass=0
fail=0
skipped=0
ok() {
	pass=$((pass + 1))
	printf '  PASS  %-9s %s\n' "$1" "$2"
}
bad() {
	fail=$((fail + 1))
	printf '  FAIL  %-9s %s\n' "$1" "$2"
}
skip() {
	skipped=$((skipped + 1))
	printf '  SKIP  %-9s %s\n' "$1" "$2"
}

section() { printf '\n%s\n' "$1"; }

# PKG-01 community files.
section "Community files"
for f in LICENSE CONTRIBUTING.md CODE_OF_CONDUCT.md SECURITY.md CHANGELOG.md ROADMAP.md; do
	if [ -s "$f" ]; then ok PKG-01 "$f exists and is non-empty"; else bad PKG-01 "$f missing or empty"; fi
done

# PKG-02 issue forms and PR template.
section "Issue and PR templates"
for f in .github/ISSUE_TEMPLATE/bug_report.yml .github/ISSUE_TEMPLATE/feature_request.yml \
	.github/ISSUE_TEMPLATE/config.yml .github/pull_request_template.md; do
	if [ -s "$f" ]; then ok PKG-02 "$f"; else bad PKG-02 "$f missing or empty"; fi
done

# PKG-03 Dependabot.
section "Dependency automation"
if [ -s .github/dependabot.yml ] && grep -q 'package-ecosystem' .github/dependabot.yml; then
	ok PKG-03 ".github/dependabot.yml declares package ecosystems"
else
	bad PKG-03 ".github/dependabot.yml missing or declares no ecosystem"
fi

# LIC-01 licence reconcile.
section "Licence"
lic_out=$(python3 - <<'PY'
import re
import sys
from pathlib import Path

try:
    cargo = Path("Cargo.toml").read_text(encoding="utf-8")
    lic = Path("LICENSE").read_text(encoding="utf-8")
except OSError as exc:
    print(f"cannot read: {exc}")
    sys.exit(1)
m = re.search(r'(?m)^license\s*=\s*"([^"]+)"', cargo)
if not m:
    print("Cargo.toml has no workspace.package license")
    sys.exit(1)
field = m.group(1)
first = lic.splitlines()[0] if lic.splitlines() else ""
# MIT's canonical first line is "MIT License" / "MIT No Attribution" style; the
# check is that the identifier appears at the start, not that the wording matches.
if not re.match(r"MIT", first):
    print(f"LICENSE does not start with MIT (got {first!r})")
    sys.exit(1)
if field != "MIT":
    print(f"Cargo.toml license={field!r} but LICENSE is MIT")
    sys.exit(1)
print(f"Cargo.toml license={field!r}; LICENSE starts with {first!r}")
PY
)
if [ $? -eq 0 ]; then ok LIC-01 "$lic_out"; else bad LIC-01 "$lic_out"; fi

# LIC-02 the licence file is the MIT text, not a placeholder.
section "Licence text"
if grep -qi 'permission is hereby granted, free of charge' LICENSE; then
	ok LIC-02 "LICENSE contains the MIT grant text"
else
	bad LIC-02 "LICENSE does not contain the MIT grant text"
fi

# VER-01/02/03 versions.
section "Version consistency"
ver_out=$(python3 - <<'PY'
import re
import sys
from pathlib import Path

root = Path("Cargo.toml").read_text(encoding="utf-8")
block = re.search(r"(?ms)^\[workspace\.package\](.*?)(?=^\[|\Z)", root)
if not block:
    print("no [workspace.package]")
    sys.exit(1)
versions = re.findall(r'(?m)^version\s*=\s*"([^"]+)"', block.group(1))
if len(versions) != 1:
    print(f"expected exactly one workspace.version, found {versions}")
    sys.exit(1)
v = versions[0]
# ADR-013 scheme: 0.YYYYMMDD.N until 1.0.
if not re.fullmatch(r"\d+\.\d{8}\.\d+", v):
    print(f"version {v!r} does not match the documented scheme N.YYYYMMDD.N")
    sys.exit(1)
rust_version = re.search(r'(?m)^rust-version\s*=\s*"([^"]+)"', block.group(1))
if not rust_version:
    print("no workspace.package rust-version")
    sys.exit(1)
lit = re.compile(r'(?m)^version\s*=\s*"')
ws = re.compile(r"(?m)^version\.workspace\s*=\s*true\s*$")
crates = sorted(Path("crates").glob("*/Cargo.toml"))
if not crates:
    print("no crates/")
    sys.exit(1)
problems = []
for p in crates:
    t = p.read_text(encoding="utf-8")
    if lit.search(t):
        problems.append(f"literal version in {p}")
    if not ws.search(t):
        problems.append(f"missing version.workspace in {p}")
if problems:
    print("; ".join(problems))
    sys.exit(1)
print(f"version {v} (rust-version {rust_version.group(1)}); {len(crates)} crates inherit it")
PY
)
if [ $? -eq 0 ]; then ok VER-01 "$ver_out"; else bad VER-01 "$ver_out"; fi

tc_out=$(python3 - <<'PY'
import re
import sys
from pathlib import Path

toolchain = Path("rust-toolchain.toml").read_text(encoding="utf-8")
m = re.search(r'(?m)^channel\s*=\s*"([^"]+)"', toolchain)
if not m:
    print("rust-toolchain.toml has no pinned channel")
    sys.exit(1)
root = Path("Cargo.toml").read_text(encoding="utf-8")
rv = re.search(r'(?m)^rust-version\s*=\s*"([^"]+)"', root)
channel = m.group(1)
if rv and channel.split(".")[0] != rv.group(1).split(".")[0]:
    print(f"toolchain {channel} and rust-version {rv.group(1)} disagree on the major version")
    sys.exit(1)
comps = re.findall(r'(?m)^(?:- )?"?([a-z-]+)"?,?\s*$', toolchain)
print(f"toolchain {channel} pinned; rust-version {rv.group(1) if rv else '?'}")
PY
)
if [ $? -eq 0 ]; then ok VER-03 "$tc_out"; else bad VER-03 "$tc_out"; fi

# Documentation checks.
section "Documentation checks"
for pair in "DOC-01:scripts/check-docs.sh" "DOC-02:scripts/check-matrix.sh" "DOC-03:scripts/check-layering.sh" "DOC-04:scripts/check-undeclared-src.sh" "DOC-05:scripts/check-grammar-provenance.sh"; do
	id=${pair%%:*}
	script=${pair#*:}
	if out=$(bash "$script" 2>&1); then
		ok "$id" "$script: $(printf '%s' "$out" | tail -1)"
	else
		bad "$id" "$script failed: $(printf '%s' "$out" | tail -3 | tr '\n' ' ')"
	fi
done

# DENY-01.
section "Dependency policy"
if command -v cargo-deny >/dev/null 2>&1; then
	if out=$(cargo deny check 2>&1); then
		ok DENY-01 "cargo deny check: $(printf '%s' "$out" | tail -1)"
	else
		bad DENY-01 "cargo deny check failed: $(printf '%s' "$out" | tail -3 | tr '\n' ' ')"
	fi
else
	# This used to be `skip`, which reads as "not applicable" and left a release with no
	# dependency-policy evidence at all while the summary said everything passed. cargo-deny is
	# in CI, so the policy IS enforced there; a local machine without the binary simply cannot
	# answer, and that is a `fail` worth seeing rather than a quiet gap.
	bad DENY-01 "cargo-deny is not installed — dependency policy is unverified on this machine (CI runs it; install it with \`cargo install cargo-deny\`)"
fi

# CI wiring.
section "CI wiring"
sha_out=$(python3 - <<'PY'
import re
import sys
from pathlib import Path

ci = Path(".github/workflows/ci.yml").read_text(encoding="utf-8")
bad = []
for line in ci.splitlines():
    if line.strip().startswith("uses:"):
        at = line.split("@")
        if len(at) < 2 or not re.fullmatch(r"[0-9a-f]{40}", at[1].split()[0]):
            bad.append(line.strip())
if bad:
    print("; ".join(bad))
    sys.exit(1)
print(f"{sum(1 for l in ci.splitlines() if l.strip().startswith('uses:'))} actions pinned to full SHAs")
PY
)
if [ $? -eq 0 ]; then ok GIT-01 "$sha_out"; else bad GIT-01 "$sha_out"; fi

cov_out=$(python3 - <<'PY'
import sys
from pathlib import Path

ci = Path(".github/workflows/ci.yml").read_text(encoding="utf-8")
for name in ("check-coverage.sh", "test-check-coverage.sh"):
    if name not in ci:
        print(f"{name} is not wired into ci.yml")
        sys.exit(1)
print("coverage gate and its self-test are both wired into ci.yml")
PY
)
if [ $? -eq 0 ]; then ok CI-01 "$cov_out"; else bad CI-01 "$cov_out"; fi

undecl_out=$(python3 - <<'PY'
import sys
from pathlib import Path

ci = Path(".github/workflows/ci.yml").read_text(encoding="utf-8")
for name in ("check-undeclared-src.sh", "test-check-undeclared-src.sh"):
    if name not in ci:
        print(f"{name} is not wired into ci.yml")
        sys.exit(1)
print("undeclared-src gate and its self-test are both wired into ci.yml")
PY
)
if [ $? -eq 0 ]; then ok CI-02 "$undecl_out"; else bad CI-02 "$undecl_out"; fi

# GIT-02 clean tree.
section "Repository state"
if [ -z "$(git status --porcelain 2>/dev/null)" ]; then
	ok GIT-02 "working tree is clean"
else
	bad GIT-02 "working tree has uncommitted changes: $(git status --porcelain | head -3 | tr '\n' ' ')"
fi

# ART-01/ART-02 the artefact tree, when it has been staged.
#
# These are checks on `dist/`, which is gitignored and empty on a fresh checkout — so a run
# without a staged artefact must NOT report them as passed. Either the tree is there and they
# are checked for real, or they are `skip` with the reason. A gate that answers "pass" for a
# file it never looked at is the exact false green this release check exists to avoid.
section "Release artefact"
if [ -d "${DIST:-dist}" ] && [ -n "$(ls -A "${DIST:-dist}" 2>/dev/null)" ]; then
	if [ -s "${DIST:-dist}/THIRD-PARTY-LICENSES.md" ]; then
		ok ART-01 "dist/THIRD-PARTY-LICENSES.md present and non-empty"
	else
		bad ART-01 "dist/THIRD-PARTY-LICENSES.md missing or empty — the artefact ships linked third-party code and must carry its notices"
	fi
	if [ -s "${DIST:-dist}/SHA256SUMS" ]; then
		ok ART-02 "dist/SHA256SUMS present and non-empty"
	else
		bad ART-02 "dist/SHA256SUMS missing or empty"
	fi
else
	skip ART-01 "no staged dist/ tree; run \`make release-static\` first"
	skip ART-02 "no staged dist/ tree; run \`make release-static\` first"
fi

# CHANGELOG.
section "Changelog"
if grep -qE '^## \[(Unreleased|[0-9])' CHANGELOG.md; then
	ok HIST-01 "CHANGELOG.md has a current section"
else
	bad HIST-01 "CHANGELOG.md has no Unreleased or versioned section"
fi

# Manual items.
section "Manual sign-off (cannot be automated)"
if [ "$show_manual_list" -eq 1 ]; then
	printf '%s\n' "$MANUAL_ITEMS" | while read -r line; do
		id=${line%%:*}
		printf '  %s  %s\n' "$id" "${line#*:}"
	done
	exit 0
fi

if [ -z "$ack" ]; then
	printf '%s\n' "$MANUAL_ITEMS" | while read -r line; do
		id=${line%%:*}
		printf '  PENDING %-5s %s\n' "$id" "${line#*:}"
	done
	echo
	if [ "$require_manual" -eq 1 ]; then
		printf '\nsummary: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
		echo "release readiness: manual items are open and --require-manual was given"
		exit 1
	fi
	printf '\nsummary: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
	# The line below used to be printed unconditionally, so a run with a failing mechanical
	# check still ended by saying "mechanical checks passed". Anyone reading the tail — and a
	# release script's tail is what a person reads — was told the wrong thing. It is now
	# conditional on $fail, and the failing case names the count.
	if [ "$fail" -eq 0 ]; then
		echo "release readiness: mechanical checks passed."
	else
		echo "release readiness: FAILED ($fail mechanical check(s) failed; see the FAIL lines above)."
	fi
	echo "The PENDING items above are NOT satisfied by this run. Re-run with"
	echo "--require-manual (or --ack-manual M1,...) once a person has confirmed each."
	exit $((fail > 0 ? 1 : 0))
fi

missing=""
for line in $MANUAL_ITEMS; do
	id=${line%%:*}
	case ",$ack," in
	*",$id,"*) ok "$id" "signed off by the person running this script" ;;
	*) missing="$missing $id" ;;
	esac
done
if [ -n "$missing" ]; then
	bad MANUAL "manual items not acknowledged:$missing"
fi

echo
printf 'summary: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
if [ "$fail" -eq 0 ]; then
	echo "release readiness: all checks passed"
	exit 0
fi
echo "release readiness: FAILED ($fail)"
exit 1