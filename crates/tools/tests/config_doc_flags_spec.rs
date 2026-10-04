//! `docs/CONFIGURATION.md`'s flag table must be the flag set `parse_args` accepts.
//!
//! # Why this file exists
//!
//! `CONFIGURATION.md` documented seven flags. `crates/mcp/src/main.rs::parse_args`
//! accepted four: `--workspace`, `--allow-write`, `--config`, `--help`/`-h`, and
//! returned `unknown argument` for anything else — which aborts startup. So
//! `--read-root`, `--state-dir`, `--languages`, `--isolation` and `--log-level`
//! were all documented as available and all fatal to pass, while `--config` existed
//! in code and was absent from the document.
//!
//! The defect class here is the same one `config_doc_example_spec.rs` guards for
//! settings: a document that states a fact about code, with nothing that can fail
//! when the two disagree. This file is that something, for flags.
//!
//! - `CFG-FLAG-01` the **implemented** table names exactly the flags `parse_args`
//!   accepts, in both directions. A flag added to the code without a table row fails
//!   the "missing from the doc" half; a flag documented that the parser refuses fails
//!   the "not implemented" half.
//! - `CFG-FLAG-02` no flag from the not-implemented table has quietly become real.
//!   This is deliberately the opposite polarity of 01: the moment one of those five
//!   lands, the document's "not implemented, the server does not start" sentence
//!   becomes a lie, and this goes red to force it to be rewritten.
//!
//! The flag set is recovered from `parse_args`' own `match` arms rather than
//! transcribed, so this file cannot disagree with the parser by being stale about the
//! parser's spelling.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

const DOC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/CONFIGURATION.md"
));
const MAIN_RS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../mcp/src/main.rs"));

fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/tools is two levels below the repo root")
        .to_path_buf()
}

/// Every string literal `parse_args` matches as an accepted argument.
///
/// Read from the `match arg.as_str()` arms inside `parse_args`. The rule is the
/// narrow one that matches what the function actually does: a quoted literal that
/// **begins with a dash** is an accepted flag, whatever the arm's body does with it
/// (`=> allow_write = true`, `=> { ... }`, or a `| "-h" =>` alternative). The refusal
/// arm is spelled `other => ...` with no literal, so it contributes nothing — which is
/// exactly the distinction the document has to get right: a flag absent from this set
/// is one the server refuses and exits on.
///
/// Only quoted *tokens* are collected, never prose, so the help text
/// ("Usage: opencrayast-mcp --workspace <dir>") and the error strings
/// ("missing value for --workspace") cannot read as flags.
fn accepted_flags() -> BTreeSet<String> {
    let start = MAIN_RS
        .find("fn parse_args")
        .expect("parse_args exists in crates/mcp/src/main.rs");
    let body = &MAIN_RS[start..];
    // Bounded to the function: a `match` arm in some later function must not be
    // mistaken for one of parse_args'.
    let end = body[1..].find("\nfn ").map(|i| i + 1).unwrap_or(body.len());
    let body = &body[..end];

    let mut out = BTreeSet::new();
    for raw in body.lines() {
        let line = raw.trim();
        if line == "other =>" {
            continue;
        }
        for lit in line.split('|') {
            let lit = lit.trim();
            let Some(name) = lit
                .strip_prefix('"')
                .and_then(|r| r.split_once('"'))
                .map(|(n, _)| n)
            else {
                continue;
            };
            if name.starts_with('-') {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// The flag tokens named in the tables between `from` and `to`.
///
/// Bounded on BOTH ends by a marker, because these two tables sit in the same section
/// under the same `## ` heading: cutting at the next `## ` would capture the
/// not-implemented table too, which is the opposite table and the opposite claim.
///
/// The markers are HEADINGS, not prose sentences. An earlier revision of this file
/// anchored on two sentences of running text ("`opencrayast-mcp` accepts **exactly**
/// these four arguments and errors on anything"), which broke the moment the
/// paragraph was reworded — the test then failed with "must contain the marker" while
/// the flag set it was checking had not changed at all. A heading is the structure the
/// two tables are actually organised around, and it is what a reworded paragraph
/// leaves alone.
fn table_flags_between(from: &str, to: &str, include_end: bool) -> BTreeSet<String> {
    let start = DOC
        .find(from)
        .unwrap_or_else(|| panic!("CONFIGURATION.md must contain the marker {from}"));
    let after = start + from.len();
    let rel_end = DOC[after..]
        .find(to)
        .unwrap_or_else(|| panic!("CONFIGURATION.md must contain the marker {to} after {from}"));
    let end = after + rel_end;
    let _ = include_end;
    let body = &DOC[start..end];

    let mut out = BTreeSet::new();
    for line in body.lines() {
        let t = line.trim();
        if !t.starts_with('|') || t.starts_with("|---") {
            continue;
        }
        let first = t.trim_start_matches('|').split('|').next().unwrap_or("");
        let first = first.trim();
        // The header row's first cell is literally "Flag".
        if first == "Flag" || first.is_empty() {
            continue;
        }
        // `--- `, `-h` are separate tokens inside one cell, and a value placeholder is
        // separated from the flag by a space (`--workspace DIR`).
        for tok in first.split(['`', ',', ' ']) {
            let tok = tok.trim();
            if tok.starts_with("--") || tok == "-h" {
                out.insert(tok.to_string());
            }
        }
    }
    out
}

/// The flags of the implemented table: the ones `parse_args` accepts.
fn implemented_table_flags() -> BTreeSet<String> {
    table_flags_between(
        "## Flags\n",
        "### Flags that are documented elsewhere but not implemented",
        false,
    )
}

/// The flags of the not-implemented table.
fn planned_table_flags() -> BTreeSet<String> {
    table_flags_between(
        "### Flags that are documented elsewhere but not implemented",
        "The same holds for `[protect] extra`",
        false,
    )
}

/// CFG-FLAG-01: the implemented table is exactly the accepted set, both directions.
#[test]
fn cfg_flag_01_the_implemented_table_is_exactly_the_accepted_flag_set() {
    let accepted = accepted_flags();
    assert!(
        accepted.len() >= 4,
        "only {accepted:?} was recovered from parse_args; the scan is too narrow and this \
         test would pass vacuously"
    );

    let documented = implemented_table_flags();

    let undocumented: Vec<&String> = accepted.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "parse_args accepts {undocumented:?} but CONFIGURATION.md's implemented table does \
         not list {undocumented:?}. A flag that exists but is undocumented is exactly how \
         --config went missing."
    );

    let nonexistent: Vec<&String> = documented.difference(&accepted).collect();
    assert!(
        nonexistent.is_empty(),
        "CONFIGURATION.md's implemented table lists {nonexistent:?}, which parse_args does \
         NOT accept — passing it prints `unknown argument` and the server refuses to start. \
         Move it to the not-implemented table, or implement it."
    );
}

/// CFG-FLAG-02: the not-implemented table has not quietly become real.
#[test]
fn cfg_flag_02_the_not_implemented_table_is_still_not_implemented() {
    let accepted = accepted_flags();
    let planned = planned_table_flags();
    // The floor exists so a broken scan cannot make this test vacuous — not to pin a count the
    // product is expected to keep. It was 5 while `--read-root` was still listed here; that flag
    // is implemented now (4de4e0c), so 4 is the honest floor. The `len() >= 4` with the message
    // naming the set keeps the guard: a scan that found nothing, or one flag, still fails.
    assert!(
        planned.len() >= 4,
        "only {planned:?} was found under the not-implemented heading; the scan found fewer \
         flags than the table documents, so this would pass vacuously"
    );
    for flag in &planned {
        assert!(
            !accepted.contains(flag),
            "{flag} is listed as NOT implemented, but parse_args accepts it. The \
             document's 'the server does not start' sentence is now false: move {flag} up \
             into the implemented table and document what it does."
        );
    }
}

/// CFG-FLAG-03: the document's own claim about which environment it reads is checkable.
///
/// The prose says no `OPENCRAYAST_*` variable is read anywhere in `crates/`. That is a
/// falsifiable claim about the tree, and it is the one the Finding 3 rewrite rests on:
/// if someone adds the first `OPENCRAYAST_*` read, the "there is no
/// environment-variable layer" paragraph becomes stale and this goes red.
///
/// Test-only helper variables (`OPENCRAYAST_FSIO_*`, `OPENCRAYAST_EACCES_HELPER`) are
/// used by `crates/core/tests/` to hand a re-exec helper its instructions; they live
/// under a `tests/` directory and are excluded here, because they are test harness
/// plumbing rather than configuration. Product `src/` is what the claim is about.
#[test]
fn cfg_flag_03_no_product_code_reads_an_opencrayast_variable() {
    let mut hits = Vec::new();
    let crates = repo_root().join("crates");
    for entry in std::fs::read_dir(&crates)
        .expect("crates/ exists")
        .flatten()
    {
        let src = entry.path().join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n == "spec") {
                        continue;
                    }
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    for (i, line) in text.lines().enumerate() {
                        // Strip line comments so a mention in prose is not a read.
                        let code = line.split("//").next().unwrap_or(line);
                        if code.contains("OPENCRAYAST_") {
                            hits.push(format!("{}:{}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "CONFIGURATION.md says no OPENCRAYAST_* variable is read by the program, but \
         product source now contains: {hits:?}. Either the environment layer has landed \
         (implement it and rewrite the section) or these are something else and the \
         claim needs rewording."
    );
}
