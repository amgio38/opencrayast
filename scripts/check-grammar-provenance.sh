#!/usr/bin/env bash
# Reconcile the grammar provenance table in THIRD-PARTY-LICENSES.md against the
# lockfile, and refuse an unpinned grammar dependency.
#
# WHY THIS EXISTS SEPARATELY FROM `cargo deny check`
# -------------------------------------------------
# `cargo deny` reads Rust crate metadata: SPDX licence, source registry, advisory
# status, duplicate versions. It is the right tool for those four questions and
# this script does not duplicate them. It cannot answer the three that matter most
# about a grammar crate, because the answers are not in the Rust metadata:
#
#   1. Which grammars are compiled into the binary at all? A new grammar reaches
#      the product through Cargo.lock, and nothing else in the tree notices.
#   2. Which upstream commit produced the generated C? crates.io records it in
#      .cargo_vcs_info.json; cargo deny never reads it.
#   3. Does the build script do anything beyond invoking cc? A build script is
#      arbitrary code that runs before any of this project's code does, on every
#      build, on every contributor's machine.
#
# So this script holds three invariants, each of which is checked in BOTH
# directions where a direction makes sense:
#
#   A. Every `tree-sitter*` package in Cargo.lock has a row in the grammar table.
#      (A grammar that is in the graph but unrecorded is a C parser shipping with
#      no provenance.)
#   B. Every row's version equals the version in Cargo.lock. (A table that drifts
#      from the lockfile records a version nobody builds.)
#   C. No grammar dependency is unpinned. A git source is accepted only as an
#      exact `#`-prefixed commit in Cargo.lock; a floating ref, or a `branch =` /
#      `tag =` in any workspace manifest that names a grammar, is refused.
#
# Invariant C is checked against the LOCKFILE for git sources (that is where the
# resolved source is) and against the WORKSPACE MANIFESTS for `branch`/`tag`
# (cargo tolerates those, and they resolve to a mutable ref, so the lockfile alone
# would hide the float). Both are read, because either one alone is gameable.
#
# The check reads only Cargo.lock, the workspace manifests and the Markdown table.
# It runs no compiler, no network and no cargo subcommand, so it is cheap enough to
# run on every push and it cannot fail because the dependency cache is cold.
#
# Usage: check-grammar-provenance.sh [repo-root]   (default: the parent of scripts/)
set -eu

root=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$root"

lock=Cargo.lock
doc=THIRD-PARTY-LICENSES.md

if [ ! -f "$lock" ]; then
	echo "grammar provenance check failed:"
	echo "  - $lock does not exist; grammar versions cannot be reconciled"
	exit 1
fi
if [ ! -f "$doc" ]; then
	echo "grammar provenance check failed:"
	echo "  - $doc does not exist; grammar provenance cannot be recorded"
	exit 1
fi

python3 - "$lock" "$doc" "$root" <<'PY'
import re
import sys
import tomllib
from pathlib import Path

lock_path, doc_path, root = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])

# A `tree-sitter*` package is the family this repository ships C from: the
# grammars themselves, the `tree-sitter` runtime whose `src/*.c` is compiled by
# `cc`, and `tree-sitter-language`, the `Language` handle they all share. There
# is no separate "grammar" flag in the lockfile, so the name prefix is the rule.
# It is a superset on purpose: an unrecognised future `tree-sitter-*` crate is
# then recorded rather than silently compiling C with no provenance.
GRAMMAR = re.compile(r"^tree-sitter([a-z0-9-]+)?$")

problems = []

# ---- what the lockfile actually resolves ----------------------------------------

packages = []
for block in lock_path.read_text(encoding="utf-8").split("[[package]]")[1:]:
    fields = {}
    for line in block.splitlines():
        m = re.match(r'^(name|version|source|checksum)\s*=\s*"(.*)"\s*$', line)
        if m:
            fields[m.group(1)] = m.group(2)
        if line.startswith("dependencies"):
            break
    if "name" in fields:
        packages.append(fields)

lock_grammars = {p["name"]: p for p in packages if GRAMMAR.match(p["name"])}


# ---- what the document claims ----------------------------------------------------

text = doc_path.read_text(encoding="utf-8")
m = re.search(r"^## Grammar crates\b", text, re.M)
if not m:
    problems.append(
        "THIRD-PARTY-LICENSES.md has no '## Grammar crates' section; grammar "
        "provenance is required (regenerate with scripts/gen-third-party-licenses.py)"
    )
    rows = {}
else:
    # The table starts at the header row of the grammar section and runs to the
    # next heading. Rows are `| `name` | version | ... |`.
    tail = text[m.end():]
    nxt = re.search(r"^## ", tail, re.M)
    section = tail[: nxt.start()] if nxt else tail
    rows = {}
    for line in section.splitlines():
        rm = re.match(r"^\|\s*`([a-z0-9-]+)`\s*\|\s*([^|]+?)\s*\|", line)
        if rm:
            rows[rm.group(1)] = rm.group(2).strip()

    if not rows:
        problems.append(
            "the '## Grammar crates' section in THIRD-PARTY-LICENSES.md lists no "
            "crate rows; every tree-sitter package in Cargo.lock must have one"
        )

# ---- A: lockfile -> document ----------------------------------------------------

for name, p in sorted(lock_grammars.items()):
    if name not in rows:
        problems.append(
            f"{name} {p['version']} is in {lock_path.name} but has no row in the "
            f"'## Grammar crates' section of {doc_path.name}; every compiled grammar "
            f"needs its source, upstream commit and licence recorded"
        )

# ---- B: document -> lockfile (versions agree) ------------------------------------

for name, version in sorted(rows.items()):
    p = lock_grammars.get(name)
    if p is None:
        problems.append(
            f"{name} is listed in the '## Grammar crates' section of "
            f"{doc_path.name} but is not in {lock_path.name}"
        )
    elif p["version"] != version:
        problems.append(
            f"{name} version drifted: {doc_path.name} says {version}, "
            f"{lock_path.name} says {p['version']}"
        )

# ---- C: pinning ------------------------------------------------------------------

for name, p in sorted(lock_grammars.items()):
    src = p.get("source", "")
    if not src.startswith("git+"):
        # A registry package is pinned by its Cargo.lock checksum; the workspace
        # `bans.wildcards = "deny"` rule is what keeps the requirement itself bounded.
        continue
    if "#" not in src:
        problems.append(
            f"{name} comes from git source {src} with no commit fragment; a grammar "
            f"must be pinned to an exact commit, not a floating ref"
        )
    elif not src.rsplit("#", 1)[1].startswith("tree-sitter"):
        problems.append(
            f"{name} git source {src} does not start its commit fragment with "
            f"'tree-sitter'; record which upstream project the commit belongs to"
        )

# A `branch =` or `tag =` on a grammar is a float cargo resolves silently, so it
# would not necessarily show up as an unpinned git source above.
manifest_problems = []
for manifest in sorted(root.glob("**/Cargo.toml")):
    if "/target/" in str(manifest):
        continue
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (tomllib.TOMLDecodeError, OSError):
        continue
    try:
        rel = manifest.relative_to(root)
    except ValueError:
        rel = manifest
    deps = dict(data.get("dependencies") or {})
    deps.update(data.get("build-dependencies") or {})
    deps.update(data.get("dev-dependencies") or {})
    # `[workspace.dependencies]` is where this project actually declares its
    # grammar requirements, so its entries must land in the same namespace.
    for name, spec in (data.get("workspace", {}).get("dependencies") or {}).items():
        if isinstance(spec, dict):
            deps.setdefault(name, spec)
    for dep_name, spec in deps.items():
        if not GRAMMAR.match(dep_name):
            continue
        if not isinstance(spec, dict):
            continue
        if "branch" in spec or "tag" in spec:
            manifest_problems.append(
                f"{rel}: {dep_name} uses branch = / tag = "
                f"({spec.get('branch') or spec.get('tag')}); a grammar dependency must "
                f"name an exact commit, and branch/tag are not one"
            )
        if "rev" in spec and not re.fullmatch(r"[0-9a-fA-F]{40}", str(spec["rev"])):
            manifest_problems.append(
                f"{rel}: {dep_name} pins rev = {spec['rev']!r}, which is not a full "
                f"40-character commit id"
            )

problems.extend(manifest_problems)

if problems:
    print("grammar provenance check failed:")
    for p in problems:
        print(f"  - {p}")
    sys.exit(1)

print(
    f"grammar provenance check passed: {len(lock_grammars)} grammar package(s) in "
    f"{lock_path.name}, all recorded and version-matched in {doc_path.name}, all pinned"
)
PY
