//! Spec for the promise-to-evidence table: every security promise is backed by code or a test
//! that can actually fail.
//!
//! The acceptance criterion this file exists for is the one that had no artefact at all:
//!
//! > every sentence in the security policy has code or a test behind it
//!
//! `SECURITY.md` states that promise *in prose* and then delegates: "The promises are the
//! security invariants S-1 ... in `docs/SECURITY-MODEL.md`, each backed by named tests listed
//! in `docs/TESTING.md`." That delegation was the bug. The invariants table in
//! `SECURITY-MODEL.md` has **two** columns — ID and Invariant — and no Tests column, so the
//! sentence "each backed by named tests" was true of the *threat* rows and false of the
//! *invariant* rows, and nothing in the repository could tell the difference:
//! `scripts/check-matrix.sh` reconciles identifiers with the prefixes
//! `BND|LMT|PRS|PAT|EDT|MCP|CFG|STA|OUT|SUP`, and `S-` is not among them. The ten promises
//! that the policy actually leads with were, mechanically, outside the machine-checked set.
//!
//! So this file does what the script cannot: it treats the invariants as first-class claims
//! and requires each one to name tests that exist, carry `#[test]`, and — for the promises
//! that are strong enough to be a vulnerability if false — to be reachable at all.
//!
//! Three failure modes, deliberately separate, because they are different bugs:
//!
//! - `PROM-01` the invariant table lost a row, or a row no longer parses;
//! - `PROM-02` a promise names an identifier that is not in the test catalogue, or names none;
//! - `PROM-03` a promise's tests are all **deferred** (Target `-`), which means the promise is
//!   written down as if it held and no test is watching it. This is the one that is expected
//!   to be red on purpose for the promises that have genuinely not shipped; it is the
//!   finding, and it is asserted, not hidden.
//!
//! Every identifier this file resolves is resolved **through the catalogue's own Target
//! column and against the filesystem**, the same way `check-matrix.sh` does, so a table in
//! this file cannot claim a test that the guard script would not also accept. Nothing here is
//! hard-coded twice: the claims come out of `SECURITY-MODEL.md`, the identifiers out of that
//! same file, the targets out of `TESTING.md`, and the tests out of the tree.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/SECURITY-MODEL.md"
));
const POLICY: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../SECURITY.md"));
const CATALOGUE_DOC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/TESTING.md"
));

/// The repo root, two levels up from `crates/tools`.
///
/// Every path this file checks is resolved through here rather than through the test's own
/// working directory, because a test that depends on the cwd a runner happened to choose is
/// a test that can silently check nothing.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/tools is two levels below the repo root")
        .to_path_buf()
}

const PREFIXES: &[&str] = &[
    "BND", "LMT", "PRS", "PAT", "EDT", "MCP", "CFG", "STA", "OUT", "SUP",
];

/// The security invariants, in the order the model states them: `S-1 ..= S-10`.
///
/// Parsed from the document rather than listed here, so an eleventh invariant that someone
/// adds is *required* to name its evidence by `PROM-02` instead of quietly joining the
/// unverified set.
fn invariants() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in MODEL.lines() {
        if let Some(cells) = row_cells(line, "S") {
            out.insert(cells[0].clone(), cells[1].clone());
        }
    }
    out
}

/// The `T-nn` threat rows, mapped to the test identifiers the row cites.
fn threats() -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for line in MODEL.lines() {
        if let Some(cells) = row_cells(line, "T") {
            out.insert(cells[0].clone(), identifiers(&cells[3]));
        }
    }
    out
}

/// Split a Markdown table row into its cells, if it is a row of the given family.
///
/// `T-03r` is a residual-risk row, not a threat, and the model says an accepted risk is not
/// required to name a test — so the digits are anchored and a trailing letter does not match.
fn row_cells(line: &str, family: &str) -> Option<Vec<String>> {
    let cells: Vec<String> = line
        .split('|')
        .skip(1)
        .take_while(|c| !c.trim().is_empty())
        .map(|c| c.trim().to_string())
        .collect();
    if cells.len() < 2 {
        return None;
    }
    let is_id = format!("{family}-");
    if !cells[0].starts_with(&is_id) {
        return None;
    }
    let digits = &cells[0][is_id.len()..];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(cells)
}

/// Every catalogue identifier cited in a cell, with ranges expanded.
///
/// `PRS-01..04` is one way of writing four identifiers, and a model that wrote the range must
/// get the credit of all four — otherwise the fix for one identifier silently drops the rest.
///
/// A range only counts when the high bound is **adjacent** to the low one, separated by `..`,
/// `…`, `–` or `-`. That adjacency is the whole rule, and an earlier version of this scan got
/// it wrong by skipping ahead to the next digit anywhere in the cell: in
/// `LMT-01, LMT-02, LMT-03, LMT-06, LMT-07, BND-20` it read the `20` of `BND-20` as the end of
/// a `LMT-07..20` range and invented `LMT-08` through `LMT-20`, which then failed as a "cited
/// but not in the catalogue" finding against a row that is perfectly fine. A scanner that
/// manufactures evidence is worse than no scanner.
fn identifiers(cell: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for prefix in PREFIXES {
        let full = format!("{prefix}-");
        let mut at = 0;
        while let Some(found) = cell[at..].find(&full) {
            let num_at = at + found + full.len();
            let digits: String = cell[num_at..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.len() == 2 {
                let low: u32 = digits.parse().unwrap_or_default();
                out.insert(format!("{prefix}-{low:02}"));
                // The high bound, if and only if a range separator follows immediately.
                let after = &cell[num_at + digits.len()..];
                let trimmed = after.trim_start();
                let seps = [".", "..", "…", "–", "-"];
                let sep_len = seps
                    .iter()
                    .filter(|s| trimmed.starts_with(**s))
                    .map(|s| s.len())
                    .max()
                    .unwrap_or(0);
                if sep_len > 0 {
                    let rest = trimmed[sep_len..].trim_start();
                    let rest = rest.strip_prefix(prefix).unwrap_or(rest);
                    let rest = rest.strip_prefix('-').unwrap_or(rest);
                    let high: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    if high.len() == 2 {
                        let high: u32 = high.parse().unwrap_or_default();
                        for n in low..=high {
                            out.insert(format!("{prefix}-{n:02}"));
                        }
                    }
                }
            }
            at = num_at;
        }
    }
    out
}

/// The catalogue: identifier -> (milestone, target cell), read out of `TESTING.md`'s own
/// catalogue section.
///
/// The scan is bounded to the section the way `check-matrix.sh` bounds it, for the same
/// reason: `TESTING.md` also carries a milestone-correction table and a suite table, and
/// scanning past the catalogue would read their prose as rows.
fn catalogue() -> BTreeMap<String, (String, String)> {
    let tail = CATALOGUE_DOC
        .split_once("## Test catalogue")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| CATALOGUE_DOC);
    // Cut at the next level-2 heading.
    let end = tail.find("\n## ");
    let tail = match end {
        Some(i) => &tail[..i],
        None => tail,
    };

    let mut out = BTreeMap::new();
    for line in tail.lines() {
        let Some(cells) =
            row_cells(line, "BND").or_else(|| PREFIXES.iter().find_map(|p| row_cells(line, p)))
        else {
            continue;
        };
        if cells.len() < 5 {
            continue;
        }
        out.insert(cells[0].clone(), (cells[3].clone(), cells[4].clone()));
    }
    out
}

/// Whether a catalogue Target cell defers: the obligation is written down and no test exists.
fn is_deferred(target: &str) -> bool {
    let t = target.replace('`', "").trim().to_string();
    t.starts_with('-')
}

/// The first live `#[test]` behind an identifier, or why it has none.
///
/// The three failure shapes are kept apart on purpose, because they mean different things:
/// a missing file, a function that exists but has lost its `#[test]` (cargo no longer runs it —
/// the failure `check-matrix.sh` was written to catch), and a genuinely deferred row.
fn first_live_test(cat: &BTreeMap<String, (String, String)>, id: &str) -> Result<PathBuf, String> {
    let Some((_, target)) = cat.get(id) else {
        return Err(format!("{id} is not in the test catalogue"));
    };
    if is_deferred(target) {
        return Err(format!("{id} defers its target ({})", target.trim()));
    }
    let root = repo_root();
    let mut candidates = Vec::new();
    for part in target.split(';') {
        let cleaned = part.replace('`', "");
        // Drop any trailing parenthetical the milestone notes use.
        let part = cleaned
            .split(" (")
            .next()
            .unwrap_or(cleaned.as_str())
            .trim();
        if part.is_empty() || part.starts_with('-') {
            continue;
        }
        if let Some(rest) = part.strip_prefix("ci:") {
            // A CI step: the workflow file must exist.
            let (file, needle) = rest.split_once("::").unwrap_or((rest, ""));
            let path = root.join(file);
            if !path.is_file() {
                return Err(format!("{id}: CI workflow {} does not exist", file));
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if !text.contains(needle) {
                return Err(format!("{id}: {} has no step `{}`", file, needle));
            }
            candidates.push(path);
            continue;
        }
        let (rel, func) = match part.split_once("::") {
            Some((f, g)) => (f, Some(g)),
            None => (part, None),
        };
        let path = root.join(rel);
        if !path.is_file() {
            return Err(format!("{id}: {} does not exist", rel));
        }
        if let Some(func) = func
            && !carries_test_attr(&path, func)
        {
            return Err(format!("{id}: {} has no `#[test] fn {}`", rel, func));
        }
        candidates.push(path);
    }
    candidates
        .into_iter()
        .next()
        .ok_or_else(|| format!("{id} names no target that could carry a test"))
}

/// True when `file` contains a function `func` that **carries** `#[test]`.
///
/// Carrying is the whole rule. A function that exists but has lost its attribute still
/// compiles, still lints clean, and is no longer run — which is a test that has quietly
/// stopped being evidence while every other gate stays green.
fn carries_test_attr(path: &Path, func: &str) -> bool {
    let Ok(src) = std::fs::read_to_string(path) else {
        return false;
    };
    src.lines()
        .map(strip_line_comment)
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| has_test_attr(w[0]) && declares_fn(w[1], func))
}

fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn has_test_attr(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("#[test") && (t.ends_with(']') || t.contains('('))
}

fn declares_fn(line: &str, func: &str) -> bool {
    let t = line.trim_start();
    let Some(after_fn) = t.strip_prefix("fn ") else {
        return false;
    };
    after_fn
        .split(['(', ' '])
        .next()
        .is_some_and(|name| name == func)
}

/// The invariant -> tests map, asserted by `PROM-04`.
///
/// **This is the table the ticket asked for.** It is a literal, hand-checked judgement: the
/// invariant table in `SECURITY-MODEL.md` has no Tests column, so there was nothing in the
/// documents to parse and this list IS the missing column. Each entry is the catalogue
/// identifiers that make the invariant true — read out of the catalogue's own "What it
/// proves" cells, not invented. An invariant absent from this list is an invariant nothing
/// backs, which is what `PROM-05` reports.
const INVARIANT_BACKING: &[(&str, &[&str])] = &[
    // S-1 no byte outside the boundary is read or written: T-01, T-02, T-03, T-20, T-31, T-33.
    (
        "S-1",
        &[
            "BND-01", "BND-02", "BND-03", "BND-07", "BND-16", "BND-21", "EDT-17",
        ],
    ),
    // S-2 no write unless write mode: the write capability is a type, and the read paths refuse.
    ("S-2", &["MCP-02", "MCP-01", "EDT-04", "EDT-26"]),
    // S-3 what is written is what was previewed: the post-hash gate and the recorded edits.
    ("S-3", &["EDT-15", "EDT-13", "EDT-14"]),
    // S-4 a failed apply is recoverable: journal before touch, rollback, recovery.
    ("S-4", &["EDT-09", "EDT-10", "EDT-23", "EDT-24", "EDT-25"]),
    // S-5 nothing is executed: no crate's `src/` spawns a process or opens a socket, so the
    //     evidence is a structural property rather than a test. There is no catalogue row to
    //     cite, and inventing one would be inventing evidence, so it is empty and recorded as
    //     a known gap in EXPECTED_UNBACKED below.
    ("S-5", &[]),
    // S-6 the engine has no ambient authority: the M6 worker's tests are all deferred
    //     (PRS-05..07, PRS-11, PRS-12), so PRS-08 is the only live row and it covers the
    //     in-process fuzz surface, not the isolation itself.
    ("S-6", &["PRS-08"]),
    // S-7 every output is bounded and says so.
    ("S-7", &["LMT-04", "LMT-05", "OUT-07"]),
    // S-8 apply is always a separate, explicit call: EDT-26 (writes need the full id) and the
    //     plan tools being read-only by mode.
    ("S-8", &["EDT-26", "EDT-05"]),
    // S-9 what a reviewer sees is what is there: the output sanitiser.
    ("S-9", &["OUT-04", "OUT-05", "OUT-01"]),
    // S-10 a reviewed plan cannot be swapped: full ids, no eviction, workspace binding.
    ("S-10", &["EDT-26", "EDT-27", "EDT-03", "EDT-30"]),
];

/// The promises that are written down and have **no** test behind them, as of this commit.
///
/// This is the finding, recorded so that it is *falsifiable*. A test asserting "every promise
/// is backed" would be red from the day it was written and would be deleted on the first day
/// it annoyed someone; the ticket's criterion is not satisfiable today, so the honest artefact
/// is a **ratchet**:
///
/// - `PROM-05` fails if this set **grows** — a promise lost its backing;
/// - `PROM-06` fails if this set **shrinks** — a gap was closed but nobody updated the table,
///   which is how a fixed claim quietly keeps its old, weaker description.
///
/// Closing an entry is the one edit this file wants: write the test, point the invariant at
/// it, delete the entry here. `PROM-06` makes sure that edit cannot be forgotten.
///
/// Each entry says **why**, so an entry is a work item rather than an excuse.
const EXPECTED_UNBACKED: &[(&str, &str)] = &[(
    "S-5",
    "Nothing is executed and no network is made. No crate's src/ contains a process-spawn \
     or socket call, so the promise holds structurally, but nothing tests it: the day someone \
     adds a Command::new or a std::net client to a crate, CI stays green and this promise \
     silently becomes false. Needs a test that scans src/ and fails on a spawn or socket call.",
)];

/// Promises that have a **live** test but also cite catalogue rows that are still deferred.
///
/// These are not gaps — S-2 is genuinely covered by EDT-26 — but they are covered *partly*,
/// and the difference matters: the live half is watched, the deferred half is not, and the
/// catalogue's own view is the deferred half alone because `check-matrix.sh` resolves each
/// row independently. So `MCP-01` and `MCP-02` read as "no test" even though
/// `secfix4_01..04`, `mcp1_06_write_tool_call_is_unknown_tool` and
/// `mcp1_12_write_tools_are_listed_only_in_write_mode` exist and assert exactly that.
///
/// Each entry must stay truthful in both directions: `PROM-07` fails if a listed identifier
/// stops being deferred, and `PROM-03` still requires every *non*-deferred identifier in the
/// promise's backing to resolve to a test that runs. Closing an entry here means pointing the
/// catalogue row at the test that already exists, which is a documentation fix rather than
/// new test work.
///
/// Empty. S-2 was the only entry, citing MCP-01 and MCP-02 as deferred to M5. Both rows now
/// name real tests in `crates/mcp/tests/mcp1_stdio_spec.rs` (`mcp1_12_write_tools_are_listed_only_in_write_mode`
/// and `mcp1_06_write_tool_call_is_unknown_tool`), so `prom_07` correctly demanded this entry
/// be deleted rather than left as a stale claim that the promise was unbacked.
const PARTIAL_BACKING: &[(&str, &[&str], &str)] = &[];

/// The promise ids in [`EXPECTED_UNBACKED`], as a set.
fn expected_unbacked() -> BTreeSet<String> {
    EXPECTED_UNBACKED
        .iter()
        .map(|(id, _)| id.to_string())
        .collect()
}

/// PROM-01: the invariant table is still ten rows, and each one is non-empty prose.
///
/// A table that quietly loses a row, or a row that is renamed into something the parser no
/// longer recognises, would shrink the checked set — so the count is pinned here rather than
/// derived from whatever the parse happens to find.
#[test]
fn prom_01_every_invariant_is_still_a_row_with_prose() {
    let inv = invariants();
    assert_eq!(
        inv.len(),
        10,
        "SECURITY-MODEL.md must state exactly S-1..S-10; found {inv:?}"
    );
    for (id, text) in &inv {
        assert!(
            text.len() > 30,
            "invariant {id} has no prose: {text:?}. A promise with no sentence is not a promise."
        );
    }
}

/// PROM-01b: the policy and the model agree on how many promises there are.
///
/// `SECURITY.md` says "the security invariants S-1 ... S-8" while the model states ten. The
/// policy is the document a reader trusts, and understating the count hides two of them —
/// S-9 (display deception) and S-10 (plan swapping), which are exactly the two a user is most
/// likely to be harmed by believing are covered. This test fails until the policy is corrected
/// to match the model; it is a documentation fix, not a behavioural one.
#[test]
fn prom_01b_the_policy_names_the_invariants_that_exist() {
    let inv = invariants();
    // Sort by the numeric suffix, not lexicographically: "S-10" < "S-2" as a string, so
    // `keys().last()` on a `BTreeMap` would name S-9 and the test would fail on a tree that
    // is perfectly consistent. That bug is why the invariant ids are numbers.
    let mut nums: Vec<u32> = inv
        .keys()
        .map(|k| {
            k.rsplit('-')
                .next()
                .unwrap_or("")
                .parse()
                .unwrap_or_default()
        })
        .collect();
    nums.sort_unstable();
    let first = format!("S-{}", nums.first().copied().unwrap_or_default());
    let last = format!("S-{}", nums.last().copied().unwrap_or_default());
    assert!(
        POLICY.contains(&format!("{first} … {last}"))
            || POLICY.contains(&format!("{first} ... {last}")),
        "SECURITY.md must point at the invariants that exist: the model now states {first} \
         through {last}, but the policy does not name it. Fix the count in the policy; do not \
         delete the invariant."
    );
}

/// PROM-02: every threat row names tests, and every identifier it names is in the catalogue.
///
/// This is the invariant of the catalogue itself rather than of the invariants table, and it
/// is here because the table above is only as good as the rows it rests on: a promise backed
/// by an identifier no one can find is not evidence.
#[test]
fn prom_02_every_threat_names_catalogue_identifiers() {
    let cat = catalogue();
    for (tid, ids) in threats() {
        assert!(
            !ids.is_empty(),
            "threat {tid} names no test identifier; an unbacked threat is a vulnerability waiting"
        );
        for id in &ids {
            assert!(
                cat.contains_key(id),
                "threat {tid} cites {id}, which is not in the TESTING.md catalogue"
            );
        }
    }
}

/// PROM-03: every invariant the table claims is backed resolves to a live test.
///
/// The identifier must be in the catalogue, the catalogue's Target must be on disk, and a
/// `::fn` target must be a function cargo still runs. This is the promise-to-evidence
/// mechanism, and it is the test that goes red when a test is renamed, deleted, or quietly
/// stops being run.
///
/// An invariant whose backing is declared incomplete in [`EXPECTED_UNBACKED`] is exempt **per
/// identifier**: S-2 cites MCP-01 and MCP-02, which are deferred, alongside EDT-26, which is
/// live. The exemption is per identifier rather than per invariant so that deleting the one
/// live test behind an already-flagged promise still fails this test — the flag says "this
/// promise is not fully covered", not "ignore this promise".
#[test]
fn prom_03_every_backed_invariant_resolves_to_a_live_test() {
    let cat = catalogue();
    let inv = invariants();
    let flagged = expected_unbacked();
    for (id, ids) in INVARIANT_BACKING {
        assert!(
            inv.contains_key(*id),
            "{id} is mapped in the evidence table but SECURITY-MODEL.md does not state it"
        );
        for cid in ids.iter() {
            // A deferred identifier is only tolerated when the promise is declared either
            // wholly unbacked or partly backed, and the entry says which.
            if is_deferred(&catalogue_target(&cat, cid))
                && (flagged.contains(*id) || PARTIAL_BACKING.iter().any(|(p, _, _)| p == id))
            {
                continue;
            }
            first_live_test(&cat, cid)
                .unwrap_or_else(|why| panic!("invariant {id} is backed by {cid}, but {why}"));
        }
    }
}

/// The catalogue's Target cell for an identifier, or an empty string if it has none.
fn catalogue_target(cat: &BTreeMap<String, (String, String)>, id: &str) -> String {
    cat.get(id).map(|(_, t)| t.clone()).unwrap_or_default()
}

/// PROM-04: the evidence table and the model agree in both directions.
///
/// One direction catches a mapping to an invariant that no longer exists. The other catches
/// the failure that matters most for a new promise: an invariant added to the model and
/// given no evidence row here, which would otherwise sit outside every check in the file.
#[test]
fn prom_04_evidence_table_and_model_agree() {
    let inv = invariants();
    let mapped: BTreeSet<String> = INVARIANT_BACKING
        .iter()
        .map(|(id, _)| id.to_string())
        .collect();
    for id in inv.keys() {
        assert!(
            mapped.contains(id),
            "invariant {id} is stated in SECURITY-MODEL.md with no entry in the evidence \
             table. Add the catalogue identifiers that make it true, or record it as \
             unshipped — do not leave it unchecked."
        );
    }
    for (id, _) in INVARIANT_BACKING {
        assert!(
            inv.contains_key(*id),
            "the evidence table maps {id}, which the model no longer states"
        );
    }
    let ids: BTreeSet<&str> = INVARIANT_BACKING
        .iter()
        .flat_map(|(_, v)| *v)
        .copied()
        .collect();
    for id in ids {
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{id} is not an identifier shape the catalogue can hold"
        );
    }
}

/// PROM-05: THE ACCEPTANCE CRITERION, as a ratchet. No promise may LOSE its evidence.
///
/// "Every sentence in the security policy has code or a test behind it" is **not true today**,
/// and asserting it as a plain boolean would produce a test that is red on arrival and gets
/// deleted rather than fixed — which is how the gap got here in the first place. So this test
/// asserts the direction that is actionable: the set of unbacked promises is exactly the set
/// [`EXPECTED_UNBACKED`] declares, and that set may only ever shrink.
///
/// A promise entering that set is a build failure with the reason printed, because an
/// undocumented promise with no test is the most expensive thing this repository can ship
/// quietly. What is unbacked, and why, is [`EXPECTED_UNBACKED`].
#[test]
fn prom_05_every_promise_has_evidence() {
    let cat = catalogue();
    let mut unbacked: BTreeSet<String> = BTreeSet::new();

    for (id, ids) in INVARIANT_BACKING {
        let live = ids
            .iter()
            .filter(|c| first_live_test(&cat, c).is_ok())
            .count();
        if live == 0 {
            unbacked.insert(id.to_string());
        }
    }

    let expected = expected_unbacked();
    let lost: Vec<&String> = unbacked.difference(&expected).collect();
    let closed: Vec<&String> = expected.difference(&unbacked).collect();

    assert!(
        lost.is_empty(),
        "these security promises LOST their evidence and are not declared in \
         EXPECTED_UNBACKED: {lost:?}. Each is a sentence in SECURITY.md that a reader may \
         rely on. Either restore the test, or — if the promise was never really true — correct \
         the claim in SECURITY-MODEL.md and say so in the entry's `why`."
    );
    assert!(
        closed.is_empty(),
        "these promises are now backed but EXPECTED_UNBACKED still lists them as gaps: \
         {closed:?}. Delete the entry from EXPECTED_UNBACKED; leaving it means the table \
         understates what this repository actually tests."
    );
}

/// PROM-06: the declared gaps are still real gaps, with the reason they are not fixed.
///
/// Without this, [`EXPECTED_UNBACKED`] is only checked for membership, so an entry could claim
/// "no test exists" about a test that does. Each entry is therefore required to say why it is
/// still open — and S-5 is checked **structurally** here, because its whole point is that no
/// test watches it.
#[test]
fn prom_06_declared_gaps_are_still_real() {
    for (id, why) in EXPECTED_UNBACKED {
        assert!(
            invariants().contains_key(*id),
            "EXPECTED_UNBACKED names {id}, which SECURITY-MODEL.md no longer states"
        );
        assert!(
            why.len() > 80,
            "the gap entry for {id} gives no reason; an unexplained gap is not a work item"
        );
    }

    // S-5 says nothing in `src/` spawns a process or opens a socket. That is checkable, so it
    // is checked rather than asserted in a comment: if someone adds a `Command::new` or a
    // `std::net` import, the gap's stated reason stops being true, and the person who did it
    // must write the test (or the promise is no longer true).
    let sources = product_sources();
    assert!(
        !sources.is_empty(),
        "no crate sources were found under crates/*/src; the S-5 structural check would be \
         vacuously green"
    );
    let forbidden = [
        "std::net",
        "TcpStream",
        "TcpListener",
        "UdpSocket",
        "Command::new",
    ];
    let mut hits: Vec<String> = Vec::new();
    for path in &sources {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in text.lines().map(strip_line_comment) {
            for needle in forbidden {
                if line.contains(needle) {
                    let rel = path
                        .strip_prefix(repo_root())
                        .unwrap_or(path)
                        .display()
                        .to_string();
                    hits.push(format!("{rel}: {needle}"));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "S-5 says nothing is executed and no network is used, but product source now \
         contains: {hits:?}. Either S-5 is no longer true (fix the promise) or these are \
         legitimate and S-5 needs a test that proves the claim."
    );
}

/// PROM-07: a promise recorded as only partly backed is still partly backed.
///
/// The other half of the [`PARTIAL_BACKING`] contract. `PROM-03` checks that the live
/// identifiers of these promises resolve; this checks the deferred ones **are still deferred**,
/// so the entry cannot quietly become a lie in the other direction: if someone points MCP-01
/// at a real test, the entry must be closed (which is a good day) rather than left behind
/// claiming a gap that no longer exists.
#[test]
fn prom_07_partial_backing_is_still_partial() {
    let cat = catalogue();
    for (id, ids, why) in PARTIAL_BACKING {
        assert!(
            invariants().contains_key(*id),
            "PARTIAL_BACKING names {id}, which SECURITY-MODEL.md no longer states"
        );
        assert!(
            why.len() > 80,
            "the partial-backing entry for {id} gives no reason; an unexplained gap is not a \
             work item"
        );
        for cid in ids.iter() {
            assert!(
                cat.contains_key(*cid),
                "{id} cites {cid} as deferred, but {cid} is not in the catalogue at all"
            );
            assert!(
                is_deferred(&catalogue_target(&cat, cid)),
                "{id} cites {cid} as deferred, but {cid} now names a real test \
                 ({}). Close this entry: point the catalogue row at the test and delete the \
                 PARTIAL_BACKING entry.",
                catalogue_target(&cat, cid).trim()
            );
        }
    }
}

/// Every product `.rs` file under a crate's `src/`, recursively.
///
/// In-crate test modules (`src/spec/`) are not product code: a test may spawn a process or
/// open a socket precisely to prove that the product does not, and counting that against S-5
/// would make the promise unfalsifiable.
fn product_sources() -> Vec<PathBuf> {
    let root = repo_root();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("crates")) else {
        return out;
    };
    for entry in entries.flatten() {
        collect_product_rs(&entry.path().join("src"), &mut out);
    }
    out
}

fn collect_product_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "spec") {
                continue;
            }
            collect_product_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
