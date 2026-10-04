//! Spec for the parse that turns a human's typed line into a decision.
//!
//! # Why this file exists
//!
//! [`Answer::from_word`] is the only function in the program that interprets what a person typed.
//! Every other test in this crate reaches a confirmation through a [`ScriptedConfirmer`] or an
//! [`AlwaysYes`], both of which **inject an already-decided [`Answer`]** and never go near the
//! parse. That is the right way to test the gate — a test must not depend on whether the machine
//! running it has a terminal — but it means the parse itself was reachable only by inspection.
//!
//! So the gate could accept `n`, `no` and `maybe` as consent, or drop the `.trim()` that makes a
//! leading space turn `y` into a refusal, and the suite stayed green: a test that cannot fail is
//! not a test. This file enumerates the whole truth table instead, including the cases that must
//! NOT be yes, which is where a permissive match actually shows itself.
//!
//! # The table, not "refusals happen"
//!
//! Each case asserts one exact [`Answer`]. Asserting only that the negatives are not `Yes` would
//! pass if the function returned `No` for every input including `y` — a gate that refuses
//! everything is closed, and closed to the operator's own `y` as well.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast::confirm::Answer;

/// The whole truth table, exactly as [`Answer::from_word`] defines it.
///
/// `(typed line, expected answer, what the case is for)`.
const TRUTH_TABLE: &[(&str, Answer, &str)] = &[
    // --- yes: the three accepted spellings, and nothing else ---
    (
        "",
        Answer::Yes,
        "an empty line is yes: pressing return is consent",
    ),
    ("y", Answer::Yes, "the short form"),
    ("yes", Answer::Yes, "the long form"),
    // --- case is not a different answer ---
    ("Y", Answer::Yes, "upper single letter"),
    ("YES", Answer::Yes, "upper long form"),
    // --- surrounding whitespace is not part of the word ---
    (" y ", Answer::Yes, "trimmed on both sides"),
    ("  yes", Answer::Yes, "trimmed on the left"),
    ("yes\n", Answer::Yes, "the newline read_line leaves behind"),
    ("\t y \t\n", Answer::Yes, "tabs are whitespace too"),
    // --- the explicit noes ---
    ("n", Answer::No, "the short refusal"),
    ("no", Answer::No, "the long refusal"),
    (
        "N",
        Answer::No,
        "case does not turn a no into something else",
    ),
    ("NO", Answer::No, "upper long refusal"),
    (" n ", Answer::No, "a trimmed no is still a no"),
    ("no\n", Answer::No, "trailing newline on a refusal"),
    // --- near-misses: a prefix is not consent ---
    //
    // These are the rows a permissive match grows. `yep` starts with `y`, `maybe` starts with `m`
    // which is not `n` at all, and a prefix match on `no` would accept `nothing` — all of which
    // a human types by accident while meaning something else.
    ("maybe", Answer::No, "not a synonym for yes"),
    ("yep", Answer::No, "starts with y but is not y"),
    ("ye", Answer::No, "a truncated yes is not a yes"),
    ("yea", Answer::No, "nor is this one"),
    ("yess", Answer::No, "nor this one"),
    ("ya", Answer::No, "nor this one"),
    ("nope", Answer::No, "a refusal stays a refusal"),
    (
        "nothing",
        Answer::No,
        "a prefix match on no would wrongly accept this",
    ),
    (
        "no thanks",
        Answer::No,
        "trailing words do not turn a no into a yes",
    ),
    // --- numeric and boolean are not consent either ---
    (
        "true",
        Answer::No,
        "a boolean-looking string is not consent",
    ),
    ("false", Answer::No, "nor is its opposite"),
    ("1", Answer::No, "a digit is not y"),
    ("0", Answer::No, "nor the other digit"),
    ("on", Answer::No, "nor a switch setting"),
    // --- non-ASCII: neither ASCII-folded nor silently accepted ---
    //
    // `to_ascii_lowercase` leaves non-ASCII bytes alone, so a full-width or accented letter is
    // simply not in the accepted set. A Unicode-aware fold (`unicode-normalization`, say) would
    // change these rows, which is why they are pinned rather than left to the default arm.
    ("ｙ", Answer::No, "full-width y is not the ASCII y"),
    ("ＹＥＳ", Answer::No, "full-width YES is not YES"),
    ("是", Answer::No, "a CJK yes glyph is not yes"),
    ("sí", Answer::No, "an accented form is not y"),
    (
        "y\u{200b}",
        Answer::No,
        "a zero-width space makes it a different word",
    ),
    // --- whitespace-only trims to the empty line ---
    //
    // The match runs on `word.trim()`, so a line holding only spaces is not "a blank line" — it
    // IS the empty line, and the empty line is consent. This is the row that makes the trim
    // observable in both directions: ` y ` is yes *because* trimming happens, and `  ` is yes for
    // the same reason. A parser that matched the raw string would refuse both.
    (
        " ",
        Answer::Yes,
        "whitespace only trims to the empty line, which is consent",
    ),
    ("   ", Answer::Yes, "and so does more of it"),
    (
        "\n",
        Answer::Yes,
        "the newline read_line leaves behind an otherwise empty line",
    ),
];

#[test]
fn from_word_maps_the_documented_table_and_nothing_else() {
    for (typed, expected, why) in TRUTH_TABLE {
        assert_eq!(
            Answer::from_word(typed),
            *expected,
            "typing {typed:?} should be {expected:?} — {why}"
        );
    }
}

/// Every `No` above must really be a refusal rather than "nobody was there".
///
/// [`Answer::No`] means somebody said no; [`Answer::Refused`] means there was nobody to ask. A
/// gate treats them identically, but they are different events, and a parse that returned
/// `Refused` for a misspelling would be indistinguishable in this table from a deliberate refusal.
#[test]
fn from_word_says_no_rather_than_refused_when_a_person_answered() {
    for (typed, expected, why) in TRUTH_TABLE {
        let answer = Answer::from_word(typed);
        if *expected == Answer::No {
            assert_ne!(
                answer,
                Answer::Refused,
                "typing {typed:?} is a person answering no, not nobody answering: {why}"
            );
        }
    }
}

/// Only `Yes` grants the write.
///
/// [`Answer::granted`] is what every caller actually branches on, so the table is pinned here
/// too: `""` and `"y"` grant, and nothing in the middle of the table leaks a grant.
#[test]
fn only_the_accepted_spellings_grant_the_write() {
    for (typed, expected, why) in TRUTH_TABLE {
        assert_eq!(
            Answer::from_word(typed).granted(),
            *expected == Answer::Yes,
            "typing {typed:?} should grant={} — {why}",
            *expected == Answer::Yes
        );
    }
    assert!(!Answer::No.granted());
    assert!(!Answer::Refused.granted());
}

/// The round trip that makes the `Display` output usable again.
///
/// The prompt prints an answer with [`Display`] and a caller may feed it straight back into
/// [`from_word`]. `No` and `Refused` must not round-trip into `Yes`, or a logged refusal would
/// become consent on replay.
#[test]
fn a_displayed_answer_round_trips_through_the_parse() {
    for (typed, _, _) in TRUTH_TABLE {
        let answer = Answer::from_word(typed);
        let redisplayed = answer.to_string();
        assert_eq!(
            Answer::from_word(&redisplayed),
            answer,
            "the answer to {typed:?} displayed as {redisplayed:?} must parse back to itself"
        );
    }
}
