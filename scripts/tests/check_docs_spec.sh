#!/bin/sh
# The documentation checker's self-test (OSS-DOCS-01..05).
#
# It runs the REAL `scripts/check-docs.sh` against a COPY of the repository tree, so a failing
# case cannot damage the checkout it was started from. Each case makes exactly one change to the
# copy and asserts on the exit status AND on what the script printed, because "it failed" and
# "it failed for the right reason" are different claims.
#
# Why the cases below matter: rule 4 (no internal tracking reference) originally scanned only
# Markdown, so a project-board id written in a `//!` doc comment inside `crates/*/src/` shipped
# inside the published crate and rendered into `cargo doc` output. Every one of these cases is
# about that gap, plus the two boundaries the rule deliberately does not cross.
#
# Run directly (`sh scripts/tests/check_docs_spec.sh`) or from CI. `cargo test` does not execute
# it: it needs `python3` and a writable copy of the tree, which is not something a unit test
# should assume.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)
script_rel=scripts/check-docs.sh

pass=0
fail=0

# A fresh copy of the tree, minus target/ and .git so it is cheap.
#
# The root manifests and LICENSE have to come along even though no rule reads them: the Markdown
# rules resolve relative links through them (`docs/MAINTENANCE.md` links `../Cargo.toml`), so a
# fixture that omitted them would report a pile of broken links and drown the case under test.
fixture() {
    d=$(mktemp -d)
    for p in docs scripts .github crates; do
        [ -e "$repo/$p" ] && cp -R "$repo/$p" "$d/"
    done
    for p in README.md ROADMAP.md SECURITY.md CHANGELOG.md CONTRIBUTING.md \
             CODE_OF_CONDUCT.md SECURITY.md Cargo.toml Cargo.lock LICENSE \
             deny.toml rustfmt.toml rust-toolchain.toml clippy.toml \
             THIRD-PARTY-LICENSES.md install.sh; do
        [ -e "$repo/$p" ] && cp "$repo/$p" "$d/"
    done
    rm -rf "$d/crates"/*/target 2>/dev/null || true
    echo "$d"
}

run() {
    # run <fixture> -> sets $out and $status
    d=$1
    set +e
    out=$(cd "$d" && sh "$script_rel" 2>&1)
    status=$?
    set -e
}

ok() { pass=$((pass + 1)); printf '  ok   %s\n' "$1"; }
no() {
    fail=$((fail + 1))
    printf '  FAIL %s\n' "$1"
    printf '%s\n' "$2" | sed 's/^/         /'
}

expect_status() {
    # expect_status <name> <fixture> <wanted-status> [must-contain]
    name=$1; d=$2; want=$3; needle=${4:-}
    run "$d"
    if [ "$status" -ne "$want" ]; then
        no "$name" "expected exit $want, got $status
$out"
        return
    fi
    if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF "$needle"; then
        no "$name" "exit was $want but the output did not mention: $needle
$out"
        return
    fi
    ok "$name"
}

echo "check-docs.sh (OSS-DOCS)"

# --- OSS-DOCS-01: a board id in a crate src doc comment must fail ---------------------
#
# This is the regression test for the release-blocking leak. The line is placed in a doc comment
# on a real module so it is exactly the shape that ships: `//!` becomes crate-level docs and is
# rendered by `cargo doc`.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/edit/src/spec/undo_spec.rs"
s = p.read_text()
p.write_text("//! Ticket: `Y20261002/REQ-SECURITY-REVIEW/ISSUE-SEC-AUDIT`\n" + s)
PY
expect_status "OSS-DOCS-01 a board id in a crate src doc comment exits non-zero" "$d" 1 \
    "crates/edit/src/spec/undo_spec.rs:1: internal tracking reference in crate source"
rm -rf "$d"

# --- OSS-DOCS-02: it must be reported for EVERY crate, not just the first -------------
#
# A glob that only matched `crates/edit/src` would pass case 01 and leave the other six crates
# leaking, so this plants one in the least likely-looking crate and requires it by name.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/lang/src/whatever.rs"
p.parent.mkdir(parents=True, exist_ok=True)
p.write_text("//! ISSUE-PATTERN-MATCH\npub fn probe() {}\n")
PY
expect_status "OSS-DOCS-02 the scan covers crates other than edit" "$d" 1 \
    "crates/lang/src/whatever.rs:1: internal tracking reference in crate source"
rm -rf "$d"

# --- OSS-DOCS-03: it must reach a NESTED src path --------------------------------------
#
# `crates/*/src/**` has to be recursive: the real leaks live in `src/spec/` and
# `src/outline/`, two and three levels down. A non-recursive `src/*.rs` glob would pass 01 and
# 02 while every actual occurrence stayed invisible.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/query/src/outline/deep/nested.rs"
p.parent.mkdir(parents=True, exist_ok=True)
p.write_text("//! ISSUE-QUERY-OUTLINE-ECMA\npub fn probe() {}\n")
PY
expect_status "OSS-DOCS-03 a board id in a nested src path exits non-zero" "$d" 1 \
    "crates/query/src/outline/deep/nested.rs:1: internal tracking reference in crate source"
rm -rf "$d"

# --- OSS-DOCS-04: a NON-doc-comment line in src/ still counts --------------------------
#
# The rule is deliberately a whole-file line scan rather than a doc-comment parse: a board id in
# an ordinary `//` comment still ships in the source tarball, and the other two rules in this
# script (personal paths, script paths) are line scans too, so this keeps the file consistent.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/src/lib.rs"
s = p.read_text()
p.write_text(s + "\n// see also REQ-SECURITY-REVIEW for the audit\n")
PY
expect_status "OSS-DOCS-04 a board id in a plain src comment exits non-zero" "$d" 1 \
    "internal tracking reference in crate source"
rm -rf "$d"

# --- OSS-DOCS-05: the documented boundary - tests/ is NOT scanned ----------------------
#
# This is the one case that pins a decision NOT to widen the rule. Test sources are never
# published and never reach `cargo doc`, so a board id there leaks nothing a crates.io consumer
# can see, and there are enough of them that flagging them would bury the src/ findings. If this
# case ever starts failing, that is the signal to revisit the boundary deliberately rather than
# by accident.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/edit/tests/probe_under_test.rs"
p.write_text("//! ISSUE-SEC-AUDIT\n#[test]\nfn probe() {}\n")
PY
expect_status "OSS-DOCS-05 a board id under tests/ is deliberately not flagged" "$d" 0 \
    "documentation check passed"
rm -rf "$d"

# --- OSS-DOCS-06: the pristine tree passes -------------------------------------------
#
# Last, so a regression that makes the checker reject the real tree is caught here rather than
# being masked by the cases above. This is also the case that fails if a src/ rewrite drops a
# stable label and accidentally introduces a board id somewhere new.
d=$(fixture)
expect_status "OSS-DOCS-06 the unmodified tree passes" "$d" 0 "documentation check passed"
run "$d"
reported=$(printf '%s' "$out" | sed -n 's/.*passed: [0-9]* files, \([0-9]*\) crate sources.*/\1/p')
rm -rf "$d"

# The scan is not allowed to quietly degrade to "found no crate sources and passed": an empty
# glob satisfies every rule above vacuously.
if [ -z "$reported" ] || [ "$reported" -lt 10 ]; then
    no "OSS-DOCS-07 the scan reports a non-trivial number of crate sources" \
        "expected at least 10 crate sources, got '${reported:-none}'"
else
    ok "OSS-DOCS-07 the scan reports a non-trivial number of crate sources ($reported)"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]