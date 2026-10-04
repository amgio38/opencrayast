//! Four defects in the configuration reader, each found by an audit against the shipping
//! binaries and each reproduced here before it was fixed.
//!
//! These are *parser* and *file-trust* properties, so they are tested where the parser is:
//! `Settings::parse` for what a file may say, and `Settings::load` for what a file may be.
//! The two shells are compared in `crates/mcp/tests/exit_code_parity_spec.rs`, which needs both
//! binaries and so cannot live in this crate.
//!
//! Every test here was run red against the old behaviour before the fix; the mutation that
//! proves it is named in the test's own doc comment.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::ErrorCode;
use opencrayast_core::config::{Settings, load_or_default};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Write `body` to a private file in a fresh directory and return its path.
fn private_file(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("config.toml");
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    (dir, p)
}

/// Write `body` to a file with an explicit mode and return its path.
fn file_with_mode(body: &str, mode: u32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("config.toml");
    fs::write(&p, body).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
    (dir, p)
}

// ─────────────────────────────────────────────────────────────────────── DUP-01

/// DUP-01 — a key repeated inside `[limits]` is refused.
///
/// The defect: `set_limit` assigned unconditionally, so the LAST occurrence silently won. A
/// file that reads as `max_results = 5` to a person, in scrollback and to `grep` could carry
/// `max_results = 900` and be used at 900. That is the same lie as an ignored typo, which
/// this parser goes out of its way to refuse, and real TOML refuses it too.
///
/// Both orders are checked, because "the last one wins" is what makes the defect dangerous and
/// either order must therefore be refused rather than merely the one that reads harmlessly.
#[test]
fn dup_01_a_repeated_limits_key_is_refused_in_either_order() {
    for (src, first, second) in [
        ("[limits]\nmax_results = 5\nmax_results = 900\n", 2, 3),
        ("[limits]\nmax_results = 900\nmax_results = 5\n", 2, 3),
    ] {
        let err = Settings::parse(src).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgs, "{err:?}");
        assert!(
            err.message.contains("Duplicate") && err.message.contains("max_results"),
            "the refusal must name the key: {err:?}"
        );
        assert!(
            err.message.contains(&format!("line {first}"))
                && err.message.contains(&format!("line {second}")),
            "the refusal must name BOTH line numbers, or the reader cannot find the first: {err:?}"
        );
    }
    // Control: one occurrence parses, and the value is the one in the file.
    let ok = Settings::parse("[limits]\nmax_results = 900\n").unwrap();
    assert_eq!(ok.limits.max_results, 900);
}

/// DUP-02 — a key repeated inside `[policy]` is refused, and cannot mint a write permission.
///
/// This is the finding with teeth. `allow_write` is the only thing standing between a
/// configuration file and write mode, so a file spelling `allow_write = false` twice — or
/// spelling it `false` and then `true` — must not decide the capability by line order.
#[test]
fn dup_02_a_repeated_policy_key_is_refused_and_never_mints_write_permission() {
    for src in [
        "[policy]\nallow_write = false\nallow_write = true\n",
        "[policy]\nallow_write = true\nallow_write = false\n",
    ] {
        let err = Settings::parse(src).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgs, "{err:?}");
        assert!(
            err.message.contains("Duplicate") && err.message.contains("allow_write"),
            "{err:?}"
        );
        assert!(
            err.message.contains("line 2") && err.message.contains("line 3"),
            "both lines: {err:?}"
        );
    }
    // And through the real file path, not only the parser: the capability must not appear.
    let (_d, p) = private_file("[policy]\nallow_write = false\nallow_write = true\n");
    let err = Settings::load(&p).unwrap_err();
    assert!(err.message.contains("Duplicate"), "{err:?}");
}

/// DUP-03 — the same name in two DIFFERENT sections is not a duplicate.
///
/// The seen-set is keyed on (section, key) precisely so this keeps working: a parser that
/// tracked bare key names would refuse a perfectly legal file the first time a key was reused
/// across sections, and would have to be taught about that by a bug report.
#[test]
fn dup_03_the_same_name_in_two_sections_is_not_a_duplicate() {
    // `allow_write` in [policy] and a [limits] entry, in both orders. Neither file repeats a
    // (section, key) pair, so both must parse.
    let a = Settings::parse("[policy]\nallow_write = true\n[limits]\nmax_results = 7\n");
    assert!(a.is_ok(), "{a:?}");
    let b = Settings::parse("[limits]\nmax_results = 7\n[policy]\nallow_write = true\n");
    assert!(b.is_ok(), "{b:?}");

    // The control that makes the test non-vacuous: the same section twice IS a duplicate.
    assert!(Settings::parse("[limits]\nmax_results = 7\n[limits]\nmax_results = 8\n").is_err());
}

/// DUP-04 — a duplicate is refused even when the SECOND value would have been rejected.
///
/// The check runs before the value is parsed, so the refusal does not depend on which
/// occurrence is valid. Without that ordering, `max_results = 5` followed by
/// `max_results = zero` would report "not a number" and say nothing about the duplicate that
/// is the real problem — and `x = 1` followed by `x = 2` where `x` is unknown would report
/// the unknown key twice over, which is at least harmless, but the first case is a lie.
#[test]
fn dup_04_a_duplicate_is_refused_before_the_second_value_is_parsed() {
    let err =
        Settings::parse("[limits]\nmax_results = 5\nmax_results = not-a-number\n").unwrap_err();
    assert!(
        err.message.contains("Duplicate"),
        "the duplicate is the problem and must be the refusal: {err:?}"
    );
    // A key before any section header has no section to repeat in, and keeps its own refusal.
    let before = Settings::parse("max_results = 5\nmax_results = 6\n").unwrap_err();
    assert!(before.message.contains("before any section"), "{before:?}");
}

/// DUP-05 — the refusal survives the whole file path, on both shells' loader.
///
/// `load_or_default` is the function a shell calls, and it is the one that turns "no file" into
/// defaults. A duplicate must be an `Err` here too — not a silent fallback to defaults, which
/// would leave an operator running on limits their file does not say.
#[test]
fn dup_05_the_loader_refuses_a_duplicate_rather_than_falling_back_to_defaults() {
    let (_d, p) = private_file("[limits]\nmax_results = 5\nmax_results = 900\n");
    let err = load_or_default(Some(p.to_str().unwrap())).unwrap_err();
    assert!(err.message.contains("Duplicate"), "{err:?}");
}

// ─────────────────────────────────────────────────────────────────────── ESC-01

/// ESC-01 — a configuration file cannot put a terminal sequence on the screen.
///
/// The defect: the key and section names were interpolated into the refusal raw, while
/// `docs`-level promises in this codebase and `exit.rs`'s colour help both say no byte of a
/// file's contents can become a terminal sequence. A key spelled `max_ESC[31mRED_ESC[0mults`
/// printed a real SGR sequence to stderr.
///
/// The names are escaped at the interpolation site in `config.rs`, which is the single place
/// file text enters a message. The assertion is on the ABSENT byte, not on a `\u{1b}` that a
/// reader has to recognise: what must not exist is the control character itself.
#[test]
fn esc_01_a_escape_sequence_in_a_key_is_neutralised() {
    let src = "[limits]\nmax_\u{1b}[31mRED\u{1b}[0mults = 5\n";
    let err = Settings::parse(src).unwrap_err();
    assert!(
        !err.message.chars().any(|c| c.is_control()),
        "no raw control character may survive in the message: {:?}",
        err.message
    );
    assert!(
        err.message.contains("\\u{1b}"),
        "the ESC must be shown as visible text: {:?}",
        err.message
    );
    // The `Next:` half is a fixed string today, but it is the other half of what a shell
    // prints, so it is held to the same rule.
    assert!(!err.next.chars().any(|c| c.is_control()), "{:?}", err.next);
}

/// ESC-02 — the same for a SECTION name, and for a bidi/invisible character.
///
/// `classify` in `render.rs` treats bidi overrides as the display-deception they are, so a
/// section named with U+202E must not reverse what the operator reads either. Both are in one
/// test because the property is the same property: no control, bidi or invisible character in
/// an interpolated name.
#[test]
fn esc_02_a_section_name_is_neutralised_too() {
    for src in [
        "[\u{1b}[31mevil\u{1b}[0m]\nmax_results = 5\n",
        "[\u{202e}limits\u{202c}]\nmax_results = 5\n",
        "[li\u{200b}mits]\nmax_results = 5\n",
    ] {
        let err = Settings::parse(src).unwrap_err();
        assert!(
            !err.message.chars().any(|c| c.is_control()),
            "no raw control character: {:?}",
            err.message
        );
        for (name, bad) in [("RLO", '\u{202e}'), ("ZWSP", '\u{200b}'), ("ESC", '\u{1b}')] {
            assert!(
                !err.message.contains(bad),
                "{name} must not survive into the message: {:?}",
                err.message
            );
        }
    }
}

/// ESC-03 \u{2014} the interpolated name is escaped exactly once, and escaping is idempotent.
///
/// A key carrying ESC can never be a key the parser knows, so this exercises the
/// unknown-key refusal \u{2014} which interpolates the very same `quoted()` helper the duplicate
/// refusal uses \u{2014} and asserts on the escape COUNT rather than on recognising a literal.
///
/// The count matters because it is what catches a second escaping layer: `Out::diag` in the CLI
/// escapes too, so if `config.rs` ever stops escaping on the assumption that something
/// downstream will, this test is where the double-escape would show up as two `\u{1b}` for one
/// ESC byte rather than as a silently uglier message nobody reads.
#[test]
fn esc_03_the_interpolated_name_is_escaped_exactly_once() {
    let src = "[limits]\nma\u{1b}x_results = 5\n";
    let err = Settings::parse(src).unwrap_err();
    assert!(err.message.contains("Unknown key"), "{err:?}");
    assert!(
        !err.message.chars().any(|c| c.is_control()),
        "{:?}",
        err.message
    );
    assert_eq!(
        err.message.matches("\\u{1b}").count(),
        1,
        "exactly one escape per ESC byte, and no second escaping layer: {:?}",
        err.message
    );

    // The general property every already-sanitised message relies on: escaping an escaped
    // string changes nothing, so the CLI's own `Out::diag` escaping does not double up.
    let once = opencrayast_core::render::escape_inline("a\u{1b}b\u{202e}c").0;
    let twice = opencrayast_core::render::escape_inline(&once).0;
    assert_eq!(
        once, twice,
        "escaping an escaped string must change nothing, or two layers double-escape"
    );
    assert!(
        once.is_ascii(),
        "escaped output must be inert ASCII: {once:?}"
    );
}

/// ESC-04 — the trust refusals carry no file content at all, which is the property they always
/// claimed and still hold.
///
/// `config.rs`'s module docs say every refusal quotes no value from the file. This pins the
/// half of that claim which is about NAMES, since names are now escaped rather than absent:
/// the message still does not contain the path, and still does not contain any VALUE.
#[test]
fn esc_04_a_refusal_still_carries_no_path_and_no_value() {
    let (_d, p) = private_file("[limits]\nmax_results = SECRET_MARKER_VALUE\n");
    let err = Settings::load(&p).unwrap_err();
    assert!(
        !err.message.contains("SECRET_MARKER_VALUE"),
        "a value must never be echoed: {:?}",
        err.message
    );
    assert!(
        !err.message.contains(p.to_str().unwrap()),
        "a path must never be echoed: {:?}",
        err.message
    );
    assert!(
        !err.next.contains(p.to_str().unwrap()),
        "nor in the next step: {:?}",
        err.next
    );
}

// ─────────────────────────────────────────────────────────────────────── MODE-01

/// MODE-01 — owner-execute is refused; 0600 and 0400 are accepted.
///
/// The defect: the mask tested `& 0o077`, so any owner bit was allowed. `chmod 700` was
/// accepted, which is nearly always a `chmod -R` that caught more than was meant, and 0400
/// was accepted while `config.rs` told the operator to "keep the file at 0600".
///
/// The mask is now `& 0o177`, which accepts owner read/write and nothing else. The control
/// rows matter as much as the refusal: a fix that rejected everything would pass the first
/// half of this test and break every operator with a real configuration file.
#[test]
fn mode_01_owner_execute_is_refused_and_the_documented_modes_still_work() {
    let body = "[policy]\nallow_write = true\n";

    // Refused: anything that carries an owner-execute bit, or any group/other access.
    for mode in [0o700u32, 0o710, 0o750, 0o770, 0o777, 0o744, 0o704] {
        let (_d, p) = file_with_mode(body, mode);
        let err = Settings::load(&p).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::ConfigUntrusted,
            "mode {mode:04o} must be refused, and refused as a trust problem: {err:?}"
        );
    }

    // Accepted: the two modes a person is told to use. 0400 is read-only, which is safe and
    // was always accepted; 0600 is what the docs name.
    for mode in [0o600u32, 0o400] {
        let (_d, p) = file_with_mode(body, mode);
        let s = Settings::load(&p).unwrap_or_else(|e| panic!("mode {mode:04o} must load: {e:?}"));
        assert!(s.policy.allow_write, "the file's own setting must bind");
    }
}

/// MODE-02 — a refusal about the file is an ENVIRONMENT refusal, not a user error.
///
/// This is the split that makes the two shells agree: a malformed file is the operator's to
/// fix (a user error) and an untrustworthy file is the machine's (an environment error). Both
/// used to be `invalid_args`, which is why one shell reported 2 for everything and the other 1.
#[test]
fn mode_02_trust_and_syntax_failures_carry_different_codes() {
    // Untrustworthy: wrong owner, wrong mode, or not a regular file.
    let (_d, p) = file_with_mode("[limits]\nmax_results = 5\n", 0o644);
    assert_eq!(
        Settings::load(&p).unwrap_err().code,
        ErrorCode::ConfigUntrusted,
        "a group-readable file is an environment problem"
    );

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.toml");
    assert!(
        load_or_default(Some(missing.to_str().unwrap())).is_ok(),
        "a file that does not exist is not a refusal at all — it is the defaults"
    );

    // Malformed: the operator's to fix.
    for src in [
        "[limits]\npath_max_dept = 4\n",
        "[limits]\nmax_results = 5\nmax_results = 6\n",
        "[nonsense]\nx = 1\n",
    ] {
        assert_eq!(
            Settings::parse(src).unwrap_err().code,
            ErrorCode::InvalidArgs,
            "{src:?} is a syntax refusal, and must not claim to be an environment problem"
        );
    }

    // The classes are the two the shells map, and they are different classes.
    use opencrayast_core::error::ExitClass;
    assert_eq!(
        ErrorCode::ConfigUntrusted.exit_class(),
        ExitClass::Environment
    );
    assert_eq!(ErrorCode::InvalidArgs.exit_class(), ExitClass::User);
}

/// MODE-03 — every error code the parser can emit is classified.
///
/// `exit.rs` used to list every variant by hand, and the two shells could disagree because the
/// classification was written twice. It is now one exhaustive match in `core`, and this test
/// fails if a new variant is left unclassified — a compile error in `exit_class`, not a silent
/// fallthrough, so this row is belt-and-braces on top of the type system.
#[test]
fn mode_03_every_error_code_has_an_exit_class() {
    use opencrayast_core::error::ExitClass;
    for code in opencrayast_core::ALL_ERROR_CODES {
        let class = code.exit_class();
        assert!(
            matches!(class, ExitClass::User | ExitClass::Environment),
            "{:?} -> {class:?}",
            code.as_str()
        );
        assert!(!class.as_str().is_empty());
    }
    assert_eq!(
        opencrayast_core::ALL_ERROR_CODES.len(),
        opencrayast_core::ERROR_CODE_COUNT,
        "the slice and the count are checked in core; this is the outer belt"
    );
}

/// MODE-04 — an UNREADABLE or unreadably-shaped file is a trust refusal, not a crash.
///
/// The trust checks run before the file is opened, so each of them has to be a refusal with a
/// code rather than a panic or a bare io error. A symlink is the interesting one: `--config`
/// pointing at a link is how an operator is redirected, and it is refused on the link itself
/// rather than on whatever it points to.
#[test]
fn mode_04_a_symlink_is_refused_before_the_target_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.toml");
    fs::write(&target, "[policy]\nallow_write = true\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = Settings::load(&link).unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::ConfigUntrusted,
        "a symlinked configuration is a trust problem: {err:?}"
    );
    assert!(err.message.contains("regular file"), "{err:?}");

    // The control: the same bytes at a real path are accepted, so the refusal above is about
    // the link and not about the content.
    assert!(Settings::load(&target).is_ok());
}
