//! Hostile-input fuzz-style tests for edit sets and rewrite templates.
//!
//! An edit set is the one thing an agent hands the tool that rewrites a file, so the invariant is
//! the strict one: whatever the ranges are, a rejected set changes nothing and an accepted one
//! changes exactly the bytes it said.
//!
//! `Plan::parse` is deliberately not a target here: the plan format is still landing under
//! EDIT-2, so a hostile-input spec for it would be written against a format that is about to
//! change. It gets its own target with EDIT-2.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

mod common;

use common::fuzz::{self, Case, Ran};
use opencrayast_core::ErrorCode;
use opencrayast_core::limits::Limits;
use opencrayast_edit::editset::{Edit, apply_edits, changed_bytes, validate_edits};
use opencrayast_edit::template::{ExpandOptions, expand_template, indent_of_line};
use opencrayast_query::pattern::{Capture, CaptureKind};
use std::ops::Range;

const SEED: u64 = 0x00F0_0DE5_2026_1002;
const CASES: usize = 3000;

// -- The executed-fraction floor, one constant per target -----------------------------
//
// Declared here, next to the targets they govern, rather than as one crate-wide constant. All four
// measure 3000/3000 with nothing skipped, so 1.0 is the measurement, not a target.
//
// The floors used to be one shared `MIN_EXECUTED_FRACTION = 0.95`. That value had no force against
// this tree: the old `edit.overlap` generator produced 2900/3000 - 96.7% - and the suite stayed
// green. A floor that admits a 3.3% decline, or a 10% one, or a 30% one, is not a gate. See the
// `edit.overlap` constant for the specific regression that motivated raising it.
//
// A target that must genuinely decline cases declares a LOWER value in THIS file, where the reader
// of that target will see it, and says why. It never lowers something shared, which would weaken
// every other target silently at the same time.

/// `edit.editset`: 3000/3000. The body asserts on whatever `validate_edits` decided and never
/// declines, so every case that reaches the property is counted.
const EDIT_EDITSET_MIN_EXECUTED: f64 = 1.0;

/// `edit.overlap`: 3000/3000, and this is the one the audit used as its evidence.
///
/// `overlap_offsets` draws all four endpoints as indices into the sorted boundary list rather than
/// by arithmetic on offsets, so the pair is wrong in exactly one way and no case can fall out. An
/// earlier version that declined 100 of 3000 cases was tolerated by the old 0.95 floor; a much
/// earlier version declined 2048 of 3000 and the log never said so. Neither can pass a floor of 1.0.
const EDIT_OVERLAP_MIN_EXECUTED: f64 = 1.0;

/// `edit.template`: 3000/3000. Expansion is total and both outcomes are asserted.
const EDIT_TEMPLATE_MIN_EXECUTED: f64 = 1.0;

/// `edit.indent_of_line`: 3000/3000. Clamping is total; no case is declined.
const EDIT_INDENT_OF_LINE_MIN_EXECUTED: f64 = 1.0;

/// A source with multi-byte characters, so a range can land inside one.
const SOURCE: &str =
    "fn main() {\n    let s = \"caf\u{e9} \u{4e2d}\u{6587}\";\n    println!(\"{s}\");\n}\n";

/// Seeds for the mutation stream: the shapes an edit or a template takes.
const FRAGMENTS: &[&str] = &[
    "",
    "x",
    "caf\u{e9}",
    "\u{4e2d}\u{6587}",
    "$X",
    "$$$X",
    "$_",
    "$$$",
    "$$",
    "\\n",
    "\\r\\n",
    "\n",
    "\t",
    "  ",
    "0",
    "18446744073709551615",
    "99999999999999999999999999",
    "-1",
    "verbatim",
    "e\u{301}",
];

/// Numbers used as edit ranges. They are the interesting ones: 0, 1, the source length, one past
/// it, `usize::MAX`, and an offset inside a multi-byte character.
fn range_candidates(case: &Case, len: usize) -> Vec<usize> {
    let mut out = vec![
        0,
        1,
        len,
        len + 1,
        usize::MAX,
        len / 2,
        2,
        3,
        len.saturating_sub(1),
    ];
    for chunk in case.bytes.chunks(8).take(2) {
        if chunk.len() == 8 {
            out.push(u64::from_le_bytes(chunk.try_into().unwrap()) as usize);
        }
    }
    // Inside the multi-byte character, on purpose: `café` puts a 2-byte `é` at some offset.
    if let Some(at) = SOURCE.find('\u{e9}') {
        out.push(at + 1);
    }
    out
}

/// Offsets that are ordered, inside the source, and on character boundaries: the only offsets from
/// which an overlap sweep can build a pair that is wrong in ONE way.
///
/// The point of this generator is that a refusal of the resulting pair proves the OVERLAP check
/// fired. A pair built from `range_candidates` would also be unordered, out of range, or split a
/// character about two thirds of the time, so `validate_edits` would refuse it for a reason that
/// has nothing to do with overlap - and the case would be worthless while still counting as a
/// pass. Sorted and de-duplicated so two distinct entries are always strictly increasing, which is
/// what lets [`overlap_offsets`] guarantee room for `b` strictly inside `a` by index arithmetic.
///
/// Every character boundary of the source is present, not just the case-derived ones, so the
/// index arithmetic in `overlap_offsets` has a known-size set to work with no matter what the
/// mutation stream produced.
fn overlap_candidates(case: &Case, len: usize) -> Vec<usize> {
    let mut out: Vec<usize> = vec![0, len];
    for at in 1..len {
        if SOURCE.is_char_boundary(at) {
            out.push(at);
        }
    }
    for chunk in case.bytes.chunks(8).take(2) {
        if chunk.len() == 8 {
            let at = u64::from_le_bytes(chunk.try_into().unwrap()) as usize;
            // Only the bytes that land on a boundary count; the rest would reintroduce the very
            // "refused for another reason" ambiguity this generator exists to remove.
            if at < len && SOURCE.is_char_boundary(at) {
                out.push(at);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Pick the four endpoints of one overlapping pair out of `cands`.
///
/// The shape the sweep needs is
///
/// ```text
///     a_start  <  b_start  <  b_end  <  a_end
///     \____________________/  \________/
///            a = [a_start, a_end)      b = [b_start, b_end)
/// ```
///
/// with the result narrowed to `a_start < b_start`, which still overlaps (`a_start < b_end` and
/// `b_start < a_end` both hold) and leaves a non-empty interior. Every endpoint is an entry of
/// `cands`, so all four are in range and on character boundaries, and both edits are ordered
/// (`start < end`) - which is the whole point: the pair is wrong in exactly ONE way, so a refusal
/// is attributable to the overlap check and nothing else.
///
/// Everything is picked **by index into `cands`**, never by arithmetic on the offsets themselves.
/// That is deliberate. Offsets are not evenly spaced (the source has 2- and 3-byte characters, so
/// neighbouring boundaries can be 1 or 3 apart), so `a_start + 2` or `b_start + 1` is NOT reliably
/// a boundary - the previous version did that arithmetic and produced pairs whose `b_start` landed
/// inside a multi-byte character about 40% of the time, which `validate_edits` refused for
/// `invalid_edit` on a *boundary* check while the case counted as a pass. Every offset here comes
/// from the sorted boundary list, so a pair is a genuine overlap by construction.
///
/// The index ranges are nested so the construction is total for any boundary list of at least
/// three entries - no case can fall out of them, which is why this target can run all 3000:
///
/// - `bi` (b_start) in `1..=N-3` — room for `a_start < b_start` on the left and two more
///   indices on the right;
/// - `ai` (a_end) in `bi+2..=N-1` — room for `b_end` strictly between `b_start` and `a_end`;
/// - `ci` (b_end) in `bi+1..=ai-1` — non-empty by the nesting above;
/// - `a_start` in `0..bi` — left of `b_start`, so the two ranges intersect.
///
/// `SOURCE` has 59 boundaries, so none of these ranges is ever empty.
fn overlap_offsets(cands: &[usize], index: usize) -> (usize, usize, usize, usize) {
    let n = cands.len();
    let bi = 1 + index % (n - 3);
    let ai = bi + 2 + index % (n - bi - 2);
    let ci = bi + 1 + index % (ai - bi - 1);
    (cands[index % bi], cands[bi], cands[ci], cands[ai])
}

#[test]
fn a_hostile_edit_set_is_validated_and_never_applied_half() {
    let limits = Limits::default();
    let source = SOURCE.to_string();
    fuzz::run_cases(
        "edit.editset",
        SEED,
        FRAGMENTS,
        CASES,
        move |case: &Case| {
            let candidates = range_candidates(case, SOURCE.len());
            let count = 1 + case.index % 4;
            let edits: Vec<Edit> = (0..count)
                .map(|i| {
                    let start = candidates[(case.index + i) % candidates.len()];
                    let end = candidates[(case.index + i * 3 + 1) % candidates.len()];
                    Edit {
                        start,
                        end,
                        replacement: case.input.clone(),
                    }
                })
                .collect();

            match validate_edits(&source, &edits, &limits) {
                Ok(()) => {
                    // Validated: applying it must work and must produce exactly the promised change.
                    let applied = apply_edits(&source, &edits).expect("a validated set applies");
                    // And it must agree with a hand-written application. This comparison used to
                    // sit in the `Err` arm under `if let Ok(..)`, where it could never run: a set
                    // `validate_edits` refused has already failed `check_structure`, which
                    // `apply_edits` runs first, so `Ok` was unreachable. Here it can - the set
                    // was just accepted, so there is a real result to compare against. The oracle
                    // skips overlapping edits by hand, so it proves nothing about the overlap
                    // branch; `generated_overlaps_are_always_refused` covers that directly.
                    assert_eq!(
                        applied,
                        replace_by_hand(&source, &edits),
                        "apply_edits and a hand-written application disagreed about the same set"
                    );
                    assert!(
                        applied.len() <= source.len() + changed_bytes(&edits) as usize,
                        "the result grew by more than the edits inserted"
                    );
                    for e in &edits {
                        assert!(e.start <= e.end, "a validated edit is not ordered");
                        assert!(e.end <= SOURCE.len(), "a validated edit is out of range");
                        assert!(
                            source.is_char_boundary(e.start) && source.is_char_boundary(e.end),
                            "a validated edit splits a character"
                        );
                    }
                }
                Err(e) => {
                    assert!(
                        matches!(e.code, ErrorCode::InvalidEdit | ErrorCode::LimitExceeded),
                        "an edit set refused for an unexpected reason: {e:?}"
                    );
                    // This arm used to ask `apply_edits` about the same set and assert the two
                    // disagreed, under `if let Ok(..)` so a refusal was invisible. It could not
                    // fire: `apply_edits` runs `check_structure` first, which is exactly the
                    // check `validate_edits` had just failed, so `Ok` was unreachable and the
                    // assertion was a green line that could never go red. A refused set cannot be
                    // applied - that is the property, and it is now stated rather than probed.
                    // The overlap case that this half-heartedly reached for is covered properly
                    // by `generated_overlaps_are_always_refused` below.
                }
            }
            // Every case runs: this sweep asserts on whatever `validate_edits` decided, so a
            // refusal is a result to check rather than a reason to bow out.
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_EDITSET_MIN_EXECUTED);
}

/// A hand-written application, so the accepted-set arm of the sweep has an oracle to disagree with.
///
/// Deliberately naive about overlap: it skips an edit that starts inside the previous one rather
/// than deciding what the tool should do. `validate_edits` refuses overlapping sets outright, so
/// this oracle is only ever asked about sets that do not overlap, and this function is therefore
/// not evidence about the overlap rule. `generated_overlaps_are_always_refused` is.
fn replace_by_hand(source: &str, edits: &[Edit]) -> String {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| (e.start, e.end));
    let mut out = String::new();
    let mut at = 0usize;
    for e in sorted {
        if e.start < at {
            continue; // overlapping: skip, the tool decides
        }
        out.push_str(&source[at..e.start]);
        out.push_str(&e.replacement);
        at = e.end;
    }
    out.push_str(&source[at.min(source.len())..]);
    out
}

/// Overlapping edits are the case that has to be refused: applying them in the wrong order would
/// write text at the wrong place. This is the check the mutation self-proof removes.
///
/// Two shapes, both generated rather than hand-written:
///
/// - `overlapping_edits_are_refused` keeps the two hand-written edits, because they are the
///   smallest statement of the rule and a regression should read as one line;
/// - `generated_overlaps_are_always_refused` sweeps the same property over 3000 cases.
///
/// The general sweep above cannot cover this, and the reason is worth stating precisely rather
/// than implying otherwise. It builds its `Edit` sets from `range_candidates`, whose offsets come
/// from the case index and are not constrained to overlap, so an overlapping pair is incidental
/// rather than intended; and its oracle, `replace_by_hand`, skips an overlapping edit outright
/// (`if e.start < at { continue }`), so on the rare overlapping case it would quietly produce the
/// wrong answer rather than a failure. A mutation self-proof on `validate_edits`' overlap check
/// therefore stays green with the check deleted - which is exactly the failure mode this suite
/// exists to prevent - and only the dedicated targets below would go red.
#[test]
fn overlapping_edits_are_refused() {
    let limits = Limits::default();
    let overlapping = vec![
        Edit {
            start: 0,
            end: 10,
            replacement: "a".into(),
        },
        Edit {
            start: 5,
            end: 15,
            replacement: "b".into(),
        },
    ];
    assert_eq!(
        validate_edits(SOURCE, &overlapping, &limits)
            .expect_err("overlapping edits must be refused")
            .code,
        ErrorCode::InvalidEdit
    );
    // The same edits in the other order are refused too: the check cannot depend on the order
    // the caller happened to write them in.
    let flipped = vec![overlapping[1].clone(), overlapping[0].clone()];
    assert_eq!(
        validate_edits(SOURCE, &flipped, &limits)
            .expect_err("order must not matter")
            .code,
        ErrorCode::InvalidEdit
    );
}

/// Generated overlaps: every pair that genuinely overlaps must be refused, in both orders, and
/// the refusal must be `invalid_edit` rather than something incidental.
#[test]
fn generated_overlaps_are_always_refused() {
    // `run_cases` hands the closure to worker threads, so it must be `'static`: `limits` is built
    // inside rather than captured from the enclosing scope.
    fuzz::run_cases(
        "edit.overlap",
        SEED ^ 0x00DE_01A9,
        FRAGMENTS,
        CASES,
        |case: &Case| {
            let limits = Limits::default();
            // The candidates are sorted and de-duplicated, so distinct entries are strictly
            // increasing, and `overlap_offsets` picks all four endpoints BY INDEX out of them.
            // So every case here is a genuine overlap whose only defect is the overlap: both
            // edits are ordered, in range and on character boundaries. That is the whole point -
            // while this sweep drew its offsets from `range_candidates`, 2048 of its 3000 cases
            // came back early and the harness still printed "3000 cases".
            let cands = overlap_candidates(case, SOURCE.len());
            let (a_start, b_start, b_end, a_end) = overlap_offsets(&cands, case.index);
            // The construction is total, so these are invariants rather than checks that can
            // decline a case. If one of them ever fires it is a bug in the picker, and it must be
            // loud: a silent fallback here is how the 100 skips this target used to report came
            // back.
            debug_assert!(a_start < b_start && b_start < b_end && b_end < a_end);
            debug_assert!(a_end <= SOURCE.len() && b_end <= SOURCE.len());
            debug_assert!(
                a_start < a_end
                    && b_start < b_end
                    && SOURCE.is_char_boundary(a_start)
                    && SOURCE.is_char_boundary(a_end)
                    && SOURCE.is_char_boundary(b_start)
                    && SOURCE.is_char_boundary(b_end),
                "the overlap pair must be wrong in one way only: overlap"
            );

            let first = Edit {
                start: a_start,
                end: a_end,
                replacement: case.input.clone(),
            };
            let second = Edit {
                start: b_start,
                end: b_end,
                replacement: String::new(),
            };

            for order in [
                vec![first.clone(), second.clone()],
                vec![second.clone(), first.clone()],
            ] {
                // Overlap is the only thing wrong with these two edits: both are ordered, in range, and on
                // character boundaries (the candidates come from `overlap_candidates`, which only
                // yields those). So a refusal for any other reason would not prove that the
                // overlap check fired — which is why the message names the ranges.
                let err = validate_edits(SOURCE, &order, &limits).unwrap_err();
                assert_eq!(
                    err.code,
                    ErrorCode::InvalidEdit,
                    "case {}: edits {:?} and {:?} overlap but were refused for another reason: \
                     {err:?}\n{}",
                    case.index,
                    (first.start, first.end),
                    (second.start, second.end),
                    case.repro(),
                );
            }
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_OVERLAP_MIN_EXECUTED);
}

#[test]
fn a_hostile_template_never_panics_and_never_invents_a_capture() {
    let captures: std::sync::Arc<Vec<Capture>> = std::sync::Arc::new(vec![
        Capture {
            name: "X".into(),
            kind: CaptureKind::One,
            start_byte: 0,
            end_byte: 3,
            text: "abc".into(),
        },
        Capture {
            name: "L".into(),
            kind: CaptureKind::List,
            start_byte: 4,
            end_byte: 12,
            text: "a, b, c".into(),
        },
    ]);
    let opts = std::sync::Arc::new(ExpandOptions {
        indent: "  ",
        line_ending: "\n",
        verbatim: &[Range { start: 0, end: 0 }],
        max_output_bytes: 64 * 1024,
    });
    fuzz::run_cases(
        "edit.template",
        SEED,
        FRAGMENTS,
        CASES,
        move |case: &Case| {
            match expand_template(&case.input, &captures, &opts) {
                Ok(text) => {
                    // `X` is bound and `L` is not, so the output may contain `X`'s text but must
                    // never contain `$L`: a name that was never captured cannot appear as if it
                    // had been. (`$$` is the escape for a literal `$`, so `$$X` -> `$X` is
                    // correct output and not a failure here.)
                    assert!(
                        !text.contains("$L"),
                        "an unbound capture was expanded: {text:?}"
                    );
                }
                Err(e) => assert!(
                    matches!(e.code, ErrorCode::InvalidPattern | ErrorCode::LimitExceeded),
                    "a template refused for an unexpected reason: {e:?}"
                ),
            }
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_TEMPLATE_MIN_EXECUTED);
}

/// `indent_of_line` is the other template helper with byte arithmetic on caller input: any
/// offset, including one past the end and one inside a character, has to be clamped.
#[test]
fn indent_of_line_clamps_any_offset() {
    fuzz::run_cases(
        "edit.indent_of_line",
        SEED,
        FRAGMENTS,
        CASES,
        |case: &Case| {
            let len = SOURCE.len();
            let mut offsets = vec![0, len, len + 100, usize::MAX];
            for chunk in case.bytes.chunks(8).take(2) {
                if chunk.len() == 8 {
                    offsets.push(u64::from_le_bytes(chunk.try_into().unwrap()) as usize);
                }
            }
            for offset in offsets {
                let indent = indent_of_line(SOURCE, offset);
                assert!(
                    indent.bytes().all(|b| b == b' ' || b == b'\t'),
                    "{indent:?} at {offset} is not indentation"
                );
            }
            Ran::checked()
        },
    )
    .assert_executed_fraction(EDIT_INDENT_OF_LINE_MIN_EXECUTED);
}
