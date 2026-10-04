//! PAT-05 differential tests: our matcher vs ast-grep-core.

#![allow(missing_docs)]

#[path = "pattern_diff/mod.rs"]
mod pattern_diff;

use pattern_diff::{
    DiffLang, KNOWN_DIVERGENCES, Kind, RANDOM_JS_PATTERNS, canonicalize_hits, corpus_all,
    corpus_counts, generate_random_js_sources, normalize_ast_grep_hits, search_ast_grep,
    search_ours,
};

fn ours(
    lang: DiffLang,
    pattern: &str,
    source: &str,
) -> Result<Vec<pattern_diff::NormalizedHit>, String> {
    search_ours(lang, pattern, source).map(canonicalize_hits)
}

fn asg(
    lang: DiffLang,
    pattern: &str,
    source: &str,
) -> Result<Vec<pattern_diff::NormalizedHit>, String> {
    search_ast_grep(lang, pattern, source)
        .map(normalize_ast_grep_hits)
        .map(canonicalize_hits)
}

fn is_known(_lang: opencrayast_lang::Language, pattern: &str, source: &str) -> bool {
    // Exact repro rows, plus any `f($A, $A)` (same Intentional capture-span shape
    // across JS/TS/Python/Go/Rust corpus sources).
    KNOWN_DIVERGENCES.iter().any(|d| {
        (d.pattern == pattern && d.source == source)
            || (d.id == "repeat-metavar-capture-span" && pattern == "f($A, $A)")
    })
}

#[test]
fn corpus_has_at_least_forty_cases_per_language() {
    for (lang, n) in corpus_counts() {
        assert!(n >= 40, "{lang:?} has only {n} corpus cases (need ≥40)");
    }
}

#[test]
fn ast_grep_runs_on_entire_corpus() {
    let mut ok = 0usize;
    for case in corpus_all() {
        if search_ast_grep(case.lang, &case.pattern, &case.source).is_ok() {
            ok += 1;
        }
    }
    assert!(
        ok >= 180,
        "too few corpus cases runnable on ast-grep: ok={ok}"
    );
    for case in corpus_all().into_iter().filter(|c| c.id.contains(":pad-")) {
        search_ast_grep(case.lang, &case.pattern, &case.source)
            .unwrap_or_else(|e| panic!("pad case {} failed: {e}", case.id));
    }
}

/// The corpus is expected to be almost entirely `expect_agree`, and in fact is:
/// `expect_agree_is_not_actually_a_filter_and_is_not_relied_on` pins that every
/// row is `true`. This test stays as the cheap guard on the corpus still being
/// large and mostly marked, which is what a reader of the corpus would expect.
#[test]
fn corpus_expect_agree_cases_are_marked() {
    let agree = corpus_all().into_iter().filter(|c| c.expect_agree).count();
    assert!(
        agree > 150,
        "expected most corpus rows to expect_agree, got {agree}"
    );
}

#[test]
fn known_divergences_skeleton_is_runnable_on_ast_grep() {
    assert!(!KNOWN_DIVERGENCES.is_empty());
    for d in KNOWN_DIVERGENCES {
        assert!(!d.id.is_empty());
        assert!(!d.why.is_empty());
        let lang = DiffLang(d.lang);
        // Documented rows must not panic the oracle (Err is fine).
        let _ = search_ast_grep(lang, d.pattern, d.source);
    }
}

#[test]
fn known_divergences_are_classified_and_still_diverge() {
    for d in KNOWN_DIVERGENCES {
        let lang = DiffLang(d.lang);
        let asg_hits = match asg(lang, d.pattern, d.source) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let ours_hits = ours(lang, d.pattern, d.source)
            .unwrap_or_else(|e| panic!("ours failed on known divergence {}: {e}", d.id));
        assert_ne!(
            asg_hits, ours_hits,
            "divergence {} ({:?}) disappeared — update KNOWN_DIVERGENCES. asg={asg_hits:?} ours={ours_hits:?}",
            d.id, d.kind
        );
    }
    assert!(
        KNOWN_DIVERGENCES
            .iter()
            .any(|d| d.kind == Kind::Intentional)
    );
    assert!(
        KNOWN_DIVERGENCES
            .iter()
            .any(|d| d.kind == Kind::AstGrepDiff)
    );
    // FIX-1 removed both `OursBug` rows (the TS generic case and the Go func declaration): our
    // side now agrees with the oracle. Asserted so a future bug has to be recorded rather than
    // silently dropped.
    assert!(
        !KNOWN_DIVERGENCES.iter().any(|d| d.kind == Kind::OursBug),
        "no known OursBug divergence is expected right now"
    );
}

/// Rows where ast-grep itself could not run. We cannot disagree with an oracle
/// that produced no answer, so these are excluded from the comparison rather
/// than counted as agreement.
fn ast_grep_unrunnable(case: &pattern_diff::Case) -> bool {
    search_ast_grep(case.lang, &case.pattern, &case.source).is_err()
}

/// Our matcher refused to compile a pattern that ast-grep compiles and matches.
///
/// This is NOT a divergence between two matchers, and it is NOT something to
/// record in `KNOWN_DIVERGENCES`: the oracle produced a real answer, and ours
/// produced none. Treating it as a skip would let a genuine gap in our pattern
/// compiler pass as "no disagreement", which is precisely the claim this test
/// must not overstate. So the rows are enumerated here and asserted to be exactly
/// this set; a new one fails the test and a fixed one forces the list to shrink.
const OURS_CANNOT_COMPILE: &[(&str, &str, &str)] = &[
    // Both are a bare `import` statement used as a pattern root.
    //
    //   ours: Err("the pattern does not parse in any context")  (compile.rs, no
    //         context wrapped the fragment as a statement)
    //   asg:  Ok, one hit with the metavariable captured
    //
    // The quoted Go form `import "$P"` agrees on both sides, so this is not
    // "imports are unimplemented"; it is the bare-statement form that our
    // context selection in pattern/compile.rs does not reach. Fixing it is
    // crate-internal work, out of scope for a test change.
    (
        "typescript",
        "import type { $A } from $M",
        "import type { A } from 'm';",
    ),
    ("go", "import $P", "package p\nimport \"fmt\"\n"),
];

fn ours_cannot_compile(case: &pattern_diff::Case) -> bool {
    ours(case.lang, &case.pattern, &case.source).is_err()
}

#[test]
fn our_side_compiles_everything_the_oracle_compiles_except_these_two() {
    let mut found: Vec<String> = Vec::new();
    for case in corpus_all() {
        if ast_grep_unrunnable(&case) {
            continue;
        }
        if ours_cannot_compile(&case) {
            found.push(format!(
                "{}|{}|{}",
                case.lang.0.id(),
                case.pattern,
                case.source
            ));
        }
    }
    let expected: Vec<String> = OURS_CANNOT_COMPILE
        .iter()
        .map(|(l, p, s)| format!("{l}|{p}|{s}"))
        .collect();

    assert_eq!(
        found.len(),
        expected.len(),
        "the set of rows our matcher cannot compile changed.\n\
         our matcher cannot compile: {found:?}\n\
         recorded in OURS_CANNOT_COMPILE:   {expected:?}\n\
         If a row was FIXED, delete it from OURS_CANNOT_COMPILE (that is progress).\n\
         If a row is NEW, it is a defect in our pattern compiler, not a divergence: \
         investigate before adding it here."
    );
    // Order-independent set equality, and each recorded row must be a real one.
    let mut found_sorted = found.clone();
    let mut expected_sorted = expected.clone();
    found_sorted.sort();
    expected_sorted.sort();
    assert_eq!(
        found_sorted, expected_sorted,
        "OURS_CANNOT_COMPILE is out of date"
    );
}

/// THE differential assertion, over the WHOLE shared corpus.
///
/// What this actually proves, stated exactly:
///
/// * Every corpus row is enumerated. None is dropped for being "unmarked".
/// * A row counts as *compared* only when both sides returned an answer.
///   - ast-grep could not run  -> excluded, see `ast_grep_unrunnable`.
///   - our side could not compile -> excluded, and pinned by the test above so
///     it cannot quietly become a bigger set.
/// * Every COMPARED row that is in `KNOWN_DIVERGENCES` is skipped, because the
///   two matchers are known and intentionally to differ there.
/// * Every remaining compared row must agree exactly.
///
/// The earlier version of this test was named `differential_corpus_agrees_when_marked`
/// and filtered on `expect_agree`. Measured on 2026-10-02, that filter selected
/// NOTHING: no row in the corpus ever sets `expect_agree: false` (grep for `false`
/// in corpus.rs returns no `push` argument), so the name promised a restriction
/// that was not in force while the real restriction - the known-divergence
/// skip - went unmentioned. The filter has been dropped and the test now runs on
/// the full corpus, which is the stronger claim the name implied.
///
/// Agreement does NOT hold universally, and that is not a defect to be hidden:
/// of 196 comparable rows, 11 disagree, and every one of them is a documented
/// divergence (5 of them are the same `f($A, $A)` capture-span shape across
/// languages, covered by the `repeat-metavar-capture-span` special case). The two
/// rows our side cannot compile at all are accounted for separately. So the true
/// statement is "our matcher agrees with ast-grep on every corpus row except the
/// recorded divergences", and that is what is asserted.
#[test]
fn differential_corpus_agrees_except_for_recorded_divergences() {
    let mut compared = 0usize;
    let mut agreed = 0usize;
    let mut skipped_known = 0usize;
    let mut skipped_ours = 0usize;
    let mut skipped_ast_grep = 0usize;

    for case in corpus_all() {
        if is_known(case.lang.0, &case.pattern, &case.source) {
            skipped_known += 1;
            continue;
        }
        let asg_hits = match asg(case.lang, &case.pattern, &case.source) {
            Ok(h) => h,
            Err(_) => {
                skipped_ast_grep += 1;
                continue;
            }
        };
        let ours_hits = match ours(case.lang, &case.pattern, &case.source) {
            Ok(h) => h,
            Err(_) => {
                // Pinned by `our_side_compiles_everything_the_oracle_compiles_except_these_two`;
                // recorded here so a real gap is never mistaken for agreement.
                assert!(
                    ours_cannot_compile(&case),
                    "unreachable: both sides failed for {}",
                    case.id
                );
                skipped_ours += 1;
                continue;
            }
        };
        compared += 1;
        assert_eq!(
            asg_hits, ours_hits,
            "PAT-05 mismatch on {}: this row is not a recorded divergence, so it is a real \
             disagreement. Either our matcher or the oracle is wrong; do NOT silence it by \
             marking it expect_agree=false.\nasg={asg_hits:?}\nours={ours_hits:?}",
            case.id
        );
        agreed += 1;
    }

    eprintln!(
        "corpus compared={compared} agreed={agreed} skipped_known={skipped_known} \
         skipped_ours_cannot_compile={skipped_ours} skipped_ast_grep_unrunnable={skipped_ast_grep}"
    );

    // The row count is pinned so the corpus cannot be quietly shrunk.
    assert!(
        compared >= 180,
        "too few comparable corpus rows: {compared}"
    );
    assert_eq!(
        compared, agreed,
        "every compared row must agree; {skipped_known} known-divergence rows were excluded"
    );
    // The exclusions are pinned too, so "everything was skipped" is not a pass.
    assert_eq!(
        skipped_ours,
        OURS_CANNOT_COMPILE.len(),
        "rows our side cannot compile must match the recorded list"
    );
    assert!(
        skipped_known > 0,
        "the known-divergence list is not being exercised"
    );
}

/// `expect_agree` used to gate the differential test, and no corpus row ever set
/// it false, so the flag selected nothing. It is kept only so this test can
/// notice if that ever changes, rather than letting the flag rot back into
/// looking load-bearing.
#[test]
fn expect_agree_is_not_actually_a_filter_and_is_not_relied_on() {
    let all = corpus_all();
    let false_rows: Vec<&str> = all
        .iter()
        .filter(|c| !c.expect_agree)
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        false_rows.is_empty(),
        "corpus rows with expect_agree=false now exist: {false_rows:?}. The differential test \
         no longer filters on this flag, so either the flag is meaningful and the test should \
         filter again, or these rows are redundant."
    );
}

#[test]
fn random_js_ast_grep_side_completes() {
    let sources = generate_random_js_sources(1500);
    assert_eq!(sources.len(), 1500);
    assert_eq!(RANDOM_JS_PATTERNS.len(), 8);
    let mut searches = 0usize;
    let lang = DiffLang(opencrayast_lang::Language::JavaScript);
    for src in &sources {
        for pat in RANDOM_JS_PATTERNS {
            let _ = search_ast_grep(lang, pat, src)
                .unwrap_or_else(|e| panic!("random JS pattern {pat:?} on {src:?}: {e}"));
            searches += 1;
        }
    }
    assert_eq!(searches, 1500 * 8);
}

#[test]
fn random_js_agrees_or_is_known() {
    let sources = generate_random_js_sources(1500);
    let lang = DiffLang(opencrayast_lang::Language::JavaScript);
    let mut compared = 0usize;
    for src in &sources {
        for pat in RANDOM_JS_PATTERNS {
            if is_known(lang.0, pat, src) {
                continue;
            }
            let asg_hits = match asg(lang, pat, src) {
                Ok(h) => h,
                Err(_) => continue,
            };
            let ours_hits = ours(lang, pat, src).unwrap_or_else(|e| panic!("ours: {e}"));
            compared += 1;
            assert_eq!(
                asg_hits, ours_hits,
                "random JS diverge pat={pat:?} src={src:?}\nasg={asg_hits:?}\nours={ours_hits:?}"
            );
        }
    }
    assert_eq!(compared, 1500 * 8);
    eprintln!("random_js compared={compared} all agreed");
}

#[test]
fn normalize_sorts_hits_and_captures() {
    use pattern_diff::{CaptureSpan, NormalizedHit, finalize_hits};
    let hits = finalize_hits(vec![
        NormalizedHit {
            start: 10,
            end: 20,
            captures: vec![
                CaptureSpan {
                    name: "B".into(),
                    start: 2,
                    end: 3,
                    list: false,
                },
                CaptureSpan {
                    name: "A".into(),
                    start: 1,
                    end: 2,
                    list: false,
                },
            ],
        },
        NormalizedHit {
            start: 1,
            end: 5,
            captures: vec![],
        },
    ]);
    assert_eq!(hits[0].start, 1);
    assert_eq!(hits[1].captures[0].name, "A");
}

#[test]
fn normalize_and_canonicalize_empty_list() {
    use pattern_diff::{CaptureSpan, NormalizedHit, finalize_hits};
    let hits = canonicalize_hits(finalize_hits(vec![NormalizedHit {
        start: 10,
        end: 20,
        captures: vec![CaptureSpan {
            name: "A".into(),
            start: 15,
            end: 15,
            list: true,
        }],
    }]));
    assert_eq!(hits[0].captures[0].start, 10);
    assert_eq!(hits[0].captures[0].end, 10);
}

#[test]
fn report_first_edition_stats() {
    let counts = corpus_counts();
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    let by_kind = |k| KNOWN_DIVERGENCES.iter().filter(|d| d.kind == k).count();
    eprintln!(
        "pattern_diff v2: cases={total} known={} intentional={} asg_diff={} ours_bug={} random={}×{} agree_marked={}",
        KNOWN_DIVERGENCES.len(),
        by_kind(Kind::Intentional),
        by_kind(Kind::AstGrepDiff),
        by_kind(Kind::OursBug),
        1500,
        RANDOM_JS_PATTERNS.len(),
        corpus_all().iter().filter(|c| c.expect_agree).count(),
    );
}
