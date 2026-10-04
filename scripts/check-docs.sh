#!/bin/sh
# Documentation checks for the repository's Markdown files:
#   1. every relative link points at a file that exists, and every `#anchor`
#      names a heading in the target file (GitHub's slug rules);
#   2. no personal absolute path (/root/, /home/<user>/, /Users/<user>/) appears;
#   3. the documents are English (no CJK characters);
#   4. no internal tracking reference (project-board ids, ticket numbers) appears - in the
#      Markdown above, and also in `crates/*/src/**/*.rs`, because a doc comment in `src/` ships
#      inside the published crate and shows up in `cargo doc`;
#   5. every `scripts/...` path written as inline code exists.
#
# Rule 4 does NOT cover `crates/*/tests/**`. Test sources are not published and are not rendered
# into the crate docs, so a board id there leaks nothing a crates.io consumer can see, and there
# are enough of them that flagging them would bury the `src/` findings this rule exists to catch.
# That is a deliberate boundary, not an oversight; see `scripts/tests/check_docs_spec.sh`, which
# pins it.
#
# Usage: check-docs.sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

python3 - "$root" <<'PY'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
files = sorted(p for p in root.glob("*.md")) + sorted((root / "docs").rglob("*.md")) \
    + sorted((root / ".github").glob("*.md"))
problems = []

link_re = re.compile(r"\]\(([^)\s]+)\)")
code_script_re = re.compile(r"`(scripts/[A-Za-z0-9_./-]+)`")
heading_re = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
cjk_re = re.compile(r"[\u3000-\u303f\u3400-\u4dbf\u4e00-\u9fff\uff00-\uffef]")
personal_re = re.compile(r"(/root/|/home/[A-Za-z0-9._-]+/|/Users/[A-Za-z0-9._-]+/|C:\\Users\\)")
internal_re = re.compile(r"\b(Y20\d{6}|REQ-[A-Z0-9-]{3,}|ISSUE-[A-Z0-9-]{3,}|INIT\d+)\b")


def slug(text):
    # GitHub: lower-case, drop markup and punctuation except hyphens, spaces to hyphens.
    text = re.sub(r"`([^`]*)`", r"\1", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.strip().lower()
    out = []
    for ch in text:
        if ch.isalnum() or ch in "-_":
            out.append(ch)
        elif ch == " ":
            out.append("-")
    return "".join(out)


anchors = {}
for f in files:
    seen = {}
    found = set()
    in_fence = False
    for line in f.read_text(encoding="utf-8").splitlines():
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = heading_re.match(line)
        if m:
            s = slug(m.group(2))
            n = seen.get(s, 0)
            seen[s] = n + 1
            found.add(s if n == 0 else f"{s}-{n}")
    anchors[f] = found

for f in files:
    rel = f.relative_to(root)
    text = f.read_text(encoding="utf-8")
    if cjk_re.search(text):
        problems.append(f"{rel}: contains CJK characters (documents are English)")
    in_fence = False
    for lineno, line in enumerate(text.splitlines(), 1):
        if line.lstrip().startswith("```"):
            in_fence = not in_fence
        if personal_re.search(line):
            problems.append(f"{rel}:{lineno}: personal absolute path")
        if internal_re.search(line):
            problems.append(f"{rel}:{lineno}: internal tracking reference")
        if in_fence:
            continue
        for m in code_script_re.finditer(line):
            p = m.group(1).rstrip(".,;:")
            if "*" not in p and "<" not in p and not (root / p).exists():
                problems.append(f"{rel}:{lineno}: `{p}` does not exist")
        for m in link_re.finditer(line):
            target = m.group(1)
            if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("mailto:"):
                continue
            path, _, anchor = target.partition("#")
            dest = f if not path else (f.parent / path).resolve()
            if path and not dest.exists():
                problems.append(f"{rel}:{lineno}: broken link {target}")
                continue
            if anchor and dest.suffix == ".md":
                known = anchors.get(dest)
                if known is None:
                    known = set()
                if anchor not in known:
                    problems.append(f"{rel}:{lineno}: no heading '#{anchor}' in {dest.relative_to(root)}")

# Rule 4 also covers crate sources. A `//!` or `///` doc comment in `crates/*/src/` is compiled
# into the published crate and rendered by `cargo doc`, so a project-board id written there is
# shipped to crates.io rather than merely kept in the repository. The other rules above stay on
# Markdown: anchors, relative links and the `scripts/...` backtick convention are all things a
# Markdown file can express and a Rust file cannot, so folding them in would invent failures
# rather than find real ones.
#
# Only `src/`, not `tests/`: test sources are never published and never reach `cargo doc`, and
# they carry enough of these references that scanning them would bury the `src/` findings.
src_files = sorted(p for c in sorted((root / "crates").glob("*")) if c.is_dir()
                   for p in c.glob("src/**/*.rs"))
for f in src_files:
    rel = f.relative_to(root)
    for lineno, line in enumerate(f.read_text(encoding="utf-8").splitlines(), 1):
        if internal_re.search(line):
            problems.append(f"{rel}:{lineno}: internal tracking reference in crate source")

if problems:
    print("documentation check failed:")
    for p in problems:
        print("  - " + p)
    sys.exit(1)
print(f"documentation check passed: {len(files)} files, {len(src_files)} crate sources")
PY
