//! Spec for ISSUE-BENCH-TOKENS: the report is a measurement, so the measurement has to be
//! testable. A small fixed corpus is built here and the report is checked against numbers that
//! can be worked out by hand.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]

use opencrayast_tools::bench::{BenchConfig, run};
use std::fs;
use std::path::PathBuf;

/// A file whose outline is smaller than the file: three functions with real bodies. This is
/// not a rigged example - the outline of a file is one line per symbol, so a file only wins
/// once its symbols have bodies to leave out. The `TINY` fixture below is the same fact from
/// the other side.
const BIG: &str = "\
def alpha(values):
    total = 0
    for value in values:
        total += value
    return total


def beta(values):
    best = None
    for value in values:
        if best is None or value > best:
            best = value
    return best


def gamma(values):
    seen = set()
    for value in values:
        seen.add(value)
    return sorted(seen)
";

/// A file whose outline is bigger than the file: one assignment, and the outline adds a header
/// and a line about it.
const TINY: &str = "x = 1\n";

/// No grammar for this name, so it is never measured.
const UNSUPPORTED: &str = "plain text\n";

fn corpus() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("big.py"), BIG).unwrap();
    fs::write(d.path().join("tiny.py"), TINY).unwrap();
    fs::write(d.path().join("notes.txt"), UNSUPPORTED).unwrap();
    d
}

fn cfg(dir: &tempfile::TempDir) -> BenchConfig {
    BenchConfig {
        corpus: PathBuf::from(dir.path()),
        label: "test-corpus".to_string(),
        note: "a fixture".to_string(),
        seed: 42,
        max_files: 100,
        max_gets_per_file: 5,
    }
}

/// The report has the sections a reader needs to trust it, in the documented order.
#[test]
fn a_the_report_has_its_documented_sections() {
    let d = corpus();
    let report = run(&cfg(&d)).unwrap();
    let mut at = 0usize;
    for heading in [
        "## Corpus",
        "## Per language",
        "## Distribution of `outline/file`, all languages",
        "## Scenarios",
        "### Exploration: understand a directory",
        "### Retrieval: get one symbol per file",
        "## Where the tools lose",
        "## Limits, and where this is unfair",
        "## What these numbers do not mean",
        "## Method",
    ] {
        let found = report[at..]
            .find(heading)
            .unwrap_or_else(|| panic!("missing or out of order: {heading}\n{report}"));
        at += found + heading.len();
    }
    // The two sentences that keep the numbers honest.
    assert!(
        report.contains("ceil(bytes / 4)"),
        "the token estimate must say it is an estimate"
    );
    assert!(
        report.contains("**not** a task success rate"),
        "what the numbers are not"
    );
}

/// The corpus identity is a hash over sizes and paths, not over the directory it sits in: two
/// copies of the same files must produce the same report.
#[test]
fn b_the_corpus_id_does_not_depend_on_where_the_corpus_lives() {
    let d = corpus();
    let first = run(&cfg(&d)).unwrap();
    // The corpus id is the only 64-hex-digit cell in the table.
    let id_of = |r: &str| {
        r.lines()
            .flat_map(|l| l.split('|'))
            .map(|c| c.trim().trim_matches('`'))
            .find(|c| c.len() == 64 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
            .unwrap_or_else(|| panic!("no corpus id in:\n{r}"))
            .to_string()
    };
    // Copy the corpus somewhere else entirely.
    let elsewhere = tempfile::tempdir().unwrap();
    for name in ["big.py", "tiny.py", "notes.txt"] {
        fs::copy(d.path().join(name), elsewhere.path().join(name)).unwrap();
    }
    let second = run(&cfg(&elsewhere)).unwrap();
    assert_eq!(id_of(&first), id_of(&second), "same files, same id");
    assert_eq!(first, second, "same files, same report");

    // Change one byte of one file and the id must change.
    fs::write(elsewhere.path().join("tiny.py"), "x = 2\n").unwrap();
    let third = run(&cfg(&elsewhere)).unwrap();
    assert_ne!(
        id_of(&first),
        id_of(&third),
        "a changed file is a different corpus"
    );
}

/// Same seed, same report, byte for byte. This is what makes a committed report a fact about
/// the corpus rather than about the machine.
#[test]
fn c_the_same_seed_gives_byte_identical_reports() {
    let d = corpus();
    let a = run(&cfg(&d)).unwrap();
    let b = run(&cfg(&d)).unwrap();
    assert_eq!(a, b);

    // A different seed picks different symbols for the retrieval row, so that row must change
    // while the corpus identity does not.
    let other = BenchConfig {
        seed: 43,
        ..cfg(&d)
    };
    let c = run(&other).unwrap();
    assert_ne!(a, c, "a different seed must pick different symbols");
}

/// The saving ratio is the tool's bytes over the file's bytes, and a file whose outline is
/// BIGGER is counted, not hidden: `tiny.py` is two bytes of outline over a file of six.
#[test]
fn d_the_ratio_is_outline_over_file_and_the_losers_are_counted() {
    let d = corpus();
    let report = run(&cfg(&d)).unwrap();
    assert!(report.contains("BIGGER than the file itself"), "{report}");
    // The corpus: two measured Python files (the .txt has no grammar), and their sizes.
    let big_len = BIG.len() as u64;
    let tiny_len = TINY.len() as u64;
    assert!(
        report.contains(&format!("| 2 | {} | ", big_len + tiny_len)),
        "files measured and bytes measured:\n{report}"
    );
    // Every losing file is listed by name, so the count can be checked.
    assert!(report.contains("tiny.py"), "{report}");
    let losers = report
        .lines()
        .find(|l| l.contains("BIGGER than the file itself"))
        .expect("a section that says where the tools lose");
    assert!(
        losers.starts_with("1 of 2"),
        "exactly one of the two files loses: {losers}"
    );

    // And the retrieval row is a real subtraction: the tool's bytes are counted against the
    // files that were read, so the saved figure is exactly their difference.
    let row = retrieval_row(&report);
    let cells: Vec<&str> = row.iter().map(String::as_str).collect();
    assert_eq!(cells.len(), 6, "the retrieval row: {row:?}");
    let num = |c: &str| -> u64 {
        c.replace(',', "")
            .parse()
            .unwrap_or_else(|e| panic!("{row:?}: {e}"))
    };
    let read: u64 = num(cells[1]);
    let got: i64 = num(cells[2]) as i64;
    let saved: i64 = num(cells[4]) as i64;
    assert_eq!(read, big_len + tiny_len, "the baseline is the files read");
    assert_eq!(saved, read as i64 - got, "saved is read minus returned");
    assert!(got > 0, "the symbol was really fetched");
}

/// The retrieval row, found by its header, without depending on the rest of the table.
fn retrieval_row(report: &str) -> Vec<String> {
    let mut lines = report
        .lines()
        .skip_while(|l| !l.contains("| `ast_get` (sum) |"));
    lines.next(); // the header
    lines.next(); // the |---| separator
    lines
        .next()
        .expect("a retrieval row under the header")
        .split('|')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// The report never names the machine it ran on: the corpus appears as `$CORPUS` and a label,
/// and a corpus path would be a personal path in a committed document.
#[test]
fn e_the_report_contains_no_corpus_path() {
    let d = corpus();
    let report = run(&cfg(&d)).unwrap();
    let path = d.path().to_str().unwrap();
    assert!(!report.contains(path), "the corpus path leaked:\n{report}");
    assert!(
        report.contains("$CORPUS"),
        "how to reproduce is still there"
    );
    assert!(report.contains("test-corpus"), "the label is");
}

/// A corpus with no file that can be outlined still produces a report, and it says so.
#[test]
fn f_an_empty_corpus_reports_nothing_rather_than_failing() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("notes.txt"), UNSUPPORTED).unwrap();
    let report = run(&cfg(&d)).unwrap();
    assert!(report.contains("| 0 | 0 | 0 |"), "{report}");
    assert!(
        report.contains("| Files with a grammar | Files measured |"),
        "{report}"
    );
}

/// The header names exactly what the corpus id hashes, so a reader can recompute it, and it
/// matches what the committed docs/BENCHMARKS.md says (the label drifted once).
#[test]
fn g_the_corpus_id_label_matches_what_is_hashed_and_the_committed_doc() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("tiny.py"), "def f():\n    pass\n").unwrap();
    let report = run(&cfg(&d)).unwrap();
    let label = "Corpus id (sha256 of `size\\tpath\\tcontent`)";
    assert!(report.contains(label), "{report}");
    let doc = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/BENCHMARKS.md"
    ))
    .unwrap();
    assert!(
        doc.contains(label),
        "docs/BENCHMARKS.md must use the same label"
    );
}
