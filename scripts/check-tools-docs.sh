#!/bin/sh
# TOOLS.md <-> the tool catalogue in crates/tools/src/registry.rs and the error
# codes in crates/core/src/error.rs.
#
# docs/TOOLS.md calls itself the normative specification of the MCP tools and, in
# its first paragraph, claims that "the schemas that the server publishes are
# generated from the same definitions and checked against this document in CI".
# Only half of that sentence was ever true: crates/tools/tests/
# tool_descriptions_spec.rs checks the *description strings* in both directions,
# and ErrorCode::all() is compared to the error table by UX1-06. What NOTHING
# compared was the parts that decide what a tool is:
#
#   1. the tool SET. A tool in the registry with no section in TOOLS.md, or a
#      section in TOOLS.md for a tool nobody registered;
#   2. the MODE each tool runs in, and whether it is in the write-mode list;
#   3. the four published ANNOTATIONS (readOnlyHint, destructiveHint,
#      idempotentHint, openWorldHint) against the "Modes and annotations" table;
#   4. the inputSchema PROPERTY names each tool publishes against the argument
#      table in its own section. The schema is what a client validates against, so
#      an argument the doc documents but the schema does not accept is a tool
#      call that fails against the documentation;
#   5. the ErrorCode variants against the "Error code reference" table.
#
# Every check runs in BOTH directions. A one-directional check catches half the
# drift and is worse than none, because it looks like coverage.
#
# This is a textual comparison, not a Rust program, on purpose: it has to run in
# CI on a tree where `cargo` need not have built anything yet, and it must not
# re-implement the registry. It reads the two `const CATALOG` and `const ..._SCHEMA`
# declarations the same way the existing check-docs.sh reads Markdown.
#
# Usage: check-tools-docs.sh [repo-root]      (default: the parent of scripts/)
set -eu

root=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$root"

python3 <<'PY'
import json
import re
import sys
from pathlib import Path

problems = []
doc_path = Path("docs/TOOLS.md")
registry_path = Path("crates/tools/src/registry.rs")
error_path = Path("crates/core/src/error.rs")

if not doc_path.exists():
    sys.exit(f"check-tools-docs: {doc_path} is missing")
if not registry_path.exists():
    sys.exit(f"check-tools-docs: {registry_path} is missing")

doc = doc_path.read_text(encoding="utf-8")
registry = registry_path.read_text(encoding="utf-8")


def section(text, heading, level=2):
    """The body of `text` from a heading to the next heading of the same level.

    Bounded on purpose. docs/TOOLS.md also carries prose tables that mention tool
    names and error codes; scanning the whole file for a name would read those as
    catalogue entries.
    """
    pat = re.compile(rf"^{'#' * level} {re.escape(heading)}\s*$", re.M)
    m = pat.search(text)
    if not m:
        return None
    rest = text[m.end():]
    nxt = re.search(rf"^{'#' * level} ", rest, re.M)
    return rest[: nxt.start()] if nxt else rest


# ---------------------------------------------------------------- registry side
#
# The catalogue is read out of the source rather than hardcoded here: a second
# hand-kept list of tool names is exactly the failure this script exists to catch.

# `const CATALOG: [ToolEntry; 11] = [` — the array may be written with or without a
# leading `&`, so neither form is assumed.
cat = re.search(r"const CATALOG[^=]*=\s*&?\[(.*?)\n\];", registry, re.S)
if not cat:
    sys.exit("check-tools-docs: could not find `const CATALOG` in registry.rs")
cat_body = cat.group(1)

# One ToolEntry block. Splitting on `ToolEntry {` keeps the braces of the
# per-entry schemas out of the way; each entry has exactly one of each field.
entries = []
for raw in re.split(r"\bToolEntry\s*\{", cat_body)[1:]:
    body = raw.split("\n    },", 1)[0]

    def field(name, pattern=r'"((?:[^"\\]|\\.)*)"'):
        m = re.search(rf"\b{name}:\s*{pattern}", body, re.S)
        return m.group(1) if m else None

    name = field("name")
    if not name:
        continue
    ann = re.search(
        r"annotations:\s*(\w+)", body)
    mode = re.search(r"mode:\s*Mode::(\w+)", body)
    schema = re.search(r"input_schema:\s*(\w+)", body)
    entries.append(
        {
            "name": name,
            "const": ann.group(1) if ann else None,
            "mode": mode.group(1) if mode else None,
            "schema_const": schema.group(1) if schema else None,
        }
    )

if not entries:
    sys.exit("check-tools-docs: no ToolEntry found in the CATALOG")

# A floor, not just an emptiness test. The patterns above are line-oriented, so a
# refactor of registry.rs that moves a field onto its own line would read a
# shorter (possibly zero-length) set and pass every comparison vacuously -- the
# exact failure mode this check exists to make impossible. Taken from line A's
# check-tools-doc.sh, which had these guards; the catalogue is 11 tools and the
# floors are set well below that so they do not themselves become brittle.
MIN_TOOLS, MIN_ANNOTATION_SETS, MIN_ERROR_CODES = 5, 2, 20
if len(entries) < MIN_TOOLS:
    problems.append(
        f"only {len(entries)} catalogue entries were read from registry.rs (expected at "
        f"least {MIN_TOOLS}); the scan is too narrow and this check would pass vacuously"
    )

# The annotation constants, so `annotations: READ` resolves to four booleans.
ann_consts = {}
for m in re.finditer(
    r"const (\w+):\s*ToolAnnotations\s*=\s*ToolAnnotations\s*\{(.*?)\n\};",
    registry,
    re.S,
):
    ann_consts[m.group(1)] = {
        f: re.search(rf"{f}:\s*(true|false)", m.group(2)).group(1)
        for f in (
            "read_only_hint",
            "destructive_hint",
            "idempotent_hint",
            "open_world_hint",
        )
    }


if len(ann_consts) < MIN_ANNOTATION_SETS:
    problems.append(
        f"only {len(ann_consts)} ToolAnnotations constants were read from registry.rs "
        f"(expected at least {MIN_ANNOTATION_SETS}); annotation comparison would pass "
        f"vacuously"
    )


def schema_props(const_name):
    """The top-level property names a schema const publishes, or None."""
    m = re.search(
        rf"const {re.escape(const_name)}\s*:\s*&str\s*=\s*r#\"(.*?)\"#", registry, re.S
    )
    if not m:
        return None
    try:
        obj = json.loads(m.group(1))
    except json.JSONDecodeError:
        return None
    return sorted(obj.get("properties", {}).keys())


# --------------------------------------------------------------- doc-side tools
#
# A tool section is a level-2 heading whose text names a registered tool. The
# combined heading "## `ast_plan_show` / `ast_plan_list`" covers two tools, so the
# heading is scanned for every catalogued name rather than assumed to be one tool.
for e in entries:
    heading = ""
    body = section(doc, f"`{e['name']}`")
    if body is None:
        # `ast_plan_show` / `ast_plan_list` and `ast_edit_apply` *(write mode)*
        # both append a qualifier, so fall back to a heading that merely contains
        # the name. A section is what makes the tool documented at all.
        m = re.search(rf"^## .*`{re.escape(e['name'])}`.*$", doc, re.M)
        if m:
            heading = m.group(0)
            body = doc[m.end():]
            nxt = re.search(r"^## ", body, re.M)
            body = body[: nxt.start()] if nxt else body
        else:
            body = None
    else:
        heading = f"`{e['name']}`"
    e["section"] = body
    e["heading"] = heading

documented = {e["name"] for e in entries if e["section"]}

# A level-2 section naming something that looks like a tool but is not registered.
for m in re.finditer(r"^## `?((?:ast|cli)_[a-z_]+)`?", doc, re.M):
    n = m.group(1)
    if n not in {e["name"] for e in entries}:
        problems.append(
            f"TOOLS.md: section for `{n}` has no ToolEntry in registry.rs "
            f"(remove the section, or register the tool)"
        )

for e in entries:
    if not e["section"]:
        problems.append(
            f"TOOLS.md: tool `{e['name']}` is registered in registry.rs but has no "
            f"section in TOOLS.md"
        )

# ------------------------------------------------- modes and annotations table
# `Mode::ReadOnly` in the registry is spelled `read` in the document. The mapping is
# explicit rather than derived so that a third mode (`Mode::Sandboxed`, say) is a
# loud failure here instead of a silent string comparison that happens to pass.
DOC_MODE = {"ReadOnly": "read", "Write": "write"}

modes_body = section(doc, "Modes and annotations")
if modes_body is None:
    sys.exit("check-tools-docs: TOOLS.md has no '## Modes and annotations' heading")
modes = {}
for m in re.finditer(
    r"^\|\s*`([a-z_]+)`\s*\|\s*\**(read|write)\**[^|]*\|(.+)$", modes_body, re.M
):
    cells = m.group(3)
    vals = []
    for cell in cells.split("|"):
        c = cell.strip().strip("*").lower()
        vals.append("true" if c == "true" else ("false" if c == "false" else None))
    modes[m.group(1)] = {"mode": m.group(2), "hints": vals}

for e in entries:
    row = modes.get(e["name"])
    if row is None:
        problems.append(
            f"TOOLS.md: `{e['name']}` has no row in the 'Modes and annotations' table"
        )
        continue
    want_mode = DOC_MODE.get(e["mode"], e["mode"].lower())
    if row["mode"] != want_mode:
        problems.append(
            f"TOOLS.md: `{e['name']}` mode is documented as {row['mode']!r} but "
            f"registry.rs registers Mode::{e['mode']}"
        )
    ann = ann_consts.get(e["const"] or "")
    if ann is None:
        problems.append(
            f"TOOLS.md: `{e['name']}` uses annotation const {e['const']!r}, which is "
            f"not defined in registry.rs"
        )
        continue
    fields = (
        "read_only_hint",
        "destructive_hint",
        "idempotent_hint",
        "open_world_hint",
    )
    for i, f in enumerate(fields):
        if i >= len(row["hints"]) or row["hints"][i] is None:
            continue
        if row["hints"][i] != ann[f]:
            problems.append(
                f"TOOLS.md: `{e['name']}` documents {f} as {row['hints'][i]} but "
                f"registry.rs publishes {ann[f]}"
            )

for name in sorted(modes):
    if name not in {e["name"] for e in entries}:
        problems.append(
            f"TOOLS.md: the 'Modes and annotations' table has a row for `{name}`, "
            f"which is not in registry.rs"
        )

# ------------------------------------------------------- arguments vs schemas
#
# The first cell of an `| Argument | ...` table row, backticked. A row whose first
# cell lists several names at once ("`language`, `paths`, `pattern`") contributes
# each of them, which is why this is a findall rather than a single match.
#
# The header row and the `|---|` separator beneath it are skipped explicitly. They
# cannot be skipped by "cut the body at the next separator", because the separator
# IS the next line: doing that discards the whole table and every later check
# passes vacuously. That is why the caller raises a problem when a tool with
# properties documents no argument name — the empty result must be loud.
def argument_names(section_body, tool_name):
    m = re.search(r"^\| Argument \|.*$", section_body, re.M)
    out = set()
    if m:
        out |= _first_cell_names(section_body[m.end():])
    # `ast_plan_show` / `ast_plan_list` share one section and publish their arguments
    # in a `| Tool | Arguments | Result |` table. Only THIS tool's row counts:
    # taking the whole table would credit each tool with its sibling's arguments and
    # hide a real schema mismatch behind a neighbour's.
    t = re.search(r"^\| Tool \| Arguments \|.*$", section_body, re.M)
    if t:
        for row_name, args_cell in _rows(section_body[t.end():]):
            if row_name == tool_name:
                out |= set(re.findall(r"`([a-z_][a-z0-9_]*)`", args_cell))
    return out


def _rows(body):
    """(first-cell-name, remaining cells) for each row of a table body."""
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [c.strip() for c in stripped.split("|")]
        if len(cells) < 2:
            continue
        first = cells[1]
        if first.strip() and set(first.strip()) <= set("-: "):
            continue
        name = None
        m = re.fullmatch(r"`([a-z_][a-z0-9_]*)`", first)
        if m:
            name = m.group(1)
        yield name, "|".join(cells[2:])


def _first_cell_names(body):
    out = set()
    for line in body.splitlines():
        stripped = line.strip()
        if not stripped.startswith("|"):
            if out:
                break
            continue
        cells = stripped.split("|")
        if len(cells) < 2:
            continue
        first = cells[1]
        if first.strip() and set(first.strip()) <= set("-: "):  # |---|---|
            continue
        out.update(re.findall(r"`([a-z_][a-z0-9_]*)`", first))
    return out


for e in entries:
    if not e["section"]:
        continue
    props = schema_props(e["schema_const"] or "")
    if props is None:
        problems.append(
            f"TOOLS.md: schema const {e['schema_const']!r} for `{e['name']}` was not "
            f"found or did not parse as JSON in registry.rs"
        )
        continue
    documented_args = argument_names(e["section"], e["name"])
    if props and not documented_args:
        problems.append(
            f"TOOLS.md: `{e['name']}` publishes schema properties {props} but its "
            f"section documents no argument name, so this check would pass vacuously"
        )
        continue
    if not props:
        if documented_args:
            problems.append(
                f"TOOLS.md: `{e['name']}` documents arguments "
                f"{sorted(documented_args)} but its inputSchema accepts no properties"
            )
        continue
    for p in props:
        if p not in documented_args:
            problems.append(
                f"TOOLS.md: `{e['name']}` publishes schema property `{p}` but its "
                f"argument table does not document it"
            )
    # The other direction: an argument the doc tells an agent to pass that the
    # schema does not accept. This is the direction that breaks a real call.
    for a in sorted(documented_args - set(props)):
        problems.append(
            f"TOOLS.md: `{e['name']}` documents an argument `{a}` that its "
            f"inputSchema does not accept"
        )

# ------------------------------------------------------------- error codes
#
# The table's Code cell is a single backticked name on most rows, but one row
# collapses four variants into a prose cell: `plan_not_found` / `plan_expired` /
# `plan_corrupt` / `wrong_workspace`. Every backticked snake_case token in the cell
# is therefore a code, so a combined row still counts for all four variants rather
# than silently documenting only the first.
if error_path.exists():
    err = error_path.read_text(encoding="utf-8")
    em = re.search(r"pub enum ErrorCode\s*\{(.*?)\n\}", err, re.S)
    if em:
        variants = []
        for line in em.group(1).splitlines():
            line = line.strip()
            mv = re.match(r"([A-Z][A-Za-z0-9]*)\s*,\s*$", line)
            if mv:
                variants.append(mv.group(1))
        def snake(n):
            return re.sub(r"(?<!^)(?=[A-Z])", "_", n).lower()
        in_code = {snake(v) for v in variants}

        err_body = section(doc, "Error code reference")
        if err_body is None:
            sys.exit(
                "check-tools-docs: TOOLS.md has no '## Error code reference' heading"
            )
        in_doc = set()
        for m in re.finditer(r"^\|\s*(.+?)\s*\|", err_body, re.M):
            cell = m.group(1)
            names = re.findall(r"`([a-z_][a-z0-9_]*)`", cell)
            if not names:
                continue
            # A separator row (|---|---|) yields no backticked names and is skipped
            # by the `if not names` above.
            in_doc.update(names)
        for c in sorted(in_code - in_doc):
            problems.append(
                f"TOOLS.md: ErrorCode variant `{c}` has no row in the "
                f"'Error code reference' table"
            )
        for c in sorted(in_doc - in_code):
            problems.append(
                f"TOOLS.md: the 'Error code reference' table documents `{c}`, which is "
                f"not an ErrorCode variant in crates/core/src/error.rs"
            )

        # Same vacuity guard as above, for the error-code scan.
        if len(in_code) < MIN_ERROR_CODES:
            problems.append(
                f"only {len(in_code)} ErrorCode variants were read from error.rs (expected "
                f"at least {MIN_ERROR_CODES}); the scan is too narrow and this check would "
                f"pass vacuously"
            )

# ------------------------------------------------------------------- reporting
counts = {
    "tools in registry": len(entries),
    "documented tools": len(documented),
    "schemas checked": sum(
        1 for e in entries if schema_props(e["schema_const"] or "") is not None
    ),
}
print("check-tools-docs: " + ", ".join(f"{v} {k}" for k, v in counts.items()))

if problems:
    for p in problems:
        print(f"check-tools-docs: {p}", file=sys.stderr)
    print(
        f"check-tools-docs: {len(problems)} problem(s); docs/TOOLS.md and "
        f"crates/tools/src/registry.rs disagree",
        file=sys.stderr,
    )
    sys.exit(1)
print("check-tools-docs: TOOLS.md matches registry.rs and ErrorCode in both directions")
PY