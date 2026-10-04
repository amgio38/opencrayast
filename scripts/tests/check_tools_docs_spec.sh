#!/bin/sh
# The TOOLS.md <-> registry.rs self-test.
#
# These run the REAL scripts/check-tools-docs.sh against a COPY of the tree, so a
# planted defect cannot damage the checkout it was started from. Each case makes
# exactly one change and asserts on the exit status AND on the message, because
# "it failed" and "it failed for the right reason" are different claims: a check
# that goes red for an unrelated parse error looks identical to a check that works.
#
# The drift is planted in BOTH directions for each axis, because a one-directional
# check catches half of it:
#
#   TDDOC-01  a tool in the registry with no section in TOOLS.md
#   TDDOC-02  a section in TOOLS.md for a tool that is not registered
#   TDDOC-03  a mode that disagrees (doc says read, registry says Write)
#   TDDOC-04  an annotation that disagrees (destructiveHint)
#   TDDOC-05  a schema property the argument table does not document
#   TDDOC-06  an argument the schema does not accept
#   TDDOC-07  an ErrorCode variant with no row in the error table
#   TDDOC-08  an error code documented that no variant produces
#   TDDOC-09  the unmodified tree passes (the case that makes the others mean
#             something: a check that always fails "catches" every drift)
#
# Run directly (`sh scripts/tests/check_tools_docs_spec.sh`) or from CI. `cargo
# test` does not execute it: it needs python3 and a writable copy of the tree.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)
script_rel=scripts/check-tools-docs.sh

pass=0
fail=0

# A fresh copy of the tree, minus target/ and .git so it is cheap.
fixture() {
    d=$(mktemp -d)
    for p in docs scripts crates; do
        [ -e "$repo/$p" ] && cp -R "$repo/$p" "$d/"
    done
    rm -rf "$d/crates"/*/target 2>/dev/null || true
    echo "$d"
}

run() {
    d=$1
    set +e
    out=$(cd "$d" && bash "$script_rel" 2>&1)
    status=$?
    set -e
}

ok() {
    pass=$((pass + 1))
    printf '  ok   %s\n' "$1"
}
no() {
    fail=$((fail + 1))
    printf '  FAIL %s\n' "$1"
    printf '%s\n' "$2" | sed 's/^/         /'
}

expect_status() {
    # expect_status <name> <fixture> <wanted-status> [must-contain]
    name=$1
    d=$2
    want=$3
    needle=${4:-}
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

echo "check-tools-docs.sh (self-test)"

# --- TDDOC-09: the clean tree must pass, or every case below proves nothing ----
d=$(fixture)
expect_status "TDDOC-09 the unmodified tree passes" "$d" 0 "in both directions"
rm -rf "$d"

# --- TDDOC-01: a registered tool with no section must fail ---------------------
#
# Renaming the tool in the registry alone is enough: the doc still documents the old
# name, so the registry has a tool the doc does not, AND the doc has a section the
# registry does not. This is the drift the audit was about.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/tools/src/registry.rs"
s = p.read_text()
# Add a twelfth entry at the head of the CATALOG.
anchor = "const CATALOG: [ToolEntry; 11] = [\n"
assert anchor in s, "CATALOG declaration not found; update this case"
s = s.replace(
    anchor,
    "const CATALOG: [ToolEntry; 12] = [\n"
    "    ToolEntry {\n"
    '        name: "ast_newfangled",\n'
    '        description: "A tool the document has never heard of.",\n'
    "        mode: Mode::ReadOnly,\n"
    "        annotations: READ,\n"
    "        input_schema: EMPTY_OBJECT,\n"
    "    },\n",
    1,
)
p.write_text(s)
PY
expect_status "TDDOC-01 an undocumented tool in the registry exits non-zero" "$d" 1 \
    "ast_newfangled"
rm -rf "$d"

# --- TDDOC-02: a documented tool that is not registered must fail --------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TOOLS.md"
s = p.read_text()
anchor = "## `ast_recover` *(write mode)*"
assert anchor in s, "ast_recover heading not found"
s = s.replace(anchor,
              "## `ast_ghost`\n\nNot a real tool.\n\n## `ast_recover` *(write mode)*", 1)
p.write_text(s)
PY
expect_status "TDDOC-02 a section for an unregistered tool exits non-zero" "$d" 1 \
    "ast_ghost"
rm -rf "$d"

# --- TDDOC-03: a mode that disagrees must fail ---------------------------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TOOLS.md"
s = p.read_text()
anchor = "| `ast_undo` | **write** |"
assert anchor in s, "ast_undo row not found"
s = s.replace(anchor, "| `ast_undo` | read |", 1)
p.write_text(s)
PY
expect_status "TDDOC-03 a mode mismatch exits non-zero" "$d" 1 \
    "mode is documented as 'read'"
rm -rf "$d"

# --- TDDOC-04: an annotation that disagrees must fail --------------------------
#
# `ast_recover` is destructive but idempotent; flipping the registry to a
# non-idempotent const changes exactly one boolean.
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/tools/src/registry.rs"
s = p.read_text()
s = s.replace("        annotations: WRITE_RECOVER,", "        annotations: WRITE_DESTRUCTIVE,", 1)
p.write_text(s)
PY
expect_status "TDDOC-04 an annotation mismatch exits non-zero" "$d" 1 \
    "idempotent_hint"
rm -rf "$d"

# --- TDDOC-05: a schema property the doc does not document must fail -----------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/tools/src/registry.rs"
s = p.read_text()
anchor = '    "include_docs": { "type": "boolean" },\n'
assert anchor in s, "anchor for OUTLINE_SCHEMA not found"
s = s.replace(
    anchor,
    anchor + '    "undocumented_flag": { "type": "boolean" },\n',
    1,
)
p.write_text(s)
PY
expect_status "TDDOC-05 an undocumented schema property exits non-zero" "$d" 1 \
    "undocumented_flag"
rm -rf "$d"

# --- TDDOC-06: an argument the schema refuses must fail ------------------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TOOLS.md"
s = p.read_text()
anchor = "| `limit` | integer 1..= `limits.max_results` | 100 |"
assert anchor in s, "ast_search limit row not found"
s = s.replace(anchor, anchor + "\n"
              "| `not_a_real_arg` | string | – | Documented but never accepted |", 1)
p.write_text(s)
PY
expect_status "TDDOC-06 a documented argument the schema rejects exits non-zero" "$d" 1 \
    "not_a_real_arg"
rm -rf "$d"

# --- TDDOC-07: an ErrorCode with no documented row must fail -------------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "crates/core/src/error.rs"
s = p.read_text()
anchor = "    /// A defect; never expected.\n    Internal,"
assert anchor in s, "Internal variant not found"
s = s.replace(anchor,
              "    /// A defect; never expected.\n    Internal,\n"
              "    /// A code nobody documents yet.\n    SomeNewCode,", 1)
p.write_text(s)
PY
expect_status "TDDOC-07 an undocumented ErrorCode variant exits non-zero" "$d" 1 \
    "some_new_code"
rm -rf "$d"

# --- TDDOC-08: a documented code no variant produces must fail -----------------
d=$(fixture)
python3 - "$d" <<'PY'
import sys
from pathlib import Path
p = Path(sys.argv[1]) / "docs/TOOLS.md"
s = p.read_text()
anchor = "| `internal` | A defect; never expected |"
assert anchor in s, "internal row not found"
s = s.replace(anchor,
              "| `no_such_code` | Documented but no variant produces it |\n"
              "| `internal` | A defect; never expected |", 1)
p.write_text(s)
PY
expect_status "TDDOC-08 an invented error code exits non-zero" "$d" 1 \
    "no_such_code"
rm -rf "$d"

echo "check-tools-docs.sh: $pass passed, $fail failed"
[ "$fail" -eq 0 ]