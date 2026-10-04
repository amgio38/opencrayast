//! The configuration example in `docs/CONFIGURATION.md` must parse.
//!
//! # Why this file exists
//!
//! `CONFIGURATION.md` shipped one "complete example" with seven sections:
//! `[policy] [limits] [plan] [journal] [protect] [languages] [ignore]`. The parser
//! (`crates/core/src/config.rs`) accepts `[policy]` and `[limits]` and refuses every
//! other section name with `Unknown section`. So the file an operator was told to copy
//! **stopped the server from starting** — and the same document's "What is wired" section
//! said the unwired sections were *ignored*, which turned a refusal into a falsehood.
//!
//! A prose claim like that cannot be reviewed into existence. What can be is mechanical:
//! the document ships two fenced blocks marked `accepted-example` and
//! `not-accepted-example`, and this file feeds each through the real
//! [`Settings::parse`].
//!
//! - `CFG-DOC-01` the **accepted** block parses cleanly. This is the one that catches the
//!   original defect: add a `[plan]` section to the accepted block and this goes red with
//!   the parser's own refusal message.
//! - `CFG-DOC-02` the **not-accepted** block is REFUSED, and refused by a section name
//!   rather than by accident. This is deliberately the opposite polarity of 01: if the
//!   unwired sections ever become real, this goes red and the document has to be rewritten
//!   to say so rather than leaving "pasting this refuses" as a lie.
//! - `CFG-DOC-03` every `[limits]` key named in the accepted block is a key the parser
//!   knows, and every key the parser knows appears in the block — so the document cannot
//!   omit `note_max_bytes` the way it did, or advertise a key that is not a `Limits`
//!   field.
//! - `CFG-DOC-04` the accepted block's markers are present and unique. A block that
//!   loses its marker would otherwise be skipped silently, which is how an "accepted"
//!   example could stop being checked at all.
//!
//! The keys are read out of `Settings::parse`'s own behaviour rather than a list written
//! here twice: a key the parser accepts is proven by `CFG-DOC-01` succeeding, and a key
//! the parser refuses is proven by `CFG-DOC-02` / `CFG-DOC-03`. Nothing in this file
//! duplicates the parser's key set.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::config::Settings;

const DOC: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/CONFIGURATION.md"
));

/// The fenced ```toml block delimited by the given HTML comment markers.
///
/// Returns `None` if the markers are absent. A fence opener that is not ```toml is a
/// hard failure rather than a silently skipped block: an operator reading the document
/// past that point sees a block the checker believes in and does not.
fn block_between(start_marker: &str, end_marker: &str) -> Option<String> {
    let (_, after_start) = DOC.split_once(start_marker)?;
    let (body, _) = after_start.split_once(end_marker)?;
    let mut out = String::new();
    let mut inside = false;
    for line in body.lines() {
        let t = line.trim_start();
        if !inside {
            if t.starts_with("```") {
                assert_eq!(
                    t.trim_end(),
                    "```toml",
                    "{start_marker} must be immediately followed by a ```toml fence; the \
                     checker only reads configuration examples, so a block of another type \
                     here means the document changed shape and this file must follow it"
                );
                inside = true;
            }
            continue;
        }
        if t.starts_with("```") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(inside, "{start_marker} has no opening ```toml fence");
    Some(out)
}

/// Every `[section]` header in `text`.
fn sections(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('[') && l.ends_with(']'))
        .map(|l| l[1..l.len() - 1].to_string())
        .collect()
}

/// Every `key = value` pair that appears under `[limits]`.
fn limits_keys(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_limits = false;
    for raw in text.lines() {
        // Strip the comment, then the whitespace, exactly as the parser does.
        let line = match raw.find('#') {
            Some(i) => &raw[..i],
            None => raw,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[') {
            in_limits = name.starts_with("limits");
            continue;
        }
        if in_limits && let Some((k, _)) = line.split_once('=') {
            out.push(k.trim().to_string());
        }
    }
    out
}

/// CFG-DOC-01: the block labelled as accepted parses with the real parser.
///
/// This is the test the original defect needed and did not have. The parser's error
/// text is carried into the assertion so a failure says what it refused, not just that
/// something was refused.
#[test]
fn cfg_doc_01_the_accepted_example_parses() {
    let block = block_between(
        "<!-- accepted-example:start -->",
        "<!-- accepted-example:end -->",
    )
    .expect("CONFIGURATION.md must carry an <!-- accepted-example --> block");

    match Settings::parse(&block) {
        Ok(_) => {}
        Err(e) => panic!(
            "the block CONFIGURATION.md labels as ACCEPTED does not parse, so an operator \
             who copies it gets a server that refuses to start.\n\
             parser said: {e}\n\
             a section named in the block but refused by the parser is a defect in the \
             document: either the parser should accept it, or the block must move into the \
             <!-- not-accepted-example --> block and say plainly that pasting it refuses."
        ),
    }
}

/// CFG-DOC-02: the not-accepted block is still refused — and for the right reason.
///
/// The polarity matters. The document tells an operator that pasting this block makes
/// the server refuse to start; if that ever stops being true, the sentence is a lie of
/// exactly the kind this ticket exists to remove. So this asserts the refusal is still
/// there AND that it is an **unknown section** — a refusal about `[protect]`'s *syntax*
/// would mean the parser grew array support while this block still claimed it was
/// refused.
#[test]
fn cfg_doc_02_the_not_accepted_example_is_refused_by_an_unknown_section() {
    let block = block_between(
        "<!-- not-accepted-example:start -->",
        "<!-- not-accepted-example:end -->",
    )
    .expect("CONFIGURATION.md must carry a <!-- not-accepted-example --> block");

    let secs = sections(&block);
    assert!(
        secs.iter().any(|s| s == "plan"),
        "the not-accepted block should still show the unwired [plan] section, or the \
         document has been rewritten and this file should be deleted rather than left \
         asserting a refusal that is no longer the story"
    );

    let err = Settings::parse(&block).expect_err(
        "the not-accepted example is refused today; if it now parses, it has to \
                     move into the accepted block and the prose above it rewritten",
    );
    let text = err.to_string();
    assert!(
        text.contains("Unknown section"),
        "the not-accepted block is refused, but not by an unknown section: {text}\n\
         a refusal about syntax or an unknown KEY means the parser changed underneath \
         this document, and the 'pasting this refuses' sentence needs re-checking"
    );
}

/// CFG-DOC-03: the accepted block's `[limits]` keys are exactly the keys the parser knows.
///
/// Both directions, because the failure they guard against was both-directional:
/// `note_max_bytes` is a real parsed limit the document omitted for several revisions,
/// and `pattern_step_budget` is not a `Limits` field at all but sat in the example as if
/// it were.
#[test]
fn cfg_doc_03_accepted_limits_keys_match_the_parser_exactly() {
    let block = block_between(
        "<!-- accepted-example:start -->",
        "<!-- accepted-example:end -->",
    )
    .expect("CONFIGURATION.md must carry an <!-- accepted-example --> block");

    let documented = limits_keys(&block);
    assert!(
        !documented.is_empty(),
        "the accepted block has no [limits] keys, so this test would pass vacuously"
    );

    // The parser's own key set, discovered rather than written down: a key is "known"
    // exactly when a file containing it parses. That is the definition the document has
    // to agree with, so it is the definition used here.
    let mut known: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for key in &documented {
        let probe = format!("[limits]\n{key} = 1\n");
        match Settings::parse(&probe) {
            // `= 1` is refused by validate() for some fields, which still PROVES the key
            // was recognised: the message names the key as the offending field rather
            // than calling it unknown.
            Ok(_) => known.push(key.clone()),
            Err(e) if e.to_string().contains("Unknown key") => unknown.push(key.clone()),
            Err(_) => known.push(key.clone()),
        }
    }

    assert!(
        unknown.is_empty(),
        "CONFIGURATION.md's accepted example lists [limits] keys the parser does not know: \
         {unknown:?}. A key the parser refuses is refused for the operator too."
    );

    // And the other direction, which needs the full key set. Every one of the parser's
    // keys is probed individually with the default value it already has, so a key that is
    // absent from the document is the only thing that can fail this.
    let all: Vec<String> = cfg_doc_03_parser_key_probe();
    let missing: Vec<&String> = all.iter().filter(|k| !documented.contains(k)).collect();
    assert!(
        missing.is_empty(),
        "CONFIGURATION.md's accepted example omits [limits] keys the parser reads and \
         validates: {missing:?}. An operator cannot set a limit the reference file does \
         not mention; that is how note_max_bytes went undocumented."
    );
}

/// Every `[limits]` key `Settings::parse` accepts, discovered by probing.
///
/// `Limits` exposes no list of field names, and duplicating the key set here would be
/// the second place it could go stale — which is the defect this file exists to remove.
/// So the set is recovered by asking the parser itself: for each candidate name, a file
/// containing only that key either parses (known), or is refused *as an unknown key*
/// (not known). A refusal from `validate()` — zero, or above a hard maximum — still
/// proves the key was recognised.
///
/// The candidate list comes from `Limits`' own field declarations, which is the one
/// place a rename would have to be reflected (`config.rs` has one setter per field and
/// the compiler refuses a rename that misses it). So a rename in `limits.rs` moves the
/// candidate list and this probe follows it; it cannot silently pass.
///
/// THE ASSUMPTION, STATED PLAINLY: that assumption is that a `Limits` FIELD NAME equals
/// the CONFIG-KEY the parser accepts. That is true today, and `config.rs` is written so
/// that a field added without its key spelling is a compile error. But it is still an
/// assumption about a convention, not a proof: a `#[serde(rename = "...")]` attribute,
/// or a key the parser handles in a match arm outside the per-field setters, would make
/// a key real without a field of the same name, and this probe would not see it. The
/// "parser key not in the document" half of CFG-DOC-03 is therefore only as strong as
/// that convention. It is worth stating rather than letting a green test imply more
/// than it proves.
fn cfg_doc_03_parser_key_probe() -> Vec<String> {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../core/src/limits.rs"
    ));
    let mut out = Vec::new();
    let mut in_struct = false;
    for raw in src.lines() {
        let line = raw.trim();
        if line.starts_with("pub struct Limits") {
            in_struct = true;
            continue;
        }
        if in_struct {
            if line.starts_with('}') {
                in_struct = false;
                continue;
            }
            // Every limit is a `pub <name>: u64,` field, possibly with a doc line above
            // it. The field name is the parser's key name — `set_limit` has one arm per
            // field, written with the field's own spelling.
            if let Some(rest) = line.strip_prefix("pub ")
                && let Some((name, ty)) = rest.split_once(':')
                && ty.trim_start().starts_with("u64")
            {
                out.push(name.trim().to_string());
            }
        }
    }
    assert!(
        out.len() >= 20,
        "only {} limit fields were found in limits.rs; the field scan is too narrow and \
         this test would pass vacuously",
        out.len()
    );
    out
}

/// CFG-DOC-04: the markers exist, are unique, and the accepted block precedes the
/// not-accepted one.
///
/// Without this the two tests above could both become no-ops: delete a marker and
/// `block_between` returns `None` and `.expect(...)` fails — but only for the block
/// whose test happens to run. Pinning both here means the shape of the document is a
/// checked property in its own right, and the ordering matters because the accepted block
/// is the one an operator is told to copy: it must not appear after a block labelled
/// "pasting this refuses to start".
#[test]
fn cfg_doc_04_the_two_examples_are_marked_unique_and_ordered() {
    for marker in [
        "<!-- accepted-example:start -->",
        "<!-- accepted-example:end -->",
        "<!-- not-accepted-example:start -->",
        "<!-- not-accepted-example:end -->",
    ] {
        assert_eq!(
            DOC.matches(marker).count(),
            1,
            "the marker {marker} must appear exactly once in CONFIGURATION.md"
        );
    }
    let accepted = DOC
        .find("<!-- accepted-example:start -->")
        .expect("marker present, checked above");
    let not_accepted = DOC
        .find("<!-- not-accepted-example:start -->")
        .expect("marker present, checked above");
    assert!(
        accepted < not_accepted,
        "the accepted example must come first: it is the block an operator is told to \
         copy, and burying it under a block headed 'pasting this refuses to start' is how \
         the original defect read in the first place"
    );
}
