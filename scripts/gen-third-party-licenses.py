#!/usr/bin/env python3
"""Regenerate the inventory tables of THIRD-PARTY-LICENSES.md from cargo metadata.

Usage (repository root): python3 scripts/gen-third-party-licenses.py
Only crates.io packages reachable from workspace members through normal
(non-dev, non-build) dependency edges are listed.

The grammar section additionally records what `cargo deny` does NOT look at: where
a grammar's C source comes from, which upstream commit produced it, and what its
build script does beyond invoking `cc`. Those facts are read out of the crate
source cargo already extracted, not typed in by hand, so the table cannot drift
away from what the build actually compiles. `scripts/check-grammar-provenance.sh`
enforces the table against Cargo.lock.
"""
import json
import os
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

# Crates whose package name starts with this are grammar/runtime crates that ship
# C or generated C compiled into the binary by a build script.
GRAMMAR_PREFIX = "tree-sitter"

# Calls in a build script that are worth writing down because they are NOT "invoke
# cc and link the result". Recorded as observed, not judged: a reader can see what
# upstream does without trusting this script's opinion.
BUILD_SCRIPT_PROBES = (
    ("shells out to a process", re.compile(r"process::Command|Command::new")),
    ("fs write/copy/remove", re.compile(r"fs::(write|copy|remove|rename|create_dir)")),
    ("filesystem read_dir", re.compile(r"read_dir\(")),
    ("network", re.compile(r"reqwest|curl|TcpStream|ureq|https?://")),
    ("bindgen", re.compile(r"bindgen::")),
)

# Paths a build script reads through env::var that are legitimate build plumbing.
# Listed so the env::var probe stays meaningful instead of firing on every crate.
BUILD_ENV_ALLOWLIST = (
    "TARGET", "OUT_DIR", "CARGO_MANIFEST_DIR", "CARGO_FEATURE_WASM",
    "CARGO_PKG_RUST_VERSION", "DEP_WASMTIME_C_API_INCLUDE",
    "DEP_TREE_SITTER_LANGUAGE_WASM_HEADERS", "HOST", "OPT_LEVEL",
)


def find_grammar_packages(pkgs):
    return sorted(
        (p for p in pkgs.values() if p["name"].startswith(GRAMMAR_PREFIX)),
        key=lambda p: p["name"],
    )


def read_text(path):
    try:
        return Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""


def c_sources(crate_root):
    """Every C source cargo would compile for this crate, as (relpath, lines)."""
    out = []
    for dirpath, _dirs, files in os.walk(crate_root):
        if os.sep + "target" in dirpath:
            continue
        for f in sorted(files):
            if f.endswith((".c", ".cc")):
                p = Path(dirpath) / f
                try:
                    n = sum(1 for _ in p.open(encoding="utf-8", errors="replace"))
                except OSError:
                    n = 0
                out.append((str(p.relative_to(crate_root)), n))
    return sorted(out)


def _resolve_basenames(crate_root, basenames):
    """Expand bare C file names to their paths in the crate, in source order.

    A grammar crate that ships two grammars (typescript and tsx) has two files
    both called `parser.c`, so one name legitimately resolves to several paths.
    """
    out = []
    for base in basenames:
        for dirpath, _dirs, files in os.walk(crate_root):
            if base in files:
                rel = str((Path(dirpath) / base).relative_to(crate_root))
                if rel not in out:
                    out.append(rel)
    return sorted(out)


def _basenames_in(text):
    return sorted(set(re.findall(r'"([A-Za-z0-9_.-]+\.(?:c|cc))"', text)))


def cc_targets(crate_root, src):
    """Files the build script actually hands to `cc`.

    A generated tree-sitter crate is one huge `parser.c` plus maybe a `scanner.c`,
    named explicitly by the build script. The tree-sitter core crate is the
    exception: it hands over a single `lib.c` that `#include`s the rest, so the
    number of files passed to the compiler is not the number of C files shipped.
    Recording both separately is the point - it says how much C is actually in
    the trusted computing base, and how much of it arrives in one translation
    unit.

    The `.file()` argument is a `PathBuf` expression, not a string, so the
    literal is taken from the argument first and from the whole script when the
    argument is a loop variable.
    """
    names = []
    for arg in re.findall(r'\.file\(\s*(.+?)\s*\)\s*[;.]', src, re.S):
        names.extend(_basenames_in(arg))
    if not names:
        names = _basenames_in(src)
    return _resolve_basenames(crate_root, names)


def build_script_facts(crate_root, manifest_text):
    """Locate the build script and report what it does, mechanically."""
    rel = None
    m = re.search(r'^build\s*=\s*"([^"]+)"', manifest_text, re.M)
    if m:
        rel = m.group(1)
    elif (Path(crate_root) / "build.rs").exists():
        rel = "build.rs"
    if not rel:
        return {"present": False}

    p = Path(crate_root) / rel
    src = read_text(p) if p.exists() else ""
    uses_cc = bool(re.search(r"cc::Build|\.compile\(", src))

    hits = []
    for label, rx in BUILD_SCRIPT_PROBES:
        if rx.search(src):
            hits.append(label)

    # Environment dependence is reported only for variables outside the known
    # cargo build plumbing, so the column says WHAT is read, not "it uses env".
    env_reads = sorted(set(re.findall(r'env::var(?:_os)?\(\s*"([A-Z0-9_]+)"', src))
                       - set(BUILD_ENV_ALLOWLIST))
    if env_reads:
        hits.append("reads env: " + ", ".join(env_reads))

    return {
        "present": True,
        "path": rel,
        "uses_cc": uses_cc,
        "probes": hits,
        "env_reads": env_reads,
        "cc_targets": cc_targets(crate_root, src) if uses_cc else [],
    }


def upstream_facts(crate_root, meta_pkg):
    """Upstream project URL, declared grammar version, and the commit published."""
    vcs = {}
    v = read_text(Path(crate_root) / ".cargo_vcs_info.json")
    if v:
        try:
            vcs = json.loads(v).get("git", {})
        except ValueError:
            vcs = {}

    ts_ver = ""
    tj = read_text(Path(crate_root) / "tree-sitter.json")
    if tj:
        try:
            ts_ver = (json.loads(tj).get("metadata") or {}).get("version") or ""
        except ValueError:
            ts_ver = ""

    return {
        "repository": meta_pkg.get("repository") or "",
        "declared_version": ts_ver or meta_pkg["version"],
        "declared_version_source": "tree-sitter.json" if ts_ver else "Cargo.toml",
        "commit": vcs.get("sha1", ""),
        "path_in_vcs": vcs.get("path_in_vcs", ""),
    }


def grammar_rows(grammar_pkgs):
    rows = []
    for p in grammar_pkgs:
        root = os.path.dirname(p["manifest_path"])
        b = build_script_facts(root, read_text(Path(root) / "Cargo.toml"))
        cs = c_sources(root)
        up = upstream_facts(root, p)
        if not b["present"]:
            how = "none"
        else:
            parts = []
            if b["uses_cc"]:
                parts.append("cc compile of " + ", ".join(b["cc_targets"]))
            else:
                parts.append("no C compilation")
            extras = [x for x in b["probes"]
                      if not (x == "filesystem read_dir" and b["uses_cc"])]
            for e in extras:
                parts.append(e)
            how = "; ".join(parts)

        scanner = sorted(n for n, _ in cs if n.endswith("scanner.c"))
        rows.append({
            "name": p["name"],
            "version": p["version"],
            "source": p["source"] or "",
            "license": p["license"] or "UNKNOWN",
            "repository": up["repository"],
            "declared_version": up["declared_version"],
            "declared_version_source": up["declared_version_source"],
            "commit": up["commit"],
            "path_in_vcs": up["path_in_vcs"],
            "build_script": b.get("path", "") or "(none)",
            "build_script_what": how,
            "c_files": len(cs),
            "c_lines": sum(n for _, n in cs),
            "scanner": ", ".join(scanner) if scanner else "",
        })
    return rows


def md_cell(s):
    """A markdown table cell: pipes would break the table, newlines would break the row."""
    return str(s).replace("|", "\\|").replace("\n", " ").strip()


def grammar_block(rows):
    out = [
        "## Grammar crates (C source compiled into the binary)",
        "",
        "`cargo deny check` reads Rust crate metadata: SPDX licence, source registry and",
        "advisories. It does not read C source, does not know which upstream commit a",
        "grammar was generated from, and does not look at build scripts. This section",
        "records that part, and `scripts/check-grammar-provenance.sh` holds it against",
        "`Cargo.lock` so it cannot rot.",
        "",
        "Regenerated from the extracted crate sources by `scripts/gen-third-party-licenses.py`;",
        "do not hand-edit. Read the build-script column as an observation, not a verdict:",
        "it reports the calls the upstream build script makes, so a future release that",
        "starts shelling out or fetching is visible as a diff in this table.",
        "",
        "| Crate | Version | Source | Licence | Upstream project | Upstream version | Published from | C shipped | C lines | External scanner | Build script | What the build script does |",
        "| --- | --- | --- | --- | --- | --- | --- | ---: | ---: | --- | --- | --- |",
    ]
    for r in rows:
        src = r["source"]
        short = "crates.io" if src.startswith("registry+") else md_cell(src)
        published = r["commit"] or "(not published)"
        if r["path_in_vcs"]:
            published += f" (path `{md_cell(r['path_in_vcs'])}`)" if r["path_in_vcs"] else ""
        out.append(
            "| `{}` | {} | {} | `{}` | {} | {} ({}) | `{}` | {} | {} | {} | `{}` | {} |".format(
                r["name"], r["version"], short, r["license"],
                md_cell(r["repository"]), r["declared_version"],
                r["declared_version_source"], published,
                r["c_files"], r["c_lines"],
                md_cell(r["scanner"]) or "-",
                md_cell(r["build_script"]), md_cell(r["build_script_what"]),
            )
        )
    return "\n".join(out) + "\n\n"


meta = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"]))
pkgs = {p["id"]: p for p in meta["packages"]}
members = set(meta["workspace_members"])
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}

seen = set()
stack = list(members)
while stack:
    cur = stack.pop()
    for dep in nodes[cur]["deps"]:
        if not any(k["kind"] is None for k in dep["dep_kinds"]):
            continue
        if dep["pkg"] not in seen:
            seen.add(dep["pkg"])
            stack.append(dep["pkg"])

third = sorted((pkgs[i] for i in seen if i not in members), key=lambda p: p["name"])
counts = Counter(p["license"] or "UNKNOWN" for p in third)

out = ["## Summary by SPDX expression", "", "| SPDX expression | Crates |", "| --- | ---: |"]
for lic, n in sorted(counts.items()):
    out.append(f"| `{lic}` | {n} |")
out += ["", f"Total third-party normal dependencies: **{len(third)}**.", "", "## Crate inventory", "",
        "| Crate | Version | License (SPDX) | Repository |", "| --- | --- | --- | --- |"]
for p in third:
    out.append(f"| `{p['name']}` | {p['version']} | `{p['license'] or 'UNKNOWN'}` | {p['repository'] or ''} |")
block = "\n".join(out) + "\n\n"

rows = grammar_rows(find_grammar_packages(pkgs))
gblock = grammar_block(rows)

path = "THIRD-PARTY-LICENSES.md"
text = open(path, encoding="utf-8").read()
start = text.index("## Summary by SPDX expression")
end = text.index("## Test-only / oracle dependencies")
open(path, "w", encoding="utf-8").write(text[:start] + block + gblock + text[end:])
print(f"wrote {len(third)} crates and {len(rows)} grammar rows", file=sys.stderr)
