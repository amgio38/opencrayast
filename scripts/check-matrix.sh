#!/bin/sh
# The threat-to-test matrix check.
#
#   1. every test identifier referenced from the design documents exists in the
#      catalogue in docs/TESTING.md;
#   2. every identifier in the catalogue is referenced by at least one threat,
#      invariant or edit-model obligation (an orphan test proves nothing the
#      model claims);
#   3. every threat row in docs/SECURITY-MODEL.md names at least one test.
#   4. every catalogue row names a concrete TEST TARGET, and the target is checked
#      twice: that it exists, and that it is the KIND of thing the column claims.
#      A `::test_fn` target names a file and a function in it that CARRIES
#      `#[test]`, in any spelling cargo accepts - that semantic check is the whole
#      rule, and it holds wherever the file lives (`tests/` or an in-crate
#      `src/spec/` module). A BARE target names no function, so it is checked by
#      shape instead: `crates/<crate>/tests/<name>.rs` or
#      `crates/<crate>/src/spec/<name>.rs`, never product source, a directory or
#      a document. A `ci:` target must name a file under `.github/workflows/`.
#      A row may list several targets, separated by `;`, and all are checked.
#      A row may instead be marked deferred (`-`), and only when its milestone is
#      strictly above LANDED_MILESTONE below.
#
# Check 4 is the one that makes this script able to fail for the reason the
# security model claims it can: a test file that is deleted or renamed, or a
# catalogue row that promises a test nobody wrote, both exit non-zero. Checks
# 1-3 only ever compared identifiers between Markdown files and could not see
# the test tree at all (SEC-A1 F-05).
#
# Every count this script prints is counted here, not written down: the numbers
# are derived from the catalogue and the filesystem in the same run that checks
# them.
#
# Usage: check-matrix.sh [repo-root]      (default: the parent of scripts/)
set -eu

root=${1:-$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)}
cd "$root"

# The highest milestone whose exit criteria the tree is expected to satisfy.
# A catalogue row may defer its target only ABOVE this line; at or below it, the
# row promises a test and the test has to be there. Kept in one place so the
# catalogue and this script cannot disagree about what "already landed" means.
LANDED_MILESTONE=M4

python3 - "$LANDED_MILESTONE" <<'PY'
import re
import sys
from pathlib import Path

landed = sys.argv[1]
PREFIXES = "BND|LMT|PRS|PAT|EDT|MCP|CFG|STA|OUT|SUP"
ID = re.compile(rf"\b({PREFIXES})-(\d\d)\b")
RANGE = re.compile(rf"\b({PREFIXES})-(\d\d)\s*(?:…|\.\.\.|–|-)\s*(?:\1-)?(\d\d)\b")
ROW = re.compile(rf"\|\s*((?:{PREFIXES})-\d\d)\s*\|")


def expand(text):
    ids = set()
    for m in RANGE.finditer(text):
        p, a, b = m.group(1), int(m.group(2)), int(m.group(3))
        for n in range(a, b + 1):
            ids.add(f"{p}-{n:02d}")
    for m in ID.finditer(text):
        ids.add(f"{m.group(1)}-{m.group(2)}")
    return ids


problems = []

# ---- 1-3: the identifier cross-references (unchanged behaviour) --------------------

testing_path = Path("docs/TESTING.md")
testing = testing_path.read_text(encoding="utf-8")

# The catalogue is the set of `### ...` SECTIONS under "## Test catalogue", and the scan
# is bounded to them: it stops at the next level-2 heading. That bound is load-bearing,
# not tidiness. docs/TESTING.md also carries a "Rows whose milestone was corrected"
# table and a suite table, and both mention identifiers; scanning past the catalogue
# would read them as catalogue rows. The first version of this comment claimed that
# bound while the code only did `partition("## Test catalogue")`, which keeps
# EVERYTHING after the heading -- so those tables were excluded only because their
# identifier cells happened to be backticked. Backticks are not a parser.
head, sep, tail = testing.partition("## Test catalogue")
if not sep:
    problems.append("docs/TESTING.md has no '## Test catalogue' heading")
    sys.exit(1)

# Cut the catalogue off at the next level-2 heading.
_section_end = tail.find("\n## ")
if _section_end != -1:
    tail = tail[:_section_end]

catalogue = {}          # id -> (milestone, target cell)
catalogue_order = []
for line in tail.splitlines():
    m = ROW.match(line)
    if not m:
        continue
    cells = [c.strip() for c in line.strip().strip("|").split("|")]
    tid = m.group(1)
    if len(cells) < 4:
        problems.append(f"{tid}: catalogue row has no milestone cell")
        continue
    catalogue[tid] = (cells[3], None)
    catalogue_order.append(tid)

refs = set()
docs_scanned = []
for name in sorted(Path("docs").glob("*.md")) + [
    Path(x) for x in ("README.md", "ROADMAP.md", "SECURITY.md") if Path(x).exists()
]:
    docs_scanned.append(str(name))
    if name.name == "TESTING.md":
        continue
    refs |= expand(name.read_text(encoding="utf-8"))

for missing in sorted(refs - set(catalogue)):
    problems.append(f"referenced but not in the catalogue: {missing}")
for orphan in sorted(set(catalogue) - refs):
    problems.append(f"in the catalogue but referenced by no threat/invariant: {orphan}")

sec = Path("docs/SECURITY-MODEL.md").read_text(encoding="utf-8")
threats = 0
for line in sec.splitlines():
    # Two digits, no trailing letter: `T-03r` is a residual-risk row, not a threat,
    # and an accepted risk is not required to name a test.
    m = re.match(r"\|\s*(T-\d\d)\s*\|", line)
    if m:
        threats += 1
        if not expand(line):
            problems.append(f"threat {m.group(1)} names no test")

# ---- 4: the targets actually exist, AND ARE WHAT THEY CLAIM ------------------------
#
# The catalogue's last column may be:
#     <test file>        a test file that must exist and be a test file
#     <test file>::<fn>  that file, and a `#[test] fn <fn>` inside it
#     ci:<workflow>::<t> a CI step that must appear in that workflow
#     -                  deferred: legal only above LANDED_MILESTONE
#
# Checked in declaration order so the same catalogue always produces the same
# message list.
#
# Two things are checked, and the second one exists because checking only the first
# let a row point at anything that happens to exist:
#
#   (a) EXISTENCE. The path resolves.
#   (b) IDENTITY. It is the kind of thing the column claims. A bare path must be a
#       test file under a crate's `tests/` directory, so pointing a row at product
#       source (`crates/**/src/**`), at a directory, or at a document is refused. A
#       `::fn` target must name a function that CARRIES `#[test]` - not merely a
#       function with that name, which is what a mutation that drops the attribute
#       leaves behind (and which cargo then silently stops running).

def split_row(line):
    return [c.strip() for c in line.strip().strip("|").split("|")]


# A BARE `path` target names no function, so there is nothing to check semantically and the
# rule has to be about shape. Two shapes count, and both are places this repository keeps
# tests:
#   crates/<crate>/tests/<name>.rs        an integration test
#   crates/<crate>/src/spec/<name>.rs     an in-crate unit test (`#[cfg(test)] mod`)
# `crates/<crate>/src/<anything else>.rs` is still refused, so this does not reopen the door
# that let a row point at product source.
#
# A `::fn` target needs NO shape rule at all, and must not have one. The semantic question
# is "is there a function by that name in that file that CARRIES #[test]", and
# `has_test_attr` already answers exactly that. Adding a path rule on top made the check
# NARROWER than the property it guards: SEC-FIX 4 moved apply_spec.rs, undo_spec.rs,
# edit7_extra_spec.rs and secfix4_write_cap_spec.rs into crates/edit/src/spec/ (they need
# `crate::` internals to mint a WriteCap), and all 14 EDT rows went red as false reds -
# the same shape as F1-FP, a rule narrower than the thing it protects. A file that contains
# the named #[test] IS the test, wherever it lives.
TEST_FILE = re.compile(r"^crates/[^/]+/(tests/[^/]+|src/spec/[^/]+)\.rs$")
# A CI target must be a real workflow, not any file with the right text in it.
WORKFLOW = re.compile(r"^\.github/workflows/[^/]+\.ya?ml$")


def is_test_file(rel):
    """True when `rel` names a crate integration-test file."""
    return bool(TEST_FILE.match(rel))


# `#[test]` or `#[test(...)]`, anywhere on the line. Matching the whole line instead
# (equality, or a `startswith`) is what made `#[test] #[ignore]` and
# `#[cfg(unix)] #[test]` read as "no test here" - both are tests cargo runs.
TEST_ATTR = re.compile(r"#\[test\s*[\]\(]")


def strip_comment(line):
    """The line with any `//` comment removed, so prose cannot look like code."""
    i = line.find("//")
    return line if i < 0 else line[:i]


def has_test_attr(src, fn):
    """True when `src` has a function `fn` that CARRIES `#[test]`.

    Two things this has to get right, and both were wrong in the first version:

    * The attribute is REQUIRED. It used to be matched as a zero-or-more group,
      so `#[allow(dead_code)] fn x` satisfied the check: the matrix stayed green,
      clippy stayed green, and cargo had quietly stopped running the test.

    * Every spelling cargo accepts has to be ACCEPTED here. The first version
      walked up only while a line began with `#[`, so it reported "no test here"
      for `#[test] #[ignore]`, for `#[cfg(unix)] #[test]`, for a `//` comment or a
      blank line between the attribute and the signature, and for `#[test] fn x()`
      written on one line. All five are tests cargo really runs, and this repository
      already has `#[ignore]` and `#[cfg(...)]` attributes - so those were latent
      FALSE REDS: a red light that lies is how people learn to walk past a gate.

    So: strip comments, find the signature anywhere on its line, accept an
    attribute on that same line, and otherwise walk upwards over blank and comment
    lines only, stopping at the first line that is neither.
    """
    sig = re.compile(rf"\b(?:pub\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+{re.escape(fn)}\b")
    lines = [strip_comment(l) for l in src.splitlines()]
    for i, line in enumerate(lines):
        m = sig.search(line)
        if not m:
            continue
        # Attributes written on the signature's own line.
        if TEST_ATTR.search(line[:m.start()]):
            return True
        j = i - 1
        while j >= 0:
            s = lines[j].strip()
            if s == "" or s.startswith("#["):
                if TEST_ATTR.search(s):
                    return True
                j -= 1
                continue
            break
        return False
    return False


def check_target(tid, target):
    """Check ONE target of a catalogue row. Appends to `problems`; returns nothing.

    The three forms are the ones the Target column documents. The `ci:` form is
    separated with `::` once, so the needle may itself contain anything.
    """
    if target.startswith("ci:"):
        spec, needle = target.split("::", 1)
        wf = spec[len("ci:"):]
        if not WORKFLOW.match(wf):
            problems.append(
                f"{tid}: {wf} is not a CI workflow; a `ci:` target must name a file under "
                f".github/workflows/"
            )
            return
        p = Path(wf)
        if not p.exists():
            problems.append(f"{tid}: CI workflow {wf} does not exist")
        elif needle not in p.read_text(encoding="utf-8"):
            problems.append(f"{tid}: step {needle!r} is not in {wf}")
        return

    if "::" in target:
        # No shape rule here on purpose: the semantic check IS the rule. Pointing this at
        # product source is still refused, because a source file has no `#[test] fn` by that
        # name - see SECFIX3-08 and SECFIX3-06.
        rel, fn = target.split("::", 1)
        p = Path(rel)
        if not p.exists():
            problems.append(f"{tid}: test file {rel} does not exist")
            return
        if not has_test_attr(p.read_text(encoding="utf-8"), fn):
            problems.append(
                f"{tid}: {rel} has no `#[test] fn {fn}` - either the function is gone or it no "
                f"longer carries #[test], which means cargo does not run it"
            )
        return

    if not is_test_file(target):
        problems.append(
            f"{tid}: {target} is not a place this repository keeps tests; a bare Target (one "
            f"with no ::fn) must be crates/<crate>/tests/<name>.rs or "
            f"crates/<crate>/src/spec/<name>.rs. Name a ::fn instead and the file may live "
            f"anywhere, as long as it has a #[test] fn by that name"
        )
        return
    if not Path(target).exists():
        problems.append(f"{tid}: test target {target} does not exist")


for line in tail.splitlines():
    m = ROW.match(line)
    if not m:
        continue
    tid = m.group(1)
    cells = split_row(line)
    if len(cells) < 5:
        problems.append(f"{tid}: no Target cell (add one, or `-` to defer)")
        continue
    milestone = cells[3]
    # The Target cell is written `like this` and may carry a trailing
    # "(no test yet; ...)" note for a deferred row. Strip both before using it.
    # Greedy to the LAST "(no test yet": a note may legitimately contain ")" (a reason
    # often quotes one), and `[^)]*` would stop at the first and leave the rest of the
    # note glued to the Target.
    target = re.sub(r"\s*\(no test yet.*$", "", cells[4]).strip().strip("`").strip()
    catalogue[tid] = (milestone, target)

    if target == "-":
        if milestone <= landed:
            problems.append(
                f"{tid}: deferred but its milestone {milestone} is at or below the landed "
                f"milestone {landed} - a row at or below {landed} promises a test"
            )
        continue
    if not target:
        problems.append(f"{tid}: empty Target cell (name a target, or `-` to defer)")
        continue

    # A row may name SEVERAL targets, separated by `;`. One obligation is often covered by
    # more than one test (PAT-03 is two: a length cap and a step budget), and forcing one of
    # them to be dropped would lose coverage. Every target is checked.
    for one in [t.strip().strip("`").strip() for t in target.split(";")]:
        if one:
            check_target(tid, one)

# ---- 5: a row's KIND must not claim a dimension its target cannot reach ----------
#
# Checks 1-4 ask whether a row's target EXISTS. This one asks whether the row's
# CLAIM is something that target can actually demonstrate - the defect
# ISSUE-TESTING-MD-KIND-PAT-06-RUST is about. Three rules, each stated as a
# question with a decidable answer:
#
#   (5a) `per language` -> the cited test must reach EVERY language in
#        `Language::all()`, or the row must say which ones it does cover.
#   (5b) `property`     -> a property is a claim about a SPACE, so a single named
#        test with no quantifier in it cannot carry it.
#   (5c) OS-shaped kinds -> "per OS"/"per language" cross-platform claims need a
#        `ci:` target or a `cfg(...)`-gated target, because a test cannot observe
#        the operating system it is running on from inside one process.
#
# WHY THESE THREE AND NOT A GENERIC RULE. The obvious stronger rule - "a `property`
# row may not cite exactly one test" - was measured before being written, and it
# is WRONG: of the eleven rows whose kind contains `property`, nine cite a single
# `::fn` that is nonetheless a real property test (BND-16 drives `fuzz::run_cases`
# over a seed corpus; OUT-02 loops over twenty cases; EDT-11 and EDT-30 assert
# over every file in a plan). Enforcing "more than one test" would have been nine
# false reds, and a false red is how a gate gets ignored. So 5b looks for a
# QUANTIFIER IN THE TEST ITSELF - a loop, a generator, or an `all()`-style sweep -
# rather than counting how many tests the row cites. That is the narrower, true
# property: the evidence must be quantified SOMEWHERE, and a single test body is the
# only place it can be.
#
# The language set is read from the tree, not written here, so adding a language to
# `Language` cannot silently leave this check measuring the old six.

# Read the catalogue's kind cell for every row, then check the claim.
LANG_FILE = re.compile(r"^crates/lang/src/language\.rs$")

def language_ids():
    """Every `Language` variant, from the enum in the tree.

    Derived, not hard-coded: `crates/lang/src/language.rs` declares the variants
    and `Language::all()` lists them in the same order. A drift between the two
    would make this check lie, so `all()` is preferred and the enum is the
    fallback.
    """
    p = Path("crates/lang/src/language.rs")
    if not p.exists():
        return []
    src = p.read_text(encoding="utf-8")
    m = re.search(r"pub fn all\(\) -> &'static \[Language\] \{(.*?)\n    \}", src, re.S)
    if m:
        found = re.findall(r"Language::([A-Za-z0-9_]+)", m.group(1))
        if found:
            return found
    # Fallback: the variants in the enum declaration.
    m = re.search(r"pub enum Language \{(.*?)\n\}", src, re.S)
    return re.findall(r"^\s{4}([A-Z][A-Za-z0-9_]*),", m.group(1), re.M) if m else []


# How a language can be named inside a test body: `Language::Rust` / `Rust` in an
# import list, or a file extension in a table of cases. Both spellings count,
# because both are used in this tree and requiring only one would be a rule
# NARROWER than the property it guards (the F1-FP shape this repository has been
# bitten by twice).
LANG_BY_EXT = {
    "rs": "Rust",
    "js": "JavaScript", "jsx": "JavaScript", "mjs": "JavaScript", "cjs": "JavaScript",
    "ts": "TypeScript", "mts": "TypeScript", "cts": "TypeScript",
    "tsx": "Tsx",
    "py": "Python", "pyi": "Python",
    "go": "Go",
}

QUANTIFIER = re.compile(
    r"\bfor\b|\bwhile\b|run_cases|for_each|\.all\(\)|iter\(\)|loop\s*\{"
)


def test_body(path, fn):
    """The body of `fn fn` in `path`, brace-matched and string/comment aware.

    Returns None when the function cannot be located, which the callers treat as
    "cannot verify" rather than as "fails" - check 4 already owns existence, and
    a missing function there must not be reported twice.
    """
    p = Path(path)
    if not p.exists() or not fn:
        return None
    src = p.read_text(encoding="utf-8")
    m = re.search(rf"\bfn\s+{re.escape(fn)}\b", src)
    if not m:
        return None
    j = src.find("{", m.start())
    if j < 0:
        return None
    depth = 0
    k = j
    n = len(src)
    state = None
    while k < n:
        ch = src[k]
        if state is None:
            if src.startswith("//", k):
                state = "//"
                k += 2
                continue
            if src.startswith("/*", k):
                state = "/*"
                k += 2
                continue
            if src.startswith('r#"', k):
                state = 'r#"'
                k += 3
                continue
            if ch == '"':
                state = '"'
            elif ch == "'" and k + 2 < n and src[k + 2] == "'":
                state = "'char"
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return src[j:k]
            k += 1
        elif state == "//":
            if ch == "\n":
                state = None
            k += 1
        elif state == "/*":
            if src.startswith("*/", k):
                state = None
                k += 2
                continue
            k += 1
        elif state == '"':
            if ch == "\\":
                k += 2
                continue
            if ch == '"':
                state = None
            k += 1
        elif state == 'r#"':
            if src.startswith('"#', k):
                state = None
                k += 2
                continue
            k += 1
        else:  # char literal
            if ch == "\\":
                k += 2
                continue
            if ch == "'":
                state = None
            k += 1
    return None


def languages_reached(body):
    """Which `Language` variants a test body demonstrably reaches.

    Three spellings count, because this tree uses all three and a rule that
    accepted only one would be NARROWER than the property it guards - the same
    F1-FP shape this repository has been bitten by twice:

      * `Language::Rust`  - the qualified form;
      * a bare `Rust,`    - an `import` list brings the name into scope, and a
                             case table then writes `Rust,` in argument position
                             (this is how PAT-09's cases are written);
      * a file extension  - `"fix.py"`, how a table names a language by the file
                             it is a case for (this is how EDT-13's cases are
                             written).
    """
    found = set()
    known = set(language_ids())
    for name in re.findall(r"\bLanguage::([A-Za-z0-9_]+)", body):
        found.add(name)
    # An import list carries bare names: `use ...::{self, Go, JavaScript, ...}`.
    for m in re.finditer(r"use\s+[^;]*?::\{([^}]*)\}\s*;", body, re.S):
        for part in m.group(1).split(","):
            part = part.strip()
            if re.fullmatch(r"[A-Z][A-Za-z0-9_]*", part):
                found.add(part)
    # A bare variant name used as an ARGUMENT (`run(Rust, ...)`) or in a case
    # table. Restricted to the known variant names so an unrelated capitalised
    # identifier cannot be mistaken for a language.
    if known:
        for name in known:
            if re.search(rf"(?<![A-Za-z0-9_:]){re.escape(name)}\s*(?=[,)\]])", body):
                found.add(name)
    # A table of cases names languages by extension, which is how the EDT-13 and
    # PAT-10 tables do it.
    for ext in re.findall(r'"[\w/]*\.([A-Za-z]{2,4})"', body):
        hit = LANG_BY_EXT.get(ext.lower())
        if hit:
            found.add(hit)
    return found


langs = language_ids()

for line in tail.splitlines():
    m = ROW.match(line)
    if not m:
        continue
    tid = m.group(1)
    cells = split_row(line)
    if len(cells) < 5:
        continue
    kind = cells[2].lower()
    milestone = cells[3]
    raw_target = re.sub(r"\s*\(no test yet.*$", "", cells[4]).strip().strip("`").strip()
    if not raw_target or raw_target == "-":
        continue

    targets = [t.strip().strip("`").strip() for t in raw_target.split(";")]
    targets = [t for t in targets if t]
    ci_targets = [t for t in targets if t.startswith("ci:")]
    fn_targets = [t for t in targets if "::" in t and not t.startswith("ci:")]

    claims_per_language = "per language" in kind
    claims_property = "property" in kind
    claims_per_os = "per os" in kind or "per operating system" in kind

    # --- 5a: `per language` must reach every language, or say which.
    if claims_per_language and fn_targets:
        reached = set()
        unverified = False
        for t in fn_targets:
            rel, fn = t.split("::", 1)
            body = test_body(rel, fn)
            if body is None:
                unverified = True
                continue
            reached |= languages_reached(body)
        if not unverified and langs:
            missing = [l for l in langs if l not in reached]
            if missing:
                problems.append(
                    f"{tid}: kind claims `per language`, but its cited test reaches only "
                    f"{sorted(reached) or 'no'} of the {len(langs)} languages this tree knows "
                    f"({langs}); missing {missing}. Either cover them, or narrow the Kind cell to "
                    f"the languages the test actually names"
                )

    # --- 5b: `property` must be quantified somewhere in the cited test.
    #
    # A single `::fn` is NOT itself disqualifying - see the note above the rule.
    # What disqualifies it is a body with no quantifier in it: no loop, no
    # generator, no sweep over `Language::all()`. That is a property claim backed
    # by one fixed example, which is what PAT-10 was.
    #
    # A row may cite SEVERAL targets, and the claim is satisfied if ANY ONE of them
    # carries the quantification. Requiring all of them was wrong and fired on a row
    # whose evidence was already sound: PAT-10 cites the quantified property test
    # AND the older single-example golden, and a single unquantified companion
    # should not condemn the row. The obligation is what matters, not the number of
    # tests behind it.
    if claims_property and fn_targets:
        quantified = False
        unquantified = []
        for t in fn_targets:
            rel, fn = t.split("::", 1)
            body = test_body(rel, fn)
            if body is None:
                unquantified.append(f"{rel}::{fn} (could not be read)")
                continue
            if QUANTIFIER.search(body):
                quantified = True
            else:
                unquantified.append(f"{rel}::{fn}")
        if not quantified:
            problems.append(
                f"{tid}: kind claims `property`, but none of its cited tests is quantified - "
                f"{', '.join(unquantified)} contains no loop, generator or sweep. One fixed "
                f"example cannot establish a property: quantify a test over the space it "
                f"claims, or change the Kind cell to `golden`"
            )

    # --- 5c: a cross-platform claim needs evidence that can see a platform.
    #
    # A test cannot observe the operating system it is running on from inside one
    # process: the query and language APIs take `&str`, so nothing about the host
    # reaches them. The only evidence that spans platforms is the SAME TESTS RUN ON
    # SEVERAL RUNNERS, which is a `ci:` target - or a test explicitly gated to one
    # platform by `cfg(...)`, which then says "per OS" honestly by being per-OS.
    if (claims_per_os or claims_per_language) and targets and not ci_targets:
        gated = False
        for t in fn_targets:
            rel, fn = t.split("::", 1)
            p = Path(rel)
            if p.exists():
                # The WHOLE file is scanned, not a prefix of it. A 400-character
                # window was tried first and false-reded EDT-14 and EDT-28, whose
                # files both carry `#![cfg(unix)]` - at line 13 and line 9,
                # respectively, past the window. `fsio_spec.rs` also opens with a
                # five-line `#![allow(...)]`, so any fixed prefix is a guess. A rule
                # that guesses is a rule that lies, so it reads the file.
                src_all = p.read_text(encoding="utf-8")
                if re.search(r"#!\[cfg\(|#\[cfg\(", src_all):
                    gated = True
        if not gated:
            span = "per OS" if claims_per_os else "per language"
            problems.append(
                f"{tid}: kind claims `{span}` but nothing in its targets can observe a "
                f"platform: the cited test is neither cfg-gated to one nor paired with a "
                f"`ci:` target naming a multi-platform run. Add the ci: target that runs it "
                f"on each platform, or narrow the Kind cell"
            )

# ---- report: every number below is counted here, never written down -----------------
#
# Computed before the verdict, and printed in BOTH branches: a failing run reports
# how much of the catalogue it managed to resolve, which is the number you need
# when a whole test file has gone missing.

def target_exists(target):
    """True when every target in a cell resolves. A cell may list several, `;`-separated."""
    if not target or target == "-":
        return False
    ones = [t.strip().strip("`").strip() for t in target.split(";")]
    ones = [t for t in ones if t]
    if not ones:
        return False
    for t in ones:
        head = t.split("::", 1)[0]
        head = head[len("ci:"):] if t.startswith("ci:") else head
        if not Path(head).exists():
            return False
    return True


resolved = sum(1 for _, t in catalogue.values() if target_exists(t))
deferred = [i for i, (m, t) in catalogue.items() if t == "-"]
at_or_below = [i for i, (m, t) in catalogue.items() if t != "-" and m <= landed]
above = [i for i, (m, t) in catalogue.items() if t != "-" and m > landed]
doc_refs = len(refs)

summary = (
    f"{len(catalogue)} catalogue rows "
    f"({len(at_or_below)} at or below {landed}, {len(above)} above it, "
    f"{len(deferred)} deferred); {resolved} targets resolved on disk; "
    f"{doc_refs} identifiers referenced across {len(docs_scanned)} documents; "
    f"{threats} threats, each naming a test"
)

if problems:
    print("matrix check failed:")
    for p in problems:
        print(f"  - {p}")
    print(f"\n  {summary}; {len(problems)} problem(s); landed milestone {landed}")
    sys.exit(1)

print(f"matrix check passed: {summary}")
PY
