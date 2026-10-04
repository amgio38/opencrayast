#!/usr/bin/env bash
# Refuse Rust sources under crates/*/src/ that rustc will never compile.
#
# A .rs file next to lib.rs is not part of the crate unless a `mod` (or
# `#[path = "..."] mod`) reaches it. That is how crates/cli/src/apply.rs and
# stdin_confirm.rs sat in the tree looking like the write path while the live
# commands lived in edit.rs. This check makes that shape red.
#
# Roots are src/lib.rs, src/main.rs, and every [[bin]] / [lib] path named in
# the crate manifest. File modules are those declared at brace-depth 0 of a
# reached file (`mod name;` or `#[path = "rel"] mod name;`). Inline modules
# (`mod name { ... }`) do not name a file.
#
# Usage: check-undeclared-src.sh [--root DIR]
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
while [ $# -gt 0 ]; do
	case "$1" in
		--root)
			root=$(CDPATH= cd -- "$2" && pwd)
			shift 2
			;;
		-h|--help)
			echo "usage: check-undeclared-src.sh [--root DIR]"
			exit 0
			;;
		*)
			echo "unknown argument: $1" >&2
			exit 2
			;;
	esac
done

if [ ! -d "$root/crates" ]; then
	echo "error: no crates/ directory under $root" >&2
	exit 2
fi

python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
crates = root / "crates"

IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


def skip_ws_and_comments(src: str, i: int) -> int:
    n = len(src)
    while i < n:
        c = src[i]
        if c in " \t\r\n":
            i += 1
            continue
        if src.startswith("//", i):
            nl = src.find("\n", i)
            i = n if nl < 0 else nl + 1
            continue
        if src.startswith("/*", i):
            end = src.find("*/", i + 2)
            i = n if end < 0 else end + 2
            continue
        break
    return i


def skip_string(src: str, i: int) -> int:
    n = len(src)
    if src.startswith('r#', i) or src.startswith('br#', i) or src.startswith('cr#', i):
        hashes = 0
        j = i
        if src[j] in "bc":
            j += 1
        j += 1  # r
        while j < n and src[j] == "#":
            hashes += 1
            j += 1
        if j < n and src[j] == '"':
            j += 1
            close = '"' + ("#" * hashes)
            k = src.find(close, j)
            return n if k < 0 else k + len(close)
        return i + 1
    if src.startswith('b"', i) or src.startswith('c"', i):
        i += 1
    if i < n and src[i] == '"':
        i += 1
        while i < n:
            if src[i] == "\\":
                i += 2
                continue
            if src[i] == '"':
                return i + 1
            i += 1
        return n
    return i


def skip_char_or_lifetime(src: str, i: int) -> int:
    n = len(src)
    if i >= n or src[i] != "'":
        return i
    if i + 1 < n and (src[i + 1].isalpha() or src[i + 1] == "_"):
        j = i + 2
        while j < n and (src[j].isalnum() or src[j] == "_"):
            j += 1
        return j
    j = i + 1
    if j < n and src[j] == "\\":
        j += 2
    elif j < n:
        j += 1
    if j < n and src[j] == "'":
        j += 1
    return j


def file_modules(src: str):
    """Yield (name, path_attr_or_None) for depth-0 `mod name;` items."""
    i = 0
    n = len(src)
    depth = 0
    pending_path = None
    while i < n:
        i = skip_ws_and_comments(src, i)
        if i >= n:
            break
        if src.startswith(("r#", 'r"', 'b"', 'br#', 'c"', 'cr#'), i) or (
            i < n and src[i] == '"'
        ):
            i = skip_string(src, i)
            continue
        if src[i] == "'":
            i = skip_char_or_lifetime(src, i)
            continue
        if src.startswith("#[", i):
            start = i
            i += 2
            br = 1
            while i < n and br:
                i = skip_ws_and_comments(src, i)
                if i >= n:
                    break
                if src.startswith(("r#", 'r"', 'b"', 'br#'), i) or src[i] == '"':
                    i = skip_string(src, i)
                    continue
                if src[i] == "[":
                    br += 1
                    i += 1
                elif src[i] == "]":
                    br -= 1
                    i += 1
                else:
                    i += 1
            attr = src[start:i]
            if depth == 0:
                m = re.search(r'path\s*=\s*"([^"]+)"', attr)
                if m:
                    pending_path = m.group(1)
            continue
        if src[i] in "{}":
            if src[i] == "{":
                depth += 1
            elif depth:
                depth -= 1
            pending_path = None
            i += 1
            continue
        if src[i] in "();[],":
            if src[i] == ";" and depth == 0:
                pending_path = None
            i += 1
            continue
        m = IDENT.match(src, i)
        if not m:
            i += 1
            continue
        word = m.group(0)
        i = m.end()
        if word == "pub":
            i = skip_ws_and_comments(src, i)
            if i < n and src[i] == "(":
                depth_p = 1
                i += 1
                while i < n and depth_p:
                    if src[i] == "(":
                        depth_p += 1
                    elif src[i] == ")":
                        depth_p -= 1
                    i += 1
            continue
        if word != "mod" or depth != 0:
            if depth == 0 and word not in ("pub",):
                pending_path = None
            continue
        i = skip_ws_and_comments(src, i)
        name_m = IDENT.match(src, i)
        if not name_m:
            pending_path = None
            continue
        name = name_m.group(0)
        i = name_m.end()
        i = skip_ws_and_comments(src, i)
        if i < n and src[i] == ";":
            yield name, pending_path
            pending_path = None
            i += 1
            continue
        if i < n and src[i] == "{":
            depth += 1
            pending_path = None
            i += 1
            continue
        pending_path = None


def child_dir(module_file: Path) -> Path:
    # Crate roots (lib.rs / main.rs) and directory modules (mod.rs) look for
    # children in the same directory. Other files `foo.rs` look in `foo/`.
    if module_file.name in ("lib.rs", "main.rs", "mod.rs"):
        return module_file.parent
    return module_file.parent / module_file.stem


def resolve_mod(module_file, name, path_attr):
    here = module_file.parent
    if path_attr is not None:
        candidate = (here / path_attr).resolve()
        return candidate if candidate.is_file() else None
    directory = child_dir(module_file)
    for cand in (directory / f"{name}.rs", directory / name / "mod.rs"):
        if cand.is_file():
            return cand.resolve()
    return None


def crate_roots(crate_dir):
    src = crate_dir / "src"
    found = []
    for name in ("lib.rs", "main.rs"):
        p = src / name
        if p.is_file():
            found.append(p.resolve())
    bin_dir = src / "bin"
    if bin_dir.is_dir():
        found.extend(p.resolve() for p in bin_dir.rglob("*.rs") if p.is_file())
    manifest = crate_dir / "Cargo.toml"
    if manifest.is_file():
        text = manifest.read_text(encoding="utf-8")
        for m in re.finditer(r'(?m)^\s*path\s*=\s*"([^"]+\.rs)"', text):
            p = (crate_dir / m.group(1)).resolve()
            if p.is_file() and p not in found:
                found.append(p)
    return found


problems = []
file_count = 0
crate_count = 0

for crate_dir in sorted(p for p in crates.iterdir() if (p / "Cargo.toml").is_file()):
    src = crate_dir / "src"
    if not src.is_dir():
        continue
    crate_count += 1
    all_rs = sorted(p.resolve() for p in src.rglob("*.rs") if p.is_file())
    file_count += len(all_rs)
    reached = set()
    queue = crate_roots(crate_dir)
    for r in queue:
        reached.add(r)
    i = 0
    while i < len(queue):
        current = queue[i]
        i += 1
        text = current.read_text(encoding="utf-8")
        for name, path_attr in file_modules(text):
            child = resolve_mod(current, name, path_attr)
            if child is None:
                continue
            try:
                child.relative_to(src.resolve())
            except ValueError:
                continue
            if child not in reached:
                reached.add(child)
                queue.append(child)
    for path in all_rs:
        if path not in reached:
            rel = path.relative_to(root)
            problems.append(str(rel))

if problems:
    print("undeclared crate source (not reached by any mod from a crate root):")
    for p in problems:
        print(f"  {p}")
    sys.exit(1)
print(f"undeclared-src check passed: {crate_count} crates, {file_count} src files")
PY
