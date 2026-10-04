#!/bin/sh
# The threat-to-test matrix tests (SECFIX3-01..05).
#
# These run the REAL `scripts/check-matrix.sh` against a COPY of the repository
# tree, so a failing case cannot damage the checkout it was started from. Each
# case makes exactly one change to the copy and asserts on the exit status and on
# what the script printed, because "it failed" and "it failed for the right reason"
# are different claims.
#
# Run directly (`sh scripts/tests/check_matrix_spec.sh`) or from CI. `cargo test`
# does not execute it: it needs `python3` and a writable copy of the tree, which is
# not something a unit test should assume.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)
script_rel=scripts/check-matrix.sh

pass=0
fail=0

# A fresh copy of the tree, minus target/ and .git so it is cheap.
fixture() {
    d=$(mktemp -d)
    for p in docs scripts .github crates; do
        [ -e "$repo/$p" ] && cp -R "$repo/$p" "$d/"
    done
    [ -e "$repo/README.md" ] && cp "$repo/README.md" "$d/"
    [ -e "$repo/ROADMAP.md" ] && cp "$repo/ROADMAP.md" "$d/"
    [ -e "$repo/SECURITY.md" ] && cp "$repo/SECURITY.md" "$d/"
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

echo "check-matrix.sh (SECFIX3)"

# --- SECFIX3-01: deleting a test file the catalogue points at must fail ----------
#
# This is the regression test for SEC-A1 finding F-05: before the Target check,
# this exact deletion left CI green.
d=$(fixture)
rm "$d/crates/core/tests/boundary_spec.rs"
expect_status "SECFIX3-01 deleting a referenced test file exits non-zero" "$d" 1 \
    "BND-01: test file crates/core/tests/boundary_spec.rs does not exist"
rm -rf "$d"

# A file referenced at function granularity must fail for the missing FUNCTION too,
# not only for a missing file.
d=$(fixture)
python3 - "$d" <<'PY'
import re, sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
s = s.replace("fn secret_like_names(", "fn secret_like_names_RENAMED(", 1)
p.write_text(s)
PY
expect_status "SECFIX3-02 renaming the named #[test] function exits non-zero" "$d" 1 \
    "BND-14: crates/core/tests/protected_spec.rs has no \`#[test] fn secret_like_names\`"
rm -rf "$d"

# --- SECFIX3-03: a new obligation whose test was never written must fail ----------
#
# Both halves matter: the identifier must also be referenced by a threat, so that
# the *target* check is what fires rather than the orphan check.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
root = Path(sys.argv[1])
p = root / "docs/TESTING.md"
s = p.read_text()
s = s.replace(
    "| BND-01 | `..` traversal",
    "| BND-98 | A brand new obligation whose test was never written | unit | M1 | "
    "`crates/core/tests/no_such_file_spec.rs::never_written` |\n| BND-01 | `..` traversal",
    1,
)
p.write_text(s)
q = root / "docs/SECURITY-MODEL.md"
t = q.read_text()
t = t.replace("| BND-01, BND-02, BND-12, BND-16, BND-17 |",
              "| BND-01, BND-02, BND-12, BND-16, BND-17, BND-98 |", 1)
q.write_text(t)
PY
expect_status "SECFIX3-03 a new obligation with no test exits non-zero" "$d" 1 \
    "BND-98: test file crates/core/tests/no_such_file_spec.rs does not exist"
rm -rf "$d"

# --- SECFIX3-04: the pristine tree passes -----------------------------------------
d=$(fixture)
expect_status "SECFIX3-04 the unmodified tree passes" "$d" 0 "matrix check passed"
run "$d"
baseline_rows=$(printf '%s' "$out" | sed -n 's/.*: \([0-9]*\) catalogue rows.*/\1/p')
baseline_targets=$(printf '%s' "$out" | sed -n 's/.*; \([0-9]*\) targets resolved on disk.*/\1/p')
rm -rf "$d"

# --- SECFIX3-05: the numbers the script prints are scanned, not written down ------
#
# Rather than re-derive the counts here (which would only test that this file and
# the script agree on how to count), change the tree and watch the numbers move.
# A hard-coded number cannot move.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TESTING.md"
s = p.read_text()
# A real, resolvable row pointing at a test that exists, so only the COUNTS move.
s = s.replace(
    "| BND-01 | `..` traversal",
    "| BND-97 | A resolvable obligation added to move the counters | unit | M1 | "
    "`crates/core/tests/protected_spec.rs::ordinary_files_are_not_protected` |\n"
    "| BND-01 | `..` traversal",
    1,
)
p.write_text(s)
q = Path(sys.argv[1]) / "docs/SECURITY-MODEL.md"
t = q.read_text()
q.write_text(t.replace("| BND-01, BND-02, BND-12, BND-16, BND-17 |",
                       "| BND-01, BND-02, BND-12, BND-16, BND-17, BND-97 |", 1))
PY
run "$d"
added_rows=$(printf '%s' "$out" | sed -n 's/.*: \([0-9]*\) catalogue rows.*/\1/p')
added_targets=$(printf '%s' "$out" | sed -n 's/.*; \([0-9]*\) targets resolved on disk.*/\1/p')
if [ "$status" -ne 0 ]; then
    no "SECFIX3-05 printed counts move when the catalogue grows" "exit $status
$out"
elif [ -z "$baseline_rows" ] || [ "$added_rows" -ne $((baseline_rows + 1)) ]; then
    no "SECFIX3-05 printed counts move when the catalogue grows" \
        "catalogue rows printed '$baseline_rows' then '$added_rows', expected +1"
elif [ -z "$baseline_targets" ] || [ "$added_targets" -ne $((baseline_targets + 1)) ]; then
    no "SECFIX3-05 printed counts move when the catalogue grows" \
        "targets resolved printed '$baseline_targets' then '$added_targets', expected +1"
else
    ok "SECFIX3-05 printed counts move when the catalogue grows ($baseline_rows -> $added_rows rows, $baseline_targets -> $added_targets targets)"
fi
rm -rf "$d"

# And they move DOWN when a target stops resolving: remove the file, expect the
# resolved-target count to fall while the script fails.
d=$(fixture)
rm "$d/crates/core/tests/protected_spec.rs"
run "$d"
dropped=$(printf '%s' "$out" | sed -n 's/.*; \([0-9]*\) targets resolved on disk.*/\1/p')
if [ "$status" -eq 0 ]; then
    no "SECFIX3-05b resolved-target count falls when a target stops resolving" "exit 0
$out"
elif [ -z "$dropped" ] || [ "$dropped" -ge "$baseline_targets" ]; then
    no "SECFIX3-05b resolved-target count falls when a target stops resolving" \
        "targets resolved printed '$baseline_targets' then '$dropped', expected fewer"
else
    ok "SECFIX3-05b resolved-target count falls when a target stops resolving ($baseline_targets -> $dropped)"
fi
rm -rf "$d"

# SECFIX3-06 .. SECFIX3-09 — F1, F2 and F4 from CR round 1.
#
# F1: the `#[test]` in the `::fn` check was matched by `(?:...)*`, where `*` is
#     zero-or-more, so the attribute was OPTIONAL. Replacing `#[test] fn x` with
#     `#[allow(dead_code)] fn x` kept the matrix green and clippy green while cargo
#     stopped running the test. A test that has quietly stopped running is worse than
#     a missing one: the catalogue says it is there.
#
# F2: a Target was only checked with `.exists()`, so a row could point at product
#     source, a directory, or a document and still pass. The column says "test file",
#     so the script has to check WHAT it is, not that a path answers.
#
# F4: the old SECFIX3-06 grepped the script for two strings, so disabling the check
#     while leaving the strings in a comment still reported "ok". It is replaced below
#     by a mutation: turn the checks off in a COPY and require the scenarios to stop
#     failing. That is a statement about behaviour, and it cannot be satisfied by a
#     string that happens to survive in a comment.

# --- SECFIX3-07 (F1): dropping `#[test]` must make the matrix red --------------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
# `#[test]` -> an unrelated attribute: the function still exists, still compiles, is
# still lint-clean, and cargo no longer runs it.
s = s.replace("#[test]\nfn secret_like_names(", "#[allow(dead_code)]\nfn secret_like_names(", 1)
p.write_text(s)
PY
expect_status "SECFIX3-07 a target whose #[test] was dropped exits non-zero" "$d" 1 \
    "BND-14"
# And the teeth: the same fixture must still be green with the attribute intact, so
# SECFIX3-07 is not passing because the fixture is broken in some other way.
d2=$(fixture)
expect_status "SECFIX3-07b the same fixture with #[test] intact passes" "$d2" 0 \
    "matrix check passed"
rm -rf "$d" "$d2"

# --- SECFIX3-08 (F2): a Target must be the KIND of thing the column claims ----------
for bad in "crates/core/src/boundary.rs:product source" \
           "crates/core/src:a directory" \
           "docs/TESTING.md:this document" \
           "crates/core/tests:the tests directory"; do
    target=${bad%%:*}
    what=${bad#*:}
    d=$(fixture)
    python3 - "$d" "$target" <<'PY'
import sys
from pathlib import Path
root, target = Path(sys.argv[1]), sys.argv[2]
p = root / "docs/TESTING.md"
s = p.read_text()
# Repoint BND-01, an ordinary row, at something that exists but is not a test file.
s = s.replace(
    "| BND-01 | `..` traversal (plain, nested, mixed separators, encoded) is refused for "
    "read and write | unit+golden | M1 | `crates/core/tests/boundary_spec.rs::"
    "bnd01_traversal_refused` |",
    f"| BND-01 | `..` traversal | unit+golden | M1 | `{target}` |",
    1,
)
p.write_text(s)
PY
    expect_status "SECFIX3-08 a Target pointing at $what exits non-zero" "$d" 1 \
        "not a place this repository keeps tests"
    rm -rf "$d"
done

# --- SECFIX3-11: a Target may point anywhere, as long as the named function is a test --
#
# CR round 3. The rule was "a Target must be crates/<crate>/tests/<name>.rs", which made
# the check NARROWER than the property it guards - the same shape as F1-FP. SEC-FIX 4 moved
# apply_spec.rs, undo_spec.rs, edit7_extra_spec.rs and secfix4_write_cap_spec.rs into
# crates/edit/src/spec/ (they need `crate::` internals to mint a WriteCap); cargo still runs
# them - 18 `#[test]`s in apply_spec.rs, 18 in undo_spec.rs - and all 14 EDT rows went red as
# FALSE REDS.
#
# The judge is semantic and always was: does that file contain a function of that name that
# CARRIES #[test]? A file that does is the test, wherever it lives.
#
# These cases build the situation on a tree that does NOT have the relocation, so the fix is
# exercised here rather than being taken on trust from a merge that "looked green".

# A `#[cfg(test)] mod` living under src/, exactly like SEC-FIX 4's layout.
make_src_spec() {
    d=$1
    mkdir -p "$d/crates/core/src/spec"
    cat > "$d/crates/core/src/spec/moved_spec.rs" <<'MARKER'
#![allow(dead_code)]
//! A test module that lives inside the crate because it needs `crate::` internals.
use std::fs;

fn helper() -> &'static str {
    "helper"
}

#[test]
fn moved_spec_a_passes() {
    assert_eq!(helper(), "helper");
}

#[test]
fn moved_spec_b_also_passes() {
    assert!(fs::metadata(".").is_ok());
}
MARKER
}

point_at() {
    # point_at <fixture> <target>
    python3 - "$1" "$2" <<'MARKER'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TESTING.md"
s = p.read_text()
old_row = [l for l in s.splitlines() if l.startswith("| BND-01 |")]
assert old_row, "BND-01 row not found"
s = s.replace(old_row[0], f"| BND-01 | `..` traversal | unit | M1 | `{sys.argv[2]}` |", 1)
p.write_text(s)
MARKER
}

# 1. A ::fn target inside src/spec/ must PASS.
d=$(fixture); make_src_spec "$d"
point_at "$d" "crates/core/src/spec/moved_spec.rs::moved_spec_a_passes"
expect_status "SECFIX3-11 a ::fn target under src/ passes" "$d" 0 "matrix check passed"
rm -rf "$d"

# 2. Same, with the second test, so the check is not accidentally name-specific.
d=$(fixture); make_src_spec "$d"
point_at "$d" "crates/core/src/spec/moved_spec.rs::moved_spec_b_also_passes"
expect_status "SECFIX3-11 the other test in the same file passes too" "$d" 0 "matrix check passed"
rm -rf "$d"

# 3. Drop the #[test] from that module -> the SAME target must FAIL.
d=$(fixture); make_src_spec "$d"
sed -i 's/^#\[test\]$/#\[allow(dead_code)\]/' "$d/crates/core/src/spec/moved_spec.rs"
point_at "$d" "crates/core/src/spec/moved_spec.rs::moved_spec_a_passes"
expect_status "SECFIX3-11b a ::fn under src/ whose #[test] was dropped exits non-zero" "$d" 1 \
    "BND-01"
rm -rf "$d"

# 4. A BARE target at that location is still fine (src/spec is a place tests live)...
d=$(fixture); make_src_spec "$d"
point_at "$d" "crates/core/src/spec/moved_spec.rs"
expect_status "SECFIX3-11 a bare target under src/spec passes" "$d" 0 "matrix check passed"
rm -rf "$d"

# 5. ...but product source under src/ is still refused, bare AND with a ::fn.
d=$(fixture); point_at "$d" "crates/core/src/boundary.rs"
expect_status "SECFIX3-11c product source bare under src/ exits non-zero" "$d" 1 \
    "not a place this repository keeps tests"
rm -rf "$d"
d=$(fixture); point_at "$d" "crates/core/src/boundary.rs::resolve_read"
expect_status "SECFIX3-11c a ::fn naming a non-test in product source exits non-zero" "$d" 1 \
    "has no `#[test] fn resolve_read`"
rm -rf "$d"

# --- SECFIX3-06 (F4, rewritten): a mutation, not a string search --------------------
#
# Copy the script, switch OFF one check, apply the scenario that check is supposed to
# catch, and require that the scenario now PASSES. If it still failed, something else
# would be catching it and SECFIX3-07/-08 would be proving nothing about the check they
# name.
#
# Each case mutates AND breaks the tree together. Mutating the script on a pristine tree
# would exit 0 trivially and prove nothing - which is the same mistake F4 was about, so
# it is worth stating rather than leaving to the reader.

# Repoint BND-01 at product source, BARE (no ::fn) - guarded by `is_test_file(target)`.
break_target_at_source() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TESTING.md"
s = p.read_text()
old_row = [l for l in s.splitlines() if l.startswith("| BND-01 |")]
assert old_row, "BND-01 row not found"
s = s.replace(
    old_row[0],
    "| BND-01 | `..` traversal | unit+golden | M1 | `crates/core/src/boundary.rs` |",
    1,
)
p.write_text(s)
PYEOF
}

# Repoint BND-01 at a `::fn` inside product source - guarded by `is_test_file(rel)`.
# `boundary.rs::resolve_read` really exists, so this target is only wrong in KIND: the
# function is real, it is just not a test.
break_fn_target_at_source() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TESTING.md"
s = p.read_text()
old_row = [l for l in s.splitlines() if l.startswith("| BND-01 |")]
assert old_row, "BND-01 row not found"
s = s.replace(
    old_row[0],
    "| BND-01 | `..` traversal | unit+golden | M1 | "
    "`crates/core/src/boundary.rs::resolve_read` |",
    1,
)
p.write_text(s)
PYEOF
}

# Drop the #[test] attribute from a named test: the thing SECFIX3-07 catches.
drop_test_attr() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
assert "#[test]\nfn secret_like_names(" in s, "anchor test not found"
p.write_text(s.replace(
    "#[test]\nfn secret_like_names(",
    "#[allow(dead_code)]\nfn secret_like_names(", 1))
PYEOF
}

mutate() {
    python3 - "$1" "$2" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "scripts/check-matrix.sh"
s = p.read_text()
old = sys.argv[2]
assert old in s, f"mutation anchor missing: {old}"
p.write_text(s.replace(old, "if False:  # MUTATED", 1))
PYEOF
}

# Sanity first, so a later "exit 0" cannot be a broken fixture in disguise: both
# scenarios must FAIL on the unmutated script.
d=$(fixture); break_target_at_source "$d"
expect_status "SECFIX3-06 sanity: source-pointer fails before the mutation" "$d" 1 "not a place this repository keeps tests"
rm -rf "$d"
d=$(fixture); drop_test_attr "$d"
expect_status "SECFIX3-06 sanity: dropped #[test] fails before the mutation" "$d" 1 "BND-14"
rm -rf "$d"
d=$(fixture); break_fn_target_at_source "$d"
expect_status "SECFIX3-06 sanity: ::fn into source fails before the mutation" "$d" 1 "has no `#[test] fn resolve_read`"
rm -rf "$d"

# Bare path -> guarded by is_test_file(target). ::fn -> guarded by is_test_file(rel).
# Pairing a mutation with the wrong scenario is the trap: the suite caught that once, so
# the pairing is explicit rather than looped.
d=$(fixture)
mutate "$d" 'if not is_test_file(target):'
break_target_at_source "$d"
run "$d"
if [ "$status" -ne 0 ]; then
    no "SECFIX3-06 mutating \`is_test_file(target)\` removes the teeth" \
       "the scenario still failed with the check disabled, so it was not this check:
$out"
else
    ok "SECFIX3-06 mutating \`is_test_file(target)\` removes the teeth (scenario now exit 0)"
fi
rm -rf "$d"

# The `::fn` branch's ONLY check is `has_test_attr` - that is the whole point of CR round 3:
# the semantic criterion replaces the path rule, because a path rule is narrower than the
# property it guards. So the mutation that matters here is `has_test_attr`, exercised against
# a `src/spec/` target - the case that must NOT false-red. Disabling it must turn
# SECFIX3-11b's scenario green.
d=$(fixture); make_src_spec "$d"
mutate "$d" 'if not has_test_attr(p.read_text(encoding="utf-8"), fn):'
sed -i 's/^#\[test\]$/#\[allow(dead_code)\]/' "$d/crates/core/src/spec/moved_spec.rs"
point_at "$d" "crates/core/src/spec/moved_spec.rs::moved_spec_a_passes"
run "$d"
if [ "$status" -ne 0 ]; then
    no "SECFIX3-06 mutating has_test_attr silences the only check the ::fn branch has" \
       "SECFIX3-11b's scenario still failed with has_test_attr disabled:
$out"
else
    ok "SECFIX3-06 mutating has_test_attr silences the ::fn check (SECFIX3-11b scenario now exit 0)"
fi
rm -rf "$d"


d=$(fixture)
mutate "$d" 'if not has_test_attr(p.read_text(encoding="utf-8"), fn):'
drop_test_attr "$d"
run "$d"
if [ "$status" -ne 0 ]; then
    no "SECFIX3-06 mutating the #[test] check removes the teeth" \
       "SECFIX3-07's scenario still failed with has_test_attr disabled:
$out"
else
    ok "SECFIX3-06 mutating the #[test] check removes the teeth (SECFIX3-07 scenario now exit 0)"
fi
rm -rf "$d"

# --- SECFIX3-10 (CR round 2, F1-FP): five legitimate #[test] spellings ---------------
#
# F1 was fixed by making `#[test]` REQUIRED, but the check that did it only walked up
# while a line began with `#[` and only accepted a line that WAS `#[test]`. So five
# spellings that cargo really runs were reported as "no test here":
#
#     #[test] #[ignore]                     (two attributes, one line)
#     #[cfg(unix)] #[test]                  (two attributes, one line)
#     #[test]\n// comment\nfn x           (a comment between them)
#     #[test]\n\nfn x                     (a blank line between them)
#     #[test] fn x() {}                     (all on one line)
#
# That is a FALSE RED, and a false red is as corrosive as a false green: the gate says
# something untrue, so people learn to route around it. This repository already has five
# `#[ignore]`s and eleven `#[cfg(...)]`-with-`#[test]` pairs - all written one-per-line,
# so none of them had tripped it yet. Latent, not hypothetical.
#
# Every case here must PASS. The two on the other side of the same coin - the attribute
# removed entirely, and replaced by something inert - must still FAIL.

spell() {
    # spell <name> <attribute lines, one per line, \n separated> <body>
    name=$1; attrs=$2; body=$3
    d=$(fixture)
    python3 - "$d" "$attrs" "$body" <<'PYEOF'
import sys
from pathlib import Path
attrs, body = sys.argv[2], sys.argv[3]
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
old = "#[test]\nfn secret_like_names("
assert old in s, "anchor test not found"
p.write_text(s.replace(old, attrs + "\n" + body + "fn secret_like_names(", 1))
PYEOF
    expect_status "SECFIX3-10 $name" "$d" 0 "matrix check passed"
    rm -rf "$d"
}

spell "#[test] #[ignore] on one line"          '#[test] #[ignore]'                  'fn secret_like_names('
spell "#[cfg(unix)] #[test] on one line"       '#[cfg(unix)] #[test]'               'fn secret_like_names('
spell "a // comment between attribute and fn"  '#[test]\n// why this exists'        'fn secret_like_names('
spell "a blank line between attribute and fn"  '#[test]\n'                          'fn secret_like_names('
spell "#[test] fn x() on one line"             '#[test] fn secret_like_names('       '{ let _ = ();'
spell "attribute and signature split over lines" '#[test]\n#[allow(dead_code)]'      'fn secret_like_names('
spell "doc comment between attribute and fn"   '#[test]\n/// # Examples'            'fn secret_like_names('

# And the direction that must NOT regress: the attribute gone, or inert.
drop_attr() {
    d=$(fixture)
    python3 - "$d" "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
assert "#[test]\nfn secret_like_names(" in s
p.write_text(s.replace("#[test]\nfn secret_like_names(", sys.argv[2] + "fn secret_like_names(", 1))
PYEOF
    expect_status "SECFIX3-10b attribute replaced by \`$1\` exits non-zero" "$d" 1 "BND-14"
    rm -rf "$d"
}
drop_attr "#[allow(dead_code)]
"
drop_attr "#[cfg(feature = \"never\")]
"

# A test whose name appears only inside a COMMENT must not satisfy the check.
d=$(fixture)
python3 - "$d" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/tests/protected_spec.rs"
s = p.read_text()
old = "#[test]\nfn secret_like_names("
assert old in s
s = s.replace(old, "#[test]\nfn secret_like_names_RENAMED(", 1)
# Put the old name back where a reader would plausibly find it, in prose only.
s = "// see also: #[test] fn secret_like_names() in an older revision\n" + s
p.write_text(s)
PYEOF
expect_status "SECFIX3-10c a name that only appears in a comment exits non-zero" "$d" 1 "BND-14"
rm -rf "$d"

# --- KIND-01..05: a row's Kind may not claim a dimension its target cannot reach ---
#
# The defect is ISSUE-TESTING-MD-KIND-PAT-06-RUST: `docs/TESTING.md` labelled rows
# `golden per language`, `property` and "across operating systems" while the cited
# test could demonstrate none of it. These cases plant each drift and require a
# non-zero exit for the RIGHT reason, and each is paired with a mutation that
# switches the rule off - because "it failed" and "this rule is what made it fail"
# are different claims.

kind_case() {
    # kind_case <name> <old-row-prefix> <new-row> [must-contain]
    name=$1; old=$2; new=$3; needle=${4:-}
    d=$(fixture)
    python3 - "$d" "$old" "$new" <<'PYEOF'
import sys
from pathlib import Path
root, old, new = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
p = root / "docs/TESTING.md"
s = p.read_text()
row = [l for l in s.splitlines() if l.startswith(old)]
assert row, f"row starting {old!r} not found"
p.write_text(s.replace(row[0], new, 1))
PYEOF
    expect_status "$name" "$d" 1 "$needle"
    rm -rf "$d"
}

# KIND-01: `per language` backed by a single-language fixture set.
# PAT-09 is genuinely six-language now, so this repoints a row at a test that
# names only Rust and keeps the `per language` kind.
kind_case "KIND-01 a per-language row citing one language exits non-zero" \
    "| EDT-13 |" \
    "| EDT-13 | The syntax gate refuses edits that add syntax errors | golden per language | M4 | \`crates/edit/src/spec/apply_spec.rs::file_properties_are_preserved\` |" \
    "reaches only"

# KIND-02: a `property` backed by exactly one fixed example.
kind_case "KIND-02 a property row whose only test is one fixed example exits non-zero" \
    "| PAT-07 |" \
    "| PAT-07 | Metavariable expansion can never alter structure outside the replacement | property | M3 | \`crates/edit/tests/template_spec.rs::verbatim_ranges_are_copied_exactly\` |" \
    "property"

# KIND-03: an OS-independence claim with no per-platform evidence. The cited test
# is `crates/query/tests/pattern_match_spec.rs`, which carries no `cfg(...)`, so
# nothing in the row can observe a platform.
kind_case "KIND-03 an OS claim with no per-platform evidence exits non-zero" \
    "| PAT-06 |" \
    "| PAT-06 | Results are identical across operating systems | e2e per OS | M3 | \`crates/query/tests/pattern_match_spec.rs::rust_patterns\` |" \
    "can observe a platform"

# KIND-04: the same, on a `per OS` row whose target is a plain ungated test.
kind_case "KIND-04 a per-OS row citing an ungated test exits non-zero" \
    "| PAT-06 |" \
    "| PAT-06 | Mode bits are preserved on every platform | e2e per OS | M3 | \`crates/query/tests/pattern_match_spec.rs::rust_patterns\` |" \
    "can observe a platform"

# KIND-05: a `property` row with NO target at all - the degenerate drift, where the
# claim is quantified by nothing.
kind_case "KIND-05 a property row with no quantified test exits non-zero" \
    "| PAT-07 |" \
    "| PAT-07 | Metavariable expansion can never produce an edit outside its match | property | M3 | \`crates/edit/src/spec/apply_spec.rs::file_properties_are_preserved\` |" \
    "property"

# --- the teeth: each rule switched off must turn its own case GREEN ----------------
#
# A rule that cannot be shown to fail for the right reason is not known to be the
# rule doing the work. Each mutation AND its scenario are applied to the same
# fixture; mutating the script on a pristine tree would exit 0 trivially and prove
# nothing.

k01_off() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "scripts/check-matrix.sh"
s = p.read_text()
old = '    if claims_per_language and fn_targets:'
assert old in s, "per-language anchor missing"
p.write_text(s.replace(old, "    if False:  # MUTATED", 1))
PYEOF
}
k02_off() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "scripts/check-matrix.sh"
s = p.read_text()
old = "    if claims_property and fn_targets:"
assert old in s, "property anchor missing"
p.write_text(s.replace(old, "    if False:  # MUTATED", 1))
PYEOF
}
k03_off() {
    python3 - "$1" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "scripts/check-matrix.sh"
s = p.read_text()
old = '    if (claims_per_os or claims_per_language) and targets and not ci_targets:'
assert old in s, "platform anchor missing"
p.write_text(s.replace(old, "    if False:  # MUTATED", 1))
PYEOF
}

teeth() {
    # teeth <name> <mutator-fn> <old-row-prefix> <new-row>
    name=$1; mut=$2; old=$3; new=$4
    d=$(fixture)
    python3 - "$d" "$old" "$new" <<'PYEOF'
import sys
from pathlib import Path
root, old, new = Path(sys.argv[1]), sys.argv[2], sys.argv[3]
p = root / "docs/TESTING.md"
s = p.read_text()
row = [l for l in s.splitlines() if l.startswith(old)]
assert row, f"row starting {old!r} not found"
p.write_text(s.replace(row[0], new, 1))
PYEOF
    "$mut" "$d"
    run "$d"
    if [ "$status" -ne 0 ]; then
        no "$name" "the scenario still failed with the rule disabled, so this rule is not \
what catches it:
$out"
    else
        ok "$name"
    fi
    rm -rf "$d"
}

teeth "KIND-01 teeth mutating the per-language rule removes the teeth" k01_off \
    "| EDT-13 |" \
    "| EDT-13 | The syntax gate refuses edits that add syntax errors | golden per language | M4 | \`crates/edit/src/spec/apply_spec.rs::file_properties_are_preserved\` |"

teeth "KIND-02 teeth mutating the property rule removes the teeth" k02_off \
    "| PAT-07 |" \
    "| PAT-07 | Metavariable expansion can never alter structure outside the replacement | property | M3 | \`crates/edit/tests/template_spec.rs::verbatim_ranges_are_copied_exactly\` |"

teeth "KIND-03 teeth mutating the platform rule removes the teeth" k03_off \
    "| PAT-06 |" \
    "| PAT-06 | Results are identical across operating systems | e2e per OS | M3 | \`crates/query/tests/pattern_match_spec.rs::rust_patterns\` |"

# --- the anti-theatre cases: legitimate rows must NOT be red ------------------------
#
# A false red is as corrosive as a false green: the gate says something untrue, so
# people learn to walk past it. These pin the rules' NARROWER-than-naive boundary,
# and each is a case that was a false red while this check was being written.

# A `property` row whose test IS quantified over a corpus must pass. BND-16 drives
# `fuzz::run_cases`, and a naive "one test is never a property" rule would have
# failed it - along with eight other rows.
d=$(fixture)
expect_status "KIND-06 a property row quantified over a corpus still passes" "$d" 0 \
    "matrix check passed"
rm -rf "$d"

# A `per OS` row whose test file is `#![cfg(unix)]` passes: it says per-OS by being
# per-OS. The first version of the rule read only the first 400 characters of the
# file and false-reded EDT-14 and EDT-28, whose gates sit at line 9 and line 13.
d=$(fixture)
python3 - "$d" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TESTING.md"
s = p.read_text()
row = [l for l in s.splitlines() if l.startswith("| EDT-14 |")]
assert row, "EDT-14 row not found"
# keep the kind, point at a cfg-gated file (crates/core/tests/fsio_spec.rs is
# `#![cfg(unix)]` and its test really exists)
new = "| EDT-14 | Mode bits, BOM, CRLF/LF style, trailing newline and indentation are preserved | e2e per OS | M4 | `crates/core/tests/fsio_spec.rs::refuses_hard_linked_readonly_and_symlink_targets` |"
p.write_text(s.replace(row[0], new, 1))
PYEOF
expect_status "KIND-07 a per-OS row citing a cfg-gated test passes" "$d" 0 \
    "matrix check passed"
rm -rf "$d"

# A `per language` row whose test names languages by FILE EXTENSION passes: that is
# how the EDT-13 and PAT-10 case tables are written, and a rule that only accepted
# `Language::Rust` would false-red both.
d=$(fixture)
expect_status "KIND-08 a per-language row whose test names languages as extensions passes" "$d" 0 \
    "matrix check passed"
rm -rf "$d"

# A `property` row that cites BOTH an unquantified golden AND a quantified property
# test passes. Requiring every target to be quantified false-reded PAT-10, whose
# evidence was already sound.
d=$(fixture)
expect_status "KIND-09 a property row with one quantified target among several passes" "$d" 0 \
    "matrix check passed"
rm -rf "$d"

# The language list is DERIVED from the tree, so it cannot drift. Adding a variant
# to `Language` while the tests do not cover it must make the per-language rows red -
# if the check had hard-coded the six, this would stay green.
d=$(fixture)
python3 - "$d" <<'PYEOF'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/lang/src/language.rs"
s = p.read_text()
old = "    /// Go (`.go`).\n    Go,"
assert old in s, "Go variant anchor missing"
# A new language must be added BOTH to the enum and to `Language::all()`; the
# check reads `all()` first, so adding only the variant would not be seen. Adding
# it in both places is what a real language addition looks like.
p.write_text(s.replace(old, old + "\n    /// Zig (`.zig`).\n    Zig,", 1))
p.write_text(s.replace("            Language::Go,\n        ]", "            Language::Go,\n            Language::Zig,\n        ]", 1))
q = Path(sys.argv[1]) / "docs/TESTING.md"
t = q.read_text()
q.write_text(t.replace("| PAT-09 |", "| PAT-09 |", 1))
PYEOF
expect_status "KIND-10 adding a language to Language reds the per-language rows" "$d" 1 \
    "Zig"
rm -rf "$d"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
