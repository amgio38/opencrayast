//! PAT-06: results and edit sets are identical across runs and operating systems.
//!
//! Add cases; never weaken these.
//!
//! This file is the EVIDENCE for PAT-06. It exists because the row used to cite
//! `pattern_match_spec.rs::rust_patterns` instead - a hand-written Rust
//! expectation set that cannot observe the operating system at all.
//!
//! It lives in `crates/edit/tests/` rather than `crates/query/tests/` because the
//! row covers BOTH halves - match results AND edit sets - and edit is the crate
//! that depends on query, so this is the only direction the two can be seen
//! together (`check-layering.sh`: `edit -> query`, never the reverse).
//!
//! # What PAT-06 claims
//!
//! Two different things, needing two different kinds of evidence:
//!
//! 1. **Determinism (in-process).** The same pattern over the same source yields
//!    the same matches, captures and edit sets on every run of the same binary.
//!    That IS reachable from a test: the pipeline is re-run over a corpus and
//!    every observable is compared. [`results_are_identical_on_every_run`] and
//!    [`edit_sets_are_identical_on_every_run`] do that.
//!
//! 2. **Platform invariance (cross-OS).** Those results do not change when the
//!    binary runs on Windows, macOS or Linux instead. This CANNOT be shown from
//!    inside one process: `query::pattern::search` takes `&str`, so the OS never
//!    reaches the code under test, and no Linux-only test can vary the OS it is
//!    running on. The evidence is these same tests RUN ON THREE RUNNERS, which is
//!    what `.github/workflows/ci.yml` does: its `cargo test --workspace` step
//!    carries no `if: matrix.os == ...` gate, so it runs on ubuntu-latest,
//!    macos-latest and windows-latest. PAT-06 therefore names this file twice -
//!    once for the in-process half, and once as a `ci:` target for the
//!    cross-platform half - and `check-matrix.sh` enforces both.
//!
//! # Platform-convention axes
//!
//! "The OS is the same" is a weak claim: a program can return one answer for
//! every input and still be platform-dependent. What actually differs between
//! Windows, macOS and Linux is the CONTENT a file can hold, so that is what the
//! property is parameterised over. The axes, and why each is a real difference:
//!
//! | Axis | Linux | Windows | macOS |
//! |---|---|---|---|
//! | line endings | LF | CRLF, and lone LF | LF (lone CR historically) |
//! | byte-order mark | usually absent | written by many editors | usually absent |
//! | text encoding | UTF-8 | UTF-8, BOM marks the file | UTF-8, sometimes MacRoman |
//! | Unicode form | as typed | as typed | filenames normalise toward NFD |
//! | path separators | `/` | `\` and `/` | `/` |
//!
//! The property under test is NOT "every input gives the same answer". It is the
//! property that must hold under each convention:
//!
//! > A source whose only difference is its platform convention (line endings,
//! > BOM, Unicode form) yields the SAME STRUCTURAL ANSWER - the same matches, the
//! > same captures by identity, in the same order - and the convention shows up
//! > only in the BYTE extents, which this project reports as byte offsets rather
//! > than character counts.
//!
//! That is a real property, and it can fail. [`platform_conventions_shift_only_byte_extents_not_structure`]
//! asserts both halves; [`the_unicode_axis_actually_moves_byte_extents`] is the
//! anti-theatre control - it pins the NFD offset that the property test relies on,
//! so a change in byte-offset semantics fails loudly instead of silently making
//! the property vacuous.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_edit::{RewriteRequest, apply_edits, rewrite_file};
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{Match, Pattern, SearchBudget, search};
use std::time::Duration;

fn pbudget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 24,
        timeout: Duration::from_secs(20),
        max_depth: 8192,
        max_nodes: 10_000_000,
    }
}

/// Everything an observer of a search can see, in a form `PartialEq` compares.
///
/// Deliberately MORE than the match list: a pipeline that produced the right
/// matches in the wrong ORDER, or that dropped `truncated`, is still caught.
/// `Match` is compared whole (it derives `PartialEq`), so positions and capture
/// kinds come along for free.
type Observable = (Vec<Match>, bool, u64);

fn observe(lang: Language, pat: &str, src: &str) -> Observable {
    let p = Pattern::compile(lang, pat).unwrap();
    let parsed = parse(lang, src, &pbudget()).unwrap();
    let out = search(&parsed, src, &p, None, &SearchBudget::default()).unwrap();
    (out.matches, out.truncated, out.steps_used)
}

/// The structural answer: which nodes matched, what was captured, in what order.
///
/// Byte offsets are excluded: a BOM or a CRLF shifts every offset without
/// changing anything an AST-aware tool means, so comparing raw offsets across
/// conventions compares the wrong thing. Offsets are compared separately, and
/// only against each other.
///
/// The RAW matched text is excluded too. `Match::text` is a VERBATIM slice of the
/// source, so the LF and CRLF variants legitimately hold different bytes: one
/// holds `"def add(a, b):\n    return a + b"` and the other holds
/// `"def add(a, b):\r\n    return a + b"`. Carrying the raw text here was the first
/// version's mistake: it made the structural comparison red on the very
/// convention the property says is invisible, which is a FALSE RED. What is
/// compared is the text with the ending NORMALISED, which is what "the same node
/// matched" means; the verbatim bytes are still checked, but against the source,
/// where they are meaningful.
///
/// `match_count` is kept in the tuple, so "found nothing" can never compare equal
/// to "found the right thing".
type Structure = (usize, Vec<(String, Vec<(String, String)>)>);

/// Normalise a source convention away, so two spellings of one file compare
/// equal. ONLY line endings and a leading BOM: nothing else is papered over.
fn normalise(text: &str) -> String {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
}

fn structure(obs: &Observable) -> Structure {
    (
        obs.0.len(),
        obs.0
            .iter()
            .map(|m| {
                (
                    normalise(&m.text),
                    m.captures
                        .iter()
                        .map(|c| (c.name.clone(), normalise(&c.text)))
                        .collect(),
                )
            })
            .collect(),
    )
}

fn structure_of(lang: Language, pat: &str, src: &str) -> Structure {
    structure(&observe(lang, pat, src))
}

/// One (language, pattern, source) case, run under every convention.
struct Case {
    lang: Language,
    pat: &'static str,
    /// The Unix-convention baseline: LF line endings, no BOM, NFC.
    lf: &'static str,
    /// The same source with line endings converted to CRLF.
    crlf: String,
    /// CRLF with a UTF-8 BOM prepended, as Windows editors write it.
    bom_crlf: String,
    /// The replacement used when this case is driven through the rewriter.
    repl: &'static str,
}

impl Case {
    fn new_with_repl(
        lang: Language,
        pat: &'static str,
        lf: &'static str,
        repl: &'static str,
    ) -> Case {
        let crlf = lf.replace('\n', "\r\n");
        let bom_crlf = format!("\u{feff}{crlf}");
        Case {
            lang,
            pat,
            lf,
            crlf,
            bom_crlf,
            repl,
        }
    }

    /// The same source under each platform convention.
    fn conventions(&self) -> Vec<(&'static str, String)> {
        vec![
            ("LF", self.lf.to_string()),
            ("CRLF", self.crlf.clone()),
            ("BOM+CRLF", self.bom_crlf.clone()),
        ]
    }
}

/// Six cases, one per language, so nothing here can be satisfied by one grammar.
fn cases() -> Vec<Case> {
    vec![
        Case::new_with_repl(
            Language::JavaScript,
            "console.log($$$ARGS)",
            "function f() {\n    console.log(\"start\", id);\n    console.log(x);\n}\n",
            "console.warn($$$ARGS)",
        ),
        Case::new_with_repl(
            Language::TypeScript,
            "const $X: $T = $V;",
            "const name: string = \"x\";\nconst n: number = 1;\nconst bad: = 2;\n",
            "let $X: $T = $V;",
        ),
        Case::new_with_repl(
            Language::Tsx,
            "const $X: $T = $V;",
            "const name: string = \"x\";\nconst n: number = 1;\n",
            "let $X: $T = $V;",
        ),
        Case::new_with_repl(
            Language::Rust,
            "fn $NAME($$$PARAMS) -> $RET { $$$BODY }",
            // TWO functions, so this case produces TWO edits and the edit ORDER is
            // observable. With one function it produced one edit, and a
            // `sort_by_key(|e| e.start)` -> reversed sort mutation passed it.
            "fn add(a: i32, b: i32) -> i32 { a + b }\nfn sub(a: i32, b: i32) -> i32 { a - b }\nfn noret() {}\n",
            "fn $NAME($$$PARAMS) -> $RET { $$$BODY } // touched\n",
        ),
        Case::new_with_repl(
            Language::Python,
            "def $F($$$P):\n    return $X",
            // TWO matching functions, so this case yields two edits (see the note on the
            // Rust case): `def noop()` has no `return`, so it does not match and
            // only one edit came out of it.
            "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n\ndef noop():\n    pass\n",
            "def $F($$$P):\n    return $X\n",
        ),
        Case::new_with_repl(
            Language::Go,
            "fmt.Println($$$A)",
            "package main\n\nfunc main() {\n\tfmt.Println(\"a\", 1)\n\tfmt.Println()\n}\n",
            "fmt.Printf($$$A)",
        ),
    ]
}

// ---- half 1: determinism, in-process --------------------------------------------

#[test]
fn results_are_identical_on_every_run() {
    // The repeat count is small on purpose. Repeating a deterministic function a
    // million times proves nothing extra; what it WOULD prove is that the
    // pipeline carries no state between runs. 64 runs is enough to catch an
    // accumulator, a lazily-initialised cache or a hash-order dependency, which
    // is the actual hazard - and it keeps the test cheap enough to stay in CI on
    // three runners.
    const REPEATS: usize = 64;

    for case in cases() {
        let baseline = observe(case.lang, case.pat, case.lf);
        for run in 1..=REPEATS {
            let again = observe(case.lang, case.pat, case.lf);
            assert_eq!(
                again,
                baseline,
                "{} case {:?} was not reproducible on run {run}",
                case.lang.id(),
                case.pat,
            );
        }
    }
}

#[test]
fn edit_sets_are_identical_on_every_run() {
    const REPEATS: usize = 64;

    for case in cases() {
        let baseline = rewrite(&case);
        // TWO edits minimum, not one. With a single edit the edit set is a
        // one-element vector and its ORDER is not observable, so a reversed sort
        // would pass: the first version of this test asserted only
        // `!is_empty()` and a `sort_by_key(|e| e.start)` mutation survived it.
        // Order is half of what "an edit set" means, so it has to be observable.
        assert!(
            baseline.1.len() >= 2,
            "{} case {:?} produced {} edit(s); at least two are needed for the edit ORDER \
             to be observable",
            case.lang.id(),
            case.pat,
            baseline.1.len(),
        );
        for run in 1..=REPEATS {
            let again = rewrite(&case);
            assert_eq!(
                again,
                baseline,
                "{} case {:?} produced a different edit set on run {run}",
                case.lang.id(),
                case.pat,
            );
        }
    }
}

#[test]
fn edits_are_emitted_in_document_order() {
    // The direct, readable statement of the property the previous test only
    // implied: an edit set is ORDERED, in document order, on every platform
    // convention. This is what makes the "at least two edits" guard meaningful.
    for case in cases() {
        for (label, variant) in case.conventions() {
            let (_, edits) = rewrite_src(case.lang, case.pat, case.repl, &variant);
            assert!(
                edits.len() >= 2,
                "{} under {label}: need two edits to check an order",
                case.lang.id(),
            );
            for pair in edits.windows(2) {
                assert!(
                    pair[0].0 <= pair[1].0,
                    "{} under {label}: edits must ascend by start offset, got {:?}",
                    case.lang.id(),
                    edits.iter().map(|e| (e.0, e.1)).collect::<Vec<_>>(),
                );
            }
        }
    }
}

/// The edit set as an observer sees it: the resulting text PLUS the applied
/// edits, so a change in WHERE the edit lands is caught and not only a change in
/// the final bytes.
type EditSet = (String, Vec<(usize, usize, String)>);

fn rewrite(case: &Case) -> EditSet {
    rewrite_src(case.lang, case.pat, case.repl, case.lf)
}

fn rewrite_src(lang: Language, pat: &str, repl: &str, src: &str) -> EditSet {
    let p = Pattern::compile(lang, pat).unwrap();
    let parsed = parse(lang, src, &pbudget()).unwrap();
    let req = RewriteRequest {
        pattern: &p,
        rule: None,
        replacement: repl,
        allow_comment_loss: true,
        search_budget: SearchBudget::default(),
        parse_budget: pbudget(),
        max_expansion_bytes: 1 << 20,
    };
    let out = rewrite_file(&parsed, src, &req).unwrap();
    let edits = out
        .edits
        .iter()
        .map(|e| (e.start, e.end, e.replacement.clone()))
        .collect();
    let new = if out.edits.is_empty() {
        src.to_string()
    } else {
        apply_edits(src, &out.edits).unwrap()
    };
    (new, edits)
}

// ---- half 2: platform conventions, parameterised --------------------------------
//
// Each case below compares a CRLF and a BOM+CRLF variant against the LF baseline
// of the SAME source. If the pipeline silently depended on the platform
// convention, one of two things would change: the structure, or the byte
// extents. Both are asserted.

#[test]
fn platform_conventions_shift_only_byte_extents_not_structure() {
    for case in cases() {
        let baseline = observe(case.lang, case.pat, case.lf);
        let want = structure(&baseline);
        assert!(
            want.0 > 0,
            "{} case {:?} must match something",
            case.lang.id(),
            case.pat,
        );

        for (label, variant) in case.conventions() {
            let got = observe(case.lang, case.pat, &variant);
            assert_eq!(
                structure(&got),
                want,
                "{} under {label}: the STRUCTURE must not depend on line endings or a BOM",
                case.lang.id(),
            );
            assert_eq!(
                got.1,
                baseline.1,
                "{} under {label}: truncation must not depend on the convention",
                case.lang.id(),
            );
            assert_eq!(
                got.2,
                baseline.2,
                "{} under {label}: step count must not depend on the convention",
                case.lang.id(),
            );
        }
    }
}

#[test]
fn edit_sets_are_identical_across_platform_conventions() {
    // The edit half of the same property: rewriting a CRLF/BOM file must produce
    // the same SHAPE of edit set as rewriting its LF twin. The byte offsets move,
    // because the file moved; what must not move is how many edits there are,
    // which ranges they cover relative to each other, or what they produce once
    // the original convention is restored.
    for case in cases() {
        let lf = rewrite_src(case.lang, case.pat, case.repl, case.lf);
        assert!(!lf.1.is_empty(), "{} must edit something", case.lang.id());

        for (label, variant) in case.conventions().into_iter().skip(1) {
            let other = rewrite_src(case.lang, case.pat, case.repl, &variant);

            assert_eq!(
                other.1.len(),
                lf.1.len(),
                "{} under {label}: the NUMBER of edits must not depend on the convention",
                case.lang.id(),
            );
            assert_eq!(
                other.1.iter().map(|e| normalise(&e.2)).collect::<Vec<_>>(),
                lf.1.iter().map(|e| normalise(&e.2)).collect::<Vec<_>>(),
                "{} under {label}: the replacement TEXT of each edit must not depend on the \
                 convention",
                case.lang.id(),
            );
            // Relative structure: each edit's width, measured after the file's own
            // convention is normalised away. Absolute offsets shift with the
            // BOM/CRLF prefix and are meaningless to compare across conventions;
            // the NORMALISED width is what says "this edit still covers the same
            // span of the same code".
            //
            // Measuring the RAW width here was wrong, and the test caught it: a
            // Python edit that spans a line ending is legitimately one byte wider
            // in a CRLF file (32 vs 31), because `\r\n` is two bytes where `\n`
            // is one. The raw-width assertion was a claim that had never been true
            // for CRLF sources, so it would have been a permanent false red. The
            // normalised width is the property that actually holds.
            assert_eq!(
                widths_in(other.1.as_slice(), &variant),
                widths_in(lf.1.as_slice(), case.lf),
                "{} under {label}: each edit must still span the same normalised run of code",
                case.lang.id(),
            );
            // And the outcome, once the file's own convention is restored. The
            // BOM is not text, so it is normalised away; nothing else is.
            assert_eq!(
                normalise(&other.0),
                normalise(&lf.0),
                "{} under {label}: the rewritten text must agree once the convention is restored",
                case.lang.id(),
            );
        }
    }
}

/// A BOM is not part of the text an editor sees, so it is normalised away with
/// the line endings by `normalise`. Nothing else is.
///
/// Each edit's span, measured in the SOURCE's own bytes with that source's
/// convention normalised away. Reading the source back (rather than trusting the
/// offsets alone) is what makes this a check of the edit set against the file it
/// claims to rewrite.
fn widths_in(edits: &[(usize, usize, String)], src: &str) -> Vec<usize> {
    edits
        .iter()
        .map(|(start, end, _)| normalise(&src[*start..*end]).len())
        .collect()
}

#[test]
fn the_unicode_axis_actually_moves_byte_extents() {
    // The anti-theatre control for the whole file.
    //
    // NFC "café" is 5 bytes; NFD "café" is 6, because the combining acute is a
    // second code point. A pipeline reporting CHARACTER columns would report the
    // same number for both. One reporting BYTE offsets - which is what this
    // project documents - reports one more for NFD.
    //
    // If this stops holding, byte offsets have become character offsets (or the
    // reverse) and every "byte extents" expectation in this file is measuring the
    // wrong unit. It fails loudly rather than letting the tests above go quietly
    // vacuous.
    const PAT: &str = "log(\"$X\")";
    //                            café  café
    let nfc = "log(\"caf\u{e9}\");\n";
    let nfd = "log(\"cafe\u{301}\");\n";
    assert_ne!(
        nfc.len(),
        nfd.len(),
        "the two spellings must differ in length or the axis is not being exercised",
    );

    let a = observe(Language::JavaScript, PAT, nfc);
    let b = observe(Language::JavaScript, PAT, nfd);

    assert_eq!(
        a.0.len(),
        1,
        "the NFC source must match exactly once, got {:?}",
        a.0,
    );
    assert_eq!(
        b.0.len(),
        1,
        "the NFD source must match exactly once, got {:?}",
        b.0,
    );

    // Same shape: one match, one capture named X, and the captured text is exactly
    // the bytes at the reported offsets in BOTH sources - the invariant that makes
    // the two spellings comparable at all.
    for (src, obs) in [(nfc, &a), (nfd, &b)] {
        let m = &obs.0[0];
        assert_eq!(m.captures.len(), 1, "{src:?}");
        let c = &m.captures[0];
        assert_eq!(
            c.name, "X",
            "the same metavariable must be captured in both spellings",
        );
        assert_eq!(
            &src[c.start_byte..c.end_byte],
            c.text.as_str(),
            "a capture must be the bytes at its own offsets",
        );
        // The captured text is a verbatim slice, so NFC and NFD capture DIFFERENT
        // bytes - that is the whole point of the axis, and normalising them here
        // would erase the evidence. So they are deliberately NOT asserted equal;
        // what is asserted is that the match TEXT and the capture COVER the same
        // structure, which the offset comparison below then pins.
    }

    // The axis: NFD's end offset is one byte further along. That is the entire
    // point - the convention moves the byte extents, and the structure holds.
    assert_eq!(
        b.0[0].end_col,
        a.0[0].end_col + 1,
        "NFD ({} bytes) must end one column after NFC ({} bytes): byte extents are what the \
         convention moves",
        nfd.len(),
        nfc.len(),
    );
    assert_eq!(b.0[0].end_byte, a.0[0].end_byte + 1);
}

#[test]
fn line_endings_do_not_change_which_nodes_match() {
    // A dedicated case for the axis most likely to be real: on Windows a file
    // written and rewritten by tooling often ends up CRLF, and a matcher that
    // compared against a pattern built with "\n" would stop matching. It must not.
    for (lang, pat, lf) in [
        (
            Language::JavaScript,
            "function $F() { $$$BODY }",
            "function f() {\n    return 1;\n}\n",
        ),
        (
            Language::Rust,
            "fn $F() { $$$BODY }",
            "fn f() {\n    let x = 1;\n}\n",
        ),
        (
            Language::Python,
            "def $F():\n    $$$BODY",
            "def f():\n    return 1\n",
        ),
    ] {
        let crlf = lf.replace('\n', "\r\n");
        let a = structure_of(lang, pat, lf);
        let b = structure_of(lang, pat, &crlf);
        assert_eq!(a, b, "{}: CRLF must not change the matches", lang.id());
        assert!(a.0 > 0, "{}: the pattern must match something", lang.id());
    }
}

#[test]
fn a_crlf_line_ending_is_one_line_not_two() {
    // The mutation guard for the line-ending axis, and the test that makes that
    // axis worth anything.
    //
    // Comparing CRLF against LF and finding "the same matches" is too weak to
    // catch the bug this axis is FOR: a `LineIndex` that breaks on `\r` as well
    // as `\n` produces a SPURIOUS EMPTY LINE between every CRLF pair. Structure
    // survives that - the same nodes still match - because `normalise` folds the
    // endings away. So the first version of this file passed under that mutation.
    //
    // What does not survive it is the LINE NUMBER, which is an observable and is
    // what a downstream diagnostic reports. So this test pins exact line/column
    // for a match on the second line of a CRLF file. Under the mutation the
    // second line becomes the third and this goes red.
    //
    // This is the honest shape of a platform axis: not "it still matches", but
    // "the reported positions are the ones the file's real lines imply".
    let lf = "function f() {\n    return 1;\n}\n";
    let crlf = lf.replace('\n', "\r\n");
    const PAT: &str = "function $F() { $$$BODY }";

    let a = observe(Language::JavaScript, PAT, lf);
    let b = observe(Language::JavaScript, PAT, &crlf);

    assert_eq!(a.0.len(), 1, "the LF source must match once: {:?}", a.0);
    assert_eq!(b.0.len(), 1, "the CRLF source must match once: {:?}", b.0);

    // `\r\n` is two bytes where `\n` is one, so every offset after the first
    // line break moves by exactly the number of line breaks before it - one here.
    // The LINE number must NOT move, and must not gain a phantom line for the
    // `\r` of the first ending.
    assert_eq!(
        (b.0[0].start_line, b.0[0].start_col),
        (a.0[0].start_line, a.0[0].start_col),
        "a CRLF ending is ONE line break, not two",
    );
    assert_eq!(b.0[0].start_line, 1, "the match starts on line 1");

    // A match that begins on the SECOND line is the load-bearing case: the
    // mutation makes it report line 3.
    const SECOND: &str = "return $X;";
    let c = observe(Language::JavaScript, SECOND, &crlf);
    let d = observe(Language::JavaScript, SECOND, lf);
    assert_eq!(c.0.len(), 1, "the second line must match once: {:?}", c.0);
    assert_eq!(
        c.0[0].start_line, 2,
        "the statement after the first CRLF is on line 2, not line 3",
    );
    assert_eq!(
        (c.0[0].start_line, c.0[0].start_col),
        (d.0[0].start_line, d.0[0].start_col),
        "line and column must not depend on the convention",
    );
    // And the byte offset must move by exactly one per preceding line break.
    assert_eq!(
        c.0[0].start_byte,
        d.0[0].start_byte + 1,
        "the CRLF adds exactly one byte before this match",
    );
}

#[test]
fn a_path_separator_appears_verbatim_and_is_not_reinterpreted() {
    // The fifth OS axis. Inside a string literal a path is text, and both
    // separators must survive a match byte for byte: Windows code embedding
    // `C:\Users\ada` must not be silently rewritten into a POSIX-looking path.
    // This is where a real cross-platform bug would appear, and it is why this
    // file is not only about line endings.
    //
    // Go's RAW string literal (`...`) carries the Windows path, because a Go
    // interpreted literal would need the backslashes escaped again and the test
    // would then measure the escaping rather than the separator. The FIRST
    // version of this test used an interpreted literal, and the capture it
    // asserted on never existed - so the assertion was unreachable and proved
    // nothing. That is the shape of a theatre test, which is why the
    // `captured.len() == 2` guard below is part of the case and not a nicety.
    let go_src = "package p\n\nfunc f() {\n\tos.Open(`C:\\Users\\ada\\x`)\n\tos.Open(\"/usr/local/bin\")\n}\n";
    let caps = structure_of(Language::Go, "os.Open($X)", go_src);
    let captured: Vec<&str> = caps
        .1
        .iter()
        .flat_map(|(_, c)| c.iter())
        .map(|(_, text)| text.as_str())
        .collect();

    assert_eq!(
        caps.0, 2,
        "both open() calls must match, or the separator comparison below is vacuous",
    );
    assert!(
        captured.contains(&"`C:\\Users\\ada\\x`"),
        "the Windows-separated path must be captured verbatim, got {captured:?}",
    );
    assert!(
        captured.contains(&"\"/usr/local/bin\""),
        "the POSIX-separated path must be captured verbatim, got {captured:?}",
    );
    // The load-bearing half: each separator kept ITS OWN spelling and the two
    // stayed distinct. A pipeline that normalised separators, or dropped the
    // backslashes, fails here rather than passing quietly.
    assert_ne!(
        captured[0], captured[1],
        "the two spellings must stay distinct",
    );
    assert!(
        captured[0].contains('\\'),
        "the backslash must survive: {captured:?}",
    );
    assert!(
        captured[1].contains('/'),
        "the slash must survive: {captured:?}",
    );
}

#[test]
fn every_language_the_catalogue_knows_is_covered_by_this_file() {
    // Keeps this file honest as the anchor for a `per language` claim: add a
    // language to `Language` without a case here, and this goes red rather than
    // the row quietly covering fewer languages than it claims.
    //
    // The expectation is derived from `Language::all()` rather than written out,
    // so it cannot drift, and it is compared as a SET: the order of the cases in
    // this file carries no meaning, and pinning it would make this a change
    // detector instead of a coverage check.
    let mut covered: Vec<&str> = cases().iter().map(|c| c.lang.id()).collect();
    let mut want: Vec<&str> = Language::all().iter().map(|l| l.id()).collect();
    covered.sort_unstable();
    want.sort_unstable();
    assert_eq!(
        covered, want,
        "every Language::all() member needs a case in this file",
    );
}
