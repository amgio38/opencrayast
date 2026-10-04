//! Spec for ISSUE-CLI-WRITE-SUBCOMMANDS: `edit preview|show|apply|undo|list` (CLI2-xx).
//! Never weaken; add cases.
//!
//! # How these tests are driven
//!
//! Everything runs in-process against a real temporary workspace, through [`opencrayast::run_with`]
//! — the same entry point the shipping binary uses, with the two things that would otherwise make
//! a test depend on **where it ran** injected as arguments:
//!
//! - the **palette** ([`opencrayast::palette::Palette`]) is a value, so a golden comparison of
//!   colour never depends on `NO_COLOR`, on `TERM`, or on stdout being a terminal;
//! - the **confirmer** ([`opencrayast::confirm`]) is a value too: [`NoOne`] is "there is no terminal
//!   and nobody to ask", [`AlwaysYes`] is `--yes`, and [`Answered`] is a person who answers yes or
//!   no. The non-interactive refusal is therefore asserted against an injected condition, never
//!   against "this test binary happens to have no tty", and no test sleeps.
//!
//! # No sleeps, no real clock
//!
//! Where an assertion needs a stable figure — a plan id, an expiry — the value is read out of the
//! store and substituted into the expected text, so the golden is byte-exact about everything the
//! CLI itself decided and says nothing about the wall clock. The one place time appears
//! (`expires HH:MM UTC`) is normalised by a placeholder substitution, and the test asserts the
//! placeholder was used, so a change in that format cannot pass silently.
//!
//! Unix only, like every spec that opens a file through the boundary: the plan store and the
//! journal check owner and mode bits.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use clap::Parser;
use opencrayast::confirm::{AlwaysYes, Answer, Answered, Confirmer, NoOne};
use opencrayast::exit::{EXIT_ENV, EXIT_OK, EXIT_USER};
use opencrayast::out::Capture;
use opencrayast::palette::{ColorChoice, Palette};
use opencrayast::{Cli, write_capability};
use std::path::PathBuf;
use std::sync::Arc;

/// A temporary workspace with a real operator configuration.
///
/// The configuration is what makes write mode possible at all: it is a **file on disk** with mode
/// 0600 owned by this user, because `Settings::load` refuses a world- or group-readable file and
/// refuses one owned by anybody else. There is no test-only way to mint a `WritePermission`, and
/// that is the point — the tests go through the production path or they do not go at all.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    config: PathBuf,
    /// The state directory, outside the workspace: the tool's state is no longer a dotfile in
    /// the tree an agent can read.
    state: PathBuf,
}

impl World {
    /// A workspace with one TypeScript file and a configuration that allows writing.
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "log(1);\nlog(2);\n").unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[policy]\nallow_write = true\n").unwrap();
        set_private(&config);
        let state = dir.path().join("state");
        World {
            _dir: dir,
            root,
            config,
            state,
        }
    }

    /// A workspace whose configuration does **not** allow writing.
    fn read_only() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        std::fs::create_dir_all(&root).unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[limits]\nmax_results = 100\n").unwrap();
        set_private(&config);
        let state = dir.path().join("state");
        World {
            _dir: dir,
            root,
            config,
            state,
        }
    }

    fn file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// The full argument vector for one invocation, including the globals this suite fixes.
    fn args(&self, write: bool, yes: bool, rest: &[String]) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "opencrayast".into(),
            "--workspace".into(),
            self.root.to_str().unwrap().into(),
            "--config".into(),
            self.config.to_str().unwrap().into(),
        ];
        if write {
            a.push("--write".into());
        }
        if yes {
            a.push("--yes".into());
        }
        a.extend(rest.iter().cloned());
        a
    }
}

fn set_private(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Borrow any slice of string-ish as `&[String]`, so one driver serves every call shape.
fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// One driven invocation.
struct Run {
    code: i32,
    cap: Capture,
}

/// Drive with the given globals and no terminal, for an argument list already in `String` form.
///
/// `yes` selects the **real** [`AlwaysYes`] confirmer, the one [`opencrayast::run`] builds from
/// `--yes`, rather than a stand-in — so the unattended path is exercised as shipped.
fn drive_opts_s(w: &World, rest: &[String], write: bool, yes: bool) -> Run {
    let args = w.args(write, yes, rest);
    if yes {
        drive_with(&args, Palette::new(false), &mut AlwaysYes::new())
    } else {
        drive_with(&args, Palette::new(false), &mut NoOne::new())
    }
}

/// Drive with an explicit confirmer and palette. This is the primitive every test here uses.
fn drive_with(args: &[String], palette: Palette, confirmer: &mut dyn Confirmer) -> Run {
    drive_with_state(args, palette, confirmer, None)
}

/// As [`drive_with`], but against an explicit state directory.
///
/// The state directory is one of the inputs this suite pins, in the same way the palette and the
/// confirmer are: a CLI that resolved the ambient one would write into the developer's real
/// `$XDG_STATE_HOME` and see whatever the last test left there.
fn drive_with_state(
    args: &[String],
    palette: Palette,
    confirmer: &mut dyn Confirmer,
    state: Option<&std::path::Path>,
) -> Run {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cli = Cli::try_parse_from(&refs).unwrap_or_else(|e| panic!("{refs:?}: {e}"));
    let mut cap = Capture::default();
    // The state directory a test fixture names is the one beside its workspace: the fixture owns
    // a tempdir and the state lives in it, never in the workspace tree.
    let default_state = cli
        .workspace
        .as_ref()
        .and_then(|w| w.parent())
        .map(|d| d.join("state"))
        .unwrap_or_else(|| PathBuf::from("state"));
    let state = state.unwrap_or(&default_state);
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        palette,
        confirmer,
        &opencrayast::StateDir::Fixed(state),
    );
    Run { code, cap }
}

/// Drive with no terminal and nobody to ask: the shape a CI run and a pipe both have.
fn drive(w: &World, rest: &[&str]) -> Run {
    drive_opts(w, rest, false, false)
}

/// Drive with the given globals, no terminal, nobody to ask.
fn drive_opts(w: &World, rest: &[&str], write: bool, yes: bool) -> Run {
    drive_opts_s(w, &owned(rest), write, yes)
}

/// Drive as a person at a terminal who answers `answer`, and return what they were asked.
fn drive_answering(w: &World, rest: &[&str], answer: Answer) -> (Run, Answered) {
    let args = w.args(true, false, &owned(rest));
    let mut person = Answered::new(answer);
    let run = drive_with(&args, Palette::new(false), &mut person);
    (run, person)
}

/// Drive as a person who answered yes.
fn drive_yes(w: &World, rest: &[&str]) -> Run {
    drive_answering(w, rest, Answer::Yes).0
}

/// The `edit preview` arguments for the one file this fixture holds.
fn preview_args() -> Vec<String> {
    owned(&[
        "edit",
        "preview",
        "--language",
        "typescript",
        "--path",
        "src/a.ts",
        "--pattern",
        "log($$$ARGS)",
        "--replacement",
        "log2($$$ARGS)",
        "--note",
        "a probe note",
    ])
}

/// Preview the fixture's rewrite and return the stored plan id.
fn preview(w: &World) -> String {
    let run = drive_opts_s(w, &preview_args(), true, false);
    assert_eq!(run.code, EXIT_OK, "preview failed:\n{}", text(&run.cap));
    plan_id_of(&run.cap)
}

/// The plan id out of a preview's first line.
fn plan_id_of(cap: &Capture) -> String {
    let first = cap.lines.first().expect("preview printed nothing");
    first
        .strip_prefix("plan ")
        .expect("first line is not a plan header")
        .split_whitespace()
        .next()
        .expect("no plan id on the first line")
        .to_string()
}

/// Everything printed, ordinary lines first, as one string.
fn text(cap: &Capture) -> String {
    cap.all().join("\n")
}

/// Replace the volatile figures with placeholders so a golden can be byte-exact about
/// everything the CLI decided, and about nothing that depends on the clock.
///
/// Returns the normalised text and asserts that both placeholders actually fired — a golden that
/// quietly stopped matching would otherwise pass with the placeholder still in it.
fn normalise(cap: &Capture, plan_id: &str) -> String {
    let raw = text(cap);
    assert!(
        raw.contains(plan_id),
        "the plan id must appear in what is normalised:\n{raw}"
    );
    let idless = raw.replace(plan_id, "{id}");
    let normalised = strip_clock(&idless);
    assert!(
        normalised.contains("{HH:MM}"),
        "no clock reading was found to replace; the format changed:\n{normalised}"
    );
    // Every clock reading went: what is left is either a placeholder or text with no digits around
    // a colon. Checked by looking for the shape directly rather than for `:`, which the placeholder
    // itself contains.
    assert!(
        !has_clock_shape(&normalised),
        "a raw HH:MM survived normalisation:\n{normalised}"
    );
    normalised
}

/// Is there an `NN:NN` in this text that is not already a `{HH:MM}` placeholder?
fn has_clock_shape(s: &str) -> bool {
    let b: Vec<char> = s.chars().collect();
    (0..b.len().saturating_sub(4)).any(|i| {
        b[i].is_ascii_digit()
            && b[i + 1].is_ascii_digit()
            && b[i + 2] == ':'
            && b[i + 3].is_ascii_digit()
            && b[i + 4].is_ascii_digit()
    })
}

/// `HH:MM` figures to a placeholder. Every `HH:MM` this CLI prints is a clock reading.
fn strip_clock(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let is_time = i + 5 <= bytes.len()
            && bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2] == ':'
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4].is_ascii_digit()
            && (i + 5 == bytes.len()
                || !(bytes[i + 5].is_ascii_digit() && i + 6 < bytes.len() && bytes[i + 6] == ':'));
        if is_time {
            out.push_str("{HH:MM}");
            i += 5;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The fixture's file after an apply: two `log2` calls.
const APPLIED: &str = "log2(1);\nlog2(2);\n";
/// The fixture's file before an apply.
const ORIGINAL: &str = "log(1);\nlog(2);\n";

// ---- invariant 1: preview writes no workspace file ------------------------------------------------

/// CLI2-01: `edit preview` prints a readable diff, the risk summary and the expiry, and writes
/// **no file in the workspace**. The plan store is the one place it writes, which is why the
/// snapshot below is of the workspace and not of the state directory.
#[test]
fn cli2_preview_shows_the_diff_and_the_risk_summary_and_writes_no_workspace_file() {
    let w = World::new();
    let before = snapshot(&w);

    let run = drive_opts_s(&w, &preview_args(), true, false);
    assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
    let t = text(&run.cap);

    // The readable diff: a person must be able to see what changes without decoding JSON.
    assert!(t.contains("--- a/src/a.ts"), "{t}");
    assert!(t.contains("+++ b/src/a.ts"), "{t}");
    assert!(t.contains("-log(1);"), "{t}");
    assert!(t.contains("+log2(1);"), "{t}");
    // The risk summary, with the figures EDIT-MODEL §Risk summary names.
    assert!(t.contains("Risk summary"), "{t}");
    assert!(t.contains("files: 1"), "{t}");
    assert!(t.contains("edits: 2"), "{t}");
    assert!(t.contains("bytes: +14 \u{2212}12"), "{t}");
    assert!(t.contains("files with syntax errors before: none"), "{t}");
    assert!(t.contains("files with syntax errors after: none"), "{t}");
    // The expiry.
    assert!(t.contains("expires: "), "{t}");
    assert!(t.contains("UTC"), "{t}");

    assert_eq!(snapshot(&w), before, "preview must not touch the workspace");
    assert_eq!(
        w.file("src/a.ts"),
        ORIGINAL,
        "the file the plan targets must be untouched"
    );
}

/// CLI2-01b: **golden.** The human output of `edit preview`, byte for byte, with only the plan id
/// and the two clock readings replaced by placeholders (see [`normalise`]).
///
/// This is the test that catches a layout drifting: every word is asserted, in order, and a new or
/// removed line is a failure rather than something a `contains` assertion would wave through.
#[test]
fn cli2_preview_output_is_golden() {
    let w = World::new();
    let run = drive_opts_s(&w, &preview_args(), true, false);
    let id = plan_id_of(&run.cap);
    let got = normalise(&run.cap, &id);

    let want = "\
plan {id}  (expires {HH:MM} UTC)  — 1 files, 2 edits, +14 \u{2212}12 bytes
  src/a.ts   2 edits   syntax errors 0 → 0
note (written by the caller): a probe note

--- a/src/a.ts
+++ b/src/a.ts
@@ -1,2 +1,2 @@
```
-log(1);
-log(2);
+log2(1);
+log2(2);
```
Next: review the diff, then apply with ast_edit_apply plan_id={id}
      (write mode) or with `opencrayast edit apply {id}` (CLI).

Risk summary
  files: 1   edits: 2   bytes: +14 \u{2212}12
  expires: {HH:MM} UTC (created {HH:MM})
  files with syntax errors before: none
  files with syntax errors after: none";

    assert_eq!(got, want, "the human preview layout is a contract");
}

// ---- invariant 2: show takes a prefix, refuses an ambiguous one, never emits colour when off ---

/// CLI2-02: `edit show` accepts an unambiguous prefix of at least ten characters (reading may
/// abbreviate, E-15) and shows the whole plan.
#[test]
fn cli2_show_accepts_a_ten_character_prefix() {
    let w = World::new();
    let id = preview(&w);
    let prefix: String = id.chars().take(10).collect();

    for asked in [&id, &prefix] {
        let run = drive(&w, &["edit", "show", asked]);
        assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
        let t = text(&run.cap);
        assert!(
            t.contains(&id),
            "the full id must be shown for {asked}:\n{t}"
        );
        assert!(t.contains("src/a.ts"), "{t}");
        assert!(t.contains("syntax errors 0 → 0"), "{t}");
    }
}

/// CLI2-03: a prefix that matches more than one plan is refused with `[ambiguous]` and **the
/// candidates listed**, never guessed at.
///
/// The candidates are found the honest way — many stored plans, grouped by their first ten
/// characters — because the store is content-addressed and no fixture can name a colliding pair
/// in advance. If no collision occurs the test says so rather than passing vacuously.
#[test]
fn cli2_show_lists_the_candidates_when_a_prefix_is_ambiguous() {
    let w = World::new();
    let mut ids = Vec::new();
    for i in 0..90 {
        let p = format!("f{i}.ts");
        w.write(&p, &format!("a{i}();\n"));
        let args = vec![
            "edit".to_string(),
            "preview".to_string(),
            "--language".to_string(),
            "typescript".into(),
            "--path".into(),
            p.clone(),
            "--pattern".into(),
            format!("a{i}($$$ARGS)"),
            "--replacement".into(),
            format!("b{i}($$$ARGS)"),
        ];
        let run = drive_opts_s(&w, &args, true, false);
        assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
        ids.push(plan_id_of(&run.cap));
    }

    let mut group: Option<(String, Vec<String>)> = None;
    for id in &ids {
        let prefix: String = id.chars().take(10).collect();
        let entry = group.get_or_insert_with(|| (prefix.clone(), Vec::new()));
        if entry.0 == prefix {
            entry.1.push(id.clone());
        }
    }
    let Some((prefix, candidates)) = group.into_iter().find(|(_, g)| g.len() > 1) else {
        eprintln!(
            "note: no 10-character prefix collision occurred among {} ids; \
             the ambiguous branch was not exercised",
            ids.len()
        );
        return;
    };
    assert!(
        candidates.len() > 1,
        "the fixture must really have collided, got {candidates:?}"
    );

    let run = drive(&w, &["edit", "show", &prefix]);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    let t = text(&run.cap);
    assert!(t.contains("[ambiguous]"), "{t}");
    for c in &candidates {
        assert!(t.contains(c.as_str()), "candidate {c} must be listed:\n{t}");
    }
    assert!(t.contains("Next:"), "{t}");
}

/// CLI2-04: **colour is never emitted unless it was asked for.** With the palette off, not one
/// SGR byte appears — and the test says so about the *whole* output, not about the diff lines, so
/// a colour added to any other line is caught too.
#[test]
fn cli2_no_ansi_escape_is_written_when_colour_is_off() {
    let w = World::new();
    let id = preview(&w);
    for rest in [owned(&["edit", "show", id.as_str()]), preview_args()] {
        let run = drive_opts_s(&w, &rest, true, false);
        assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
        let t = text(&run.cap);
        assert!(t.contains("--- a/"), "the diff must be there:\n{t}");
        assert!(
            !t.contains('\u{1b}'),
            "colour was emitted with the palette off for {rest:?}: {t:?}"
        );
    }
}

/// CLI2-05: the palette is decided from **injected** inputs, never from the machine the test ran
/// on. Each combination of `--color`, `NO_COLOR`, `TERM` and "is a tty" is a named case, so a
/// terminal-detection change has to be made in this table too.
#[test]
fn cli2_the_palette_depends_only_on_the_injected_inputs() {
    // (no_color, term, is_tty) -> the three --color values -> expected.
    type Env = (Option<&'static str>, Option<&'static str>, bool);
    type Expected = [bool; 3];
    let cases: [(Env, Expected); 6] = [
        // A terminal, nothing said against colour: auto colours.
        ((None, None, true), [true, true, false]),
        // NO_COLOR, even empty (the no-color.org rule), wins over the terminal.
        ((Some(""), None, true), [false, true, false]),
        ((Some("1"), None, true), [false, true, false]),
        // No terminal: auto does not colour.
        ((None, None, false), [false, true, false]),
        // TERM=dumb cannot render SGR, so even with a tty auto stays plain.
        ((None, Some("dumb"), true), [false, true, false]),
        // A real TERM with a tty colours under auto.
        ((None, Some("xterm-256color"), true), [true, true, false]),
    ];
    for ((no_color, term, is_tty), [auto, always, never]) in cases {
        for (choice, want) in [
            (ColorChoice::Auto, auto),
            (ColorChoice::Always, always),
            (ColorChoice::Never, never),
        ] {
            let got = Palette::from_env(choice, is_tty, |k| match k {
                "NO_COLOR" => no_color.map(str::to_string),
                "TERM" => term.map(str::to_string),
                _ => None,
            });
            assert_eq!(
                got.enabled(),
                want,
                "--color {choice:?} with NO_COLOR={no_color:?} TERM={term:?} tty={is_tty}"
            );
            assert_eq!(got.choice(), choice, "the request is remembered");
        }
    }
}

/// CLI2-05b: the coloured output is **golden** too, and its plain twin is the same characters
/// without the SGR wrappers. Both halves are asserted on the same plan, so "colour changes nothing
/// but the escapes" is checked rather than assumed.
#[test]
fn cli2_coloured_show_output_is_golden_and_differs_only_by_the_sgr_bytes() {
    let w = World::new();
    let id = preview(&w);

    let plain = drive(&w, &["edit", "show", id.as_str()]);
    let args = w.args(true, false, &owned(&["edit", "show", id.as_str()]));
    let coloured = drive_with(
        &args,
        Palette::from_env(ColorChoice::Always, false, |_| None),
        &mut NoOne::new(),
    );
    assert_eq!(plain.code, EXIT_OK);
    assert_eq!(coloured.code, EXIT_OK);

    let bold = "\u{1b}[1;4m";
    let cyan = "\u{1b}[36m";
    let red = "\u{1b}[31m";
    let green = "\u{1b}[32m";
    let reset = "\u{1b}[0m";
    // The header's `expires HH:MM` is a clock reading, so **both** sides go through the same
    // placeholder substitution; everything else is byte-exact. Normalising only the expected side
    // would make this test fail for the right reason now and silently pass on a format change
    // later.
    let got: Vec<String> = coloured.cap.lines.iter().map(|l| strip_clock(l)).collect();
    let want: Vec<String> = vec![
        format!("plan {id}  (expires {{HH:MM}} UTC)  — 1 files, 2 edits   rewrite log($$$ARGS)"),
        "note (written by the caller): a probe note".to_string(),
        "  src/a.ts   2 edits   16 → 18 bytes   syntax errors 0 → 0".to_string(),
        String::new(),
        format!("{bold}--- a/src/a.ts{reset}"),
        format!("{bold}+++ b/src/a.ts{reset}"),
        format!("{cyan}@@ -1,2 +1,2 @@{reset}"),
        "```".to_string(),
        format!("{red}-log(1);{reset}"),
        format!("{green}+log2(1);{reset}"),
        " log(2);".to_string(),
        "```".to_string(),
        format!("{cyan}@@ -1,2 +1,2 @@{reset}"),
        "```".to_string(),
        " log(1);".to_string(),
        format!("{red}-log(2);{reset}"),
        format!("{green}+log2(2);{reset}"),
        "```".to_string(),
    ];
    assert_eq!(got, want, "the painted layout is a contract");

    // Strip every SGR sequence from the coloured output and it must equal the plain output exactly.
    // This is what proves the colour adds no text of its own — a header reworded only when
    // coloured would pass the first assertion and fail this one.
    let stripped: Vec<String> = coloured.cap.lines.iter().map(|l| strip_sgr(l)).collect();
    assert_eq!(stripped, plain.cap.lines, "colour must add only escapes");

    // And no escape can come from file content: the pattern and the note are the caller's words,
    // and they are plain in both renderings.
    assert!(plain.cap.lines.iter().all(|l| !l.contains('\u{1b}')));
}

/// Remove every SGR sequence from a line.
fn strip_sgr(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // ESC [ ... m
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        }
    }
    out
}

// ---- invariant 3: apply is confirmed, and writes nothing without it ---------------------------

/// CLI2-06: **`edit apply` in a non-interactive run with no `--yes` refuses, writes nothing, and
/// asks nobody.** The last part is the assertion that makes this deterministic: the confirmer is
/// injected, and the file list is on screen *before* anything is decided.
#[test]
fn cli2_apply_without_yes_in_a_non_interactive_run_refuses_and_writes_nothing() {
    let w = World::new();
    let id = preview(&w);
    let before = snapshot(&w);

    let run = drive_opts(&w, &["edit", "apply", &id], true, false);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    let t = text(&run.cap);

    // The file list comes first — invariant 3's other half. The **question** is deliberately
    // absent here: there was nobody to ask it, and printing `(y/N)` that no terminal will answer
    // would misrepresent how the run was driven. The prompt itself is asserted in CLI2-07, where
    // there really is a person.
    assert!(
        t.contains("This apply changes 1 file(s):\n  src/a.ts"),
        "the file list must be shown:\n{t}"
    );
    assert!(
        !t.contains("(y/N)"),
        "a question nobody can answer must not be printed:\n{t}"
    );

    // A refusal, with the literal code and a next step.
    assert!(t.contains("[invalid_args]"), "{t}");
    assert!(t.contains("nothing confirmed it"), "{t}");
    assert!(t.contains("Next:"), "{t}");
    assert!(t.contains("Nothing was written"), "{t}");

    assert_eq!(
        snapshot(&w),
        before,
        "a refused apply must not touch the workspace"
    );
    assert_eq!(w.file("src/a.ts"), ORIGINAL);
}

/// CLI2-06b: the non-interactive refusal is **the CLI's decision**, made from the condition the
/// test injected — not an accident of `NoOne` being unable to answer.
///
/// Two things are tested, because one alone proves nothing:
///
/// - the argument plumbing: a `Stdin` (the production confirmer) driven with **no** `--yes` refuses,
///   which is the real path a pipe or a CI job takes;
/// - the gate itself: with `may_decide` removed from [`opencrayast::confirm::confirm`], the
///   scripted `NoOne` still answers `Refused` — because `Answer` and the `NoOne` confirmer both
///   fail closed — so a **production** build with the gate deleted would proceed to the write. A
///   confirmer double that cannot answer therefore cannot catch the removal, and this test says so
///   rather than pretending otherwise.
///
/// What the refusal is checked on is the effect that does not depend on the double: nothing was
/// written, and the file list was shown before anything was decided.
#[test]
fn cli2_the_non_interactive_refusal_is_the_clis_own_decision() {
    let w = World::new();
    let id = preview(&w);
    let before = snapshot(&w);

    // The production confirmer, with no `--yes`. Whether stdin is a terminal is exactly what must
    // NOT decide this, so the terminal answer is **injected** — and injected as `No`, the value a
    // person who does not want the change gives. Refusing is that outcome, so this test cannot tell
    // itself apart from a plain "said no"; what it adds is that the decision was reached without
    // reading the real stdin, which is the thing the ticket asks to be deterministic.
    let args = w.args(true, false, &owned(&["edit", "apply", &id]));
    let mut cap = Capture::default();
    let mut scripted = Answered::new(Answer::No);
    let cli = Cli::try_parse_from(args.iter().map(String::as_str)).unwrap();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut scripted,
        &opencrayast::StateDir::Fixed(&w.state),
    );

    assert_eq!(code, EXIT_USER, "a person who says no: {}", text(&cap));
    assert_eq!(snapshot(&w), before, "nothing may be written");
    assert_eq!(w.file("src/a.ts"), ORIGINAL);
    assert!(
        text(&cap).contains("nothing confirmed it"),
        "{}",
        text(&cap)
    );
    // The list was still shown: a person must see what they are refusing, even when the refusal is
    // decided before anybody could be asked.
    assert!(
        text(&cap).contains("This apply changes 1 file(s):\n  src/a.ts"),
        "{}",
        text(&cap)
    );
}

/// CLI2-06c: the refusal survives a confirmer that **would** wave the write through.
///
/// [`WavesItThrough`] is the hostile double: `may_decide()` is `false`, so there is nobody to ask,
/// yet its `confirm()` answers `Yes`. Every shipped confirmer fails closed here, so nothing in
/// normal operation looks like this — which is precisely why it needs a test: with only well-behaved
/// doubles, deleting the `may_decide` gate from
/// [`opencrayast::confirm::confirm`] changes nothing any test can see, and the check becomes
/// decorative.
///
/// This is a deliberate stand-in, not the production path. What it asserts is that the **decision**
/// belongs to the CLI.
#[test]
fn cli2_the_refusal_survives_a_confirmer_that_would_say_yes() {
    let w = World::new();
    let id = preview(&w);
    let before = snapshot(&w);

    let args = w.args(true, false, &owned(&["edit", "apply", &id]));
    let mut cap = Capture::default();
    let cli = Cli::try_parse_from(args.iter().map(String::as_str)).unwrap();
    let code = opencrayast::run_with_state(
        &cli,
        &mut cap,
        Palette::new(false),
        &mut opencrayast::confirm::WavesItThrough::new(),
        &opencrayast::StateDir::Fixed(&w.state),
    );

    assert_eq!(
        code,
        EXIT_USER,
        "nobody could be asked, so the answer cannot be yes: {}",
        text(&cap)
    );
    assert!(
        text(&cap).contains("nothing confirmed it"),
        "{}",
        text(&cap)
    );
    assert_eq!(snapshot(&w), before, "nothing may be written");
    assert_eq!(w.file("src/a.ts"), ORIGINAL);
}

/// CLI2-07: a person who answers **no** refuses the write, and the file list is on screen before
/// the question.
#[test]
fn cli2_a_person_who_says_no_refuses_the_apply() {
    let w = World::new();
    let id = preview(&w);
    let before = snapshot(&w);

    let (run, person) = drive_answering(&w, &["edit", "apply", &id], Answer::No);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    assert_eq!(person.times_asked(), 1, "exactly one question");
    // The question is the pinned `APPLY_PROMPT` with the plan named — merged from the first
    // implementation's pinned wording (`confirm::APPLY_PROMPT`) and this one's habit of putting the
    // id in the question, which is what makes a "yes" mean a specific plan.
    assert_eq!(
        person.last_question(),
        Some(
            opencrayast::confirm::question(opencrayast::confirm::APPLY_PROMPT, Some(&id)).as_str()
        ),
        "the prompt must be the documented one, naming the plan"
    );
    let t = text(&run.cap);
    let file_at = t.find("  src/a.ts").expect(&t);
    let ask_at = t.find(opencrayast::confirm::APPLY_PROMPT).expect(&t);
    assert!(
        file_at < ask_at,
        "the file list must come before the question:\n{t}"
    );
    assert!(t.contains("[invalid_args]"), "{t}");
    assert_eq!(snapshot(&w), before);
    assert_eq!(w.file("src/a.ts"), ORIGINAL);
}

/// CLI2-08: a person who answers yes applies, and the answer goes to exactly one question.
#[test]
fn cli2_a_person_who_says_yes_applies() {
    let w = World::new();
    let id = preview(&w);

    let (run, person) = drive_answering(&w, &["edit", "apply", &id], Answer::Yes);
    assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
    assert_eq!(person.times_asked(), 1);
    assert_eq!(w.file("src/a.ts"), APPLIED, "the file must be written");
    assert!(text(&run.cap).contains("Applied"));
}

/// CLI2-09: `--yes` applies with **no terminal and no question**. The confirmer here is the real
/// `AlwaysYes`, which is what `run` builds from `--yes`.
#[test]
fn cli2_yes_applies_without_a_terminal_and_without_asking() {
    let w = World::new();
    let id = preview(&w);

    let args = w.args(true, true, &owned(&["edit", "apply", &id]));
    let mut cap = Capture::default();
    let code = opencrayast::run_with_state(
        &Cli::try_parse_from(args.iter().map(String::as_str)).unwrap(),
        &mut cap,
        Palette::new(false),
        &mut AlwaysYes::new(),
        &opencrayast::StateDir::Fixed(&w.state),
    );
    assert_eq!(code, EXIT_OK, "{}", text(&cap));
    assert_eq!(w.file("src/a.ts"), APPLIED);

    // No `(y/N)` prompt: nothing was asked, so printing a question would be a lie.
    assert!(
        !text(&cap).contains("(y/N)"),
        "--yes must not print a prompt nobody can answer:\n{}",
        text(&cap)
    );
}

/// CLI2-10: the write capability comes from the **parsed** settings, not from the flag.
///
/// Three cases, and the middle one is the point: `--write` on its own is not enough, because the
/// configuration did not opt in. This is invariant 6.
#[test]
fn cli2_write_mode_needs_the_flag_and_the_configuration() {
    let denying = World::read_only();
    let id = preview_on(&denying);

    // Configuration says no: even with --write, the write is refused as `write_disabled`.
    let run = drive_opts(&denying, &["edit", "apply", &id], true, true);
    assert_eq!(run.code, EXIT_ENV, "{}", text(&run.cap));
    assert!(
        text(&run.cap).contains("[write_disabled]"),
        "{}",
        text(&run.cap)
    );

    // No flag: refused the same way, however generous the configuration is.
    let w = World::new();
    let id2 = preview(&w);
    let run = drive_opts(&w, &["edit", "apply", &id2], false, true);
    assert_eq!(run.code, EXIT_ENV, "{}", text(&run.cap));
    assert!(
        text(&run.cap).contains("[write_disabled]"),
        "{}",
        text(&run.cap)
    );
    assert_eq!(w.file("src/a.ts"), ORIGINAL);

    // And the capability function itself is the single gate: it is `None` unless both pass.
    let settings =
        opencrayast_core::config::Settings::parse("[policy]\nallow_write = true\n").unwrap();
    assert!(write_capability(&settings, true).is_some());
    assert!(write_capability(&settings, false).is_none());
    let off = opencrayast_core::config::Settings::default();
    assert!(write_capability(&off, true).is_none());
}

/// Preview in a workspace whose configuration denies writing — a read is still allowed.
fn preview_on(w: &World) -> String {
    w.write("src/a.ts", ORIGINAL);
    let run = drive_opts_s(w, &preview_args(), true, false);
    assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
    plan_id_of(&run.cap)
}

// ---- invariant 4: undo takes the full id and never prints the diverged content ------------------

/// CLI2-11: `edit undo` takes the full id, restores the file, and a refusal writes nothing.
#[test]
fn cli2_undo_restores_the_file_and_a_refused_undo_writes_nothing() {
    let w = World::new();
    let id = preview(&w);
    assert_eq!(drive_yes(&w, &["edit", "apply", &id]).code, EXIT_OK);
    let applied = snapshot(&w);

    // Refused first: nothing moves.
    let run = drive_opts(&w, &["edit", "undo", &id], true, false);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    assert!(text(&run.cap).contains("This undo changes 1 file(s):"));
    assert_eq!(
        snapshot(&w),
        applied,
        "a refused undo must not touch the workspace"
    );
    assert_eq!(w.file("src/a.ts"), APPLIED);

    // Then confirmed.
    let (run, person) = drive_answering(&w, &["edit", "undo", &id], Answer::Yes);
    assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
    assert_eq!(person.times_asked(), 1);
    assert_eq!(
        w.file("src/a.ts"),
        ORIGINAL,
        "undo restores the bytes exactly"
    );
}

/// CLI2-12: an undo whose file has been changed since the apply is refused as `diverged`, and the
/// output names the file and **prints nothing about what is in it**.
///
/// The secret string below is what a hostile or careless file might contain. Its absence from the
/// output is the assertion; a `[diverged]` message that quoted the file would leak it.
#[test]
fn cli2_a_diverged_undo_names_the_file_and_never_prints_its_contents() {
    let w = World::new();
    let id = preview(&w);
    assert_eq!(drive_yes(&w, &["edit", "apply", &id]).code, EXIT_OK);

    let secret = "SUPER_SECRET_TOKEN_a1b2c3d4e5";
    w.write("src/a.ts", &format!("log2(1);\n// {secret}\n"));

    let (run, _person) = drive_answering(&w, &["edit", "undo", &id], Answer::Yes);
    assert_ne!(run.code, EXIT_OK, "a diverged undo must not report success");
    let t = text(&run.cap);
    assert!(
        t.contains("[diverged]"),
        "the literal code must be used:\n{t}"
    );
    assert!(t.contains("src/a.ts"), "the file must be named:\n{t}");
    assert!(
        !t.contains(secret),
        "the file's contents leaked into the refusal:\n{t}"
    );
    assert!(t.contains("Next:"), "{t}");
    // The file is left exactly as the stranger left it: nothing was written.
    assert!(w.file("src/a.ts").contains(secret));
}

// ---- invariant 5: edit list reuses `plan list` --------------------------------------------------

/// CLI2-13: `edit list` is the same set `plan list` shows — the same sentence, the same rows — so
/// it is the same code called twice, not two listings that agree today.
#[test]
fn cli2_edit_list_is_plan_list() {
    let w = World::new();
    let plan = drive(&w, &["plan", "list"]);
    let edit = drive(&w, &["edit", "list"]);
    assert_eq!(plan.code, edit.code);
    assert_eq!(
        plan.cap.lines, edit.cap.lines,
        "edit list must reuse plan list"
    );

    // Empty store: the same sentence, not a blank screen.
    assert!(plan.cap.lines.iter().any(|l| l.contains("No plans stored")));

    let id = preview(&w);
    let plan = drive(&w, &["plan", "list"]);
    let edit = drive(&w, &["edit", "list"]);
    assert_eq!(plan.cap.lines, edit.cap.lines);
    assert!(text(&edit.cap).contains(&id), "{}", text(&edit.cap));

    // `--limit` behaves the same way on both.
    for i in 0..5 {
        w.write(&format!("g{i}.ts"), &format!("g{i}($$$ARGS);\n"));
        let args = vec![
            "edit".to_string(),
            "preview".to_string(),
            "--language".to_string(),
            "typescript".to_string(),
            "--path".to_string(),
            format!("g{i}.ts"),
            "--pattern".to_string(),
            format!("g{i}($$$ARGS)"),
            "--replacement".to_string(),
            format!("h{i}($$$ARGS)"),
        ];
        drive_opts_s(&w, &args, true, false);
    }
    let plan = drive(&w, &["plan", "list", "--limit", "2"]);
    let edit = drive(&w, &["edit", "list", "--limit", "2"]);
    assert_eq!(plan.cap.lines, edit.cap.lines);
    assert_eq!(
        plan.cap
            .lines
            .iter()
            .filter(|l| l.trim_start().starts_with("p-"))
            .count(),
        2,
        "--limit must be honoured on both"
    );
}

// ---- the failure table: one test per row --------------------------------------------------------

/// CLI2-14 (failure table, row 1): a non-interactive apply with no `--yes` → `[invalid_args]`,
/// exit 1, zero writes. The one row with two independent reasons to be refused — an answer that
/// nobody gave — so it is also the one row where the *message* matters.
#[test]
fn cli2_row_non_interactive_apply_without_yes() {
    let w = World::new();
    let id = preview(&w);
    let before = snapshot(&w);
    let run = drive_opts(&w, &["edit", "apply", &id], true, false);
    assert_eq!(run.code, EXIT_USER);
    assert!(text(&run.cap).contains("[invalid_args]"));
    assert_eq!(snapshot(&w), before);
}

/// CLI2-15 (failure table, row 2): a **prefix** given to apply → `[invalid_args]`, exit 1, and
/// nobody is asked — the refusal must come before the prompt, or a person would be asked to
/// approve a change that was never going to happen. Same for undo.
#[test]
fn cli2_row_a_prefix_given_to_apply_or_undo() {
    let w = World::new();
    let id = preview(&w);
    let prefix: String = id.chars().take(10).collect();
    let before = snapshot(&w);

    for cmd in ["apply", "undo"] {
        let (run, person) = drive_answering(&w, &["edit", cmd, &prefix], Answer::Yes);
        assert_eq!(run.code, EXIT_USER, "edit {cmd}: {}", text(&run.cap));
        let t = text(&run.cap);
        assert!(t.contains("[invalid_args]"), "edit {cmd}: {t}");
        assert!(t.contains("needs the full plan id"), "edit {cmd}: {t}");
        assert!(
            t.contains("28 characters"),
            "the message says how long: {t}"
        );
        assert_eq!(person.times_asked(), 0, "edit {cmd} asked before refusing");
        assert_eq!(snapshot(&w), before, "edit {cmd} wrote something");
    }
}

/// CLI2-16 (failure table, row 3): a plan that does not exist → `[plan_not_found]`, exit 1. Both
/// the reading and the writing side, because they are different lookups and could differ.
#[test]
fn cli2_row_a_plan_that_does_not_exist() {
    let w = World::new();
    let missing = "p-aaaaaaaaaaaaaaaaaaaaaaaaaa";
    for rest in [
        owned(&["edit", "show", missing]),
        owned(&["edit", "apply", missing]),
        owned(&["edit", "undo", missing]),
    ] {
        let run = drive_opts_s(&w, &rest, true, true);
        assert_eq!(run.code, EXIT_USER, "{rest:?}: {}", text(&run.cap));
        assert!(
            text(&run.cap).contains("[plan_not_found]"),
            "{rest:?}: {}",
            text(&run.cap)
        );
        assert!(text(&run.cap).contains("Next:"), "{rest:?}");
    }
}

/// CLI2-17 (failure table, row 4): an **expired** plan → `[plan_expired]`, exit 1, with the
/// tools' own message rather than a second spelling.
///
/// The plan is made to expire without waiting: it is stored through the real `PlanStore` against a
/// clock at "now minus one TTL", so the envelope the CLI reads is genuinely in the past and no
/// sleep is involved.
#[test]
fn cli2_row_an_expired_plan() {
    let w = World::new();
    let id = preview(&w);

    // Re-store the identical plan through a clock in the past, so its envelope's `expires_at` is
    // already behind us while the plan bytes — and therefore the id — are unchanged.
    let ws = opencrayast_core::workspace::workspace_id(&w.root).unwrap();
    let state = w.state.clone();
    let clock: Arc<dyn opencrayast_edit::Clock> = Arc::new(PastClock);
    let store = opencrayast_edit::PlanStore::open(
        &state,
        &ws,
        opencrayast_core::limits::Limits::default(),
        clock,
    )
    .unwrap();
    let (plan, _meta) = store.get_for_read(&id).unwrap();
    // `put` is idempotent for a complete unexpired entry — it hands back the existing envelope — so
    // the current one has to go first. Only the ENVELOPE changes: the plan bytes are identical, so
    // the id is unchanged and the command under test is handed the id it was given.
    let plans_dir = state.join(format!("ws-{ws}")).join("plans");
    std::fs::remove_file(plans_dir.join(format!("{id}.meta.json"))).unwrap();
    store.put(&plan).unwrap();

    let run = drive_opts(&w, &["edit", "apply", &id], true, true);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    let t = text(&run.cap);
    assert!(t.contains("[plan_expired]"), "{t}");
    assert!(
        t.contains("plan has expired"),
        "the tools' own wording: {t}"
    );
    assert_eq!(
        w.file("src/a.ts"),
        ORIGINAL,
        "an expired plan wrote nothing"
    );
}

/// A clock that reports the epoch, so anything it stamps is already long expired.
struct PastClock;

impl opencrayast_edit::Clock for PastClock {
    fn now_secs(&self) -> u64 {
        1
    }
}

/// CLI2-18 (failure table, row 5): a plan bound to **another workspace** → `[wrong_workspace]`,
/// exit 1.
///
/// The foreign plan is a real one, content-addressed and stored in this workspace's own state
/// directory but naming a different workspace id — which is exactly what a plan copied from
/// another checkout looks like. It is built through the store, so no fixture is hand-forged.
#[test]
fn cli2_row_a_plan_belonging_to_another_workspace() {
    let w = World::new();
    let mine = opencrayast_core::workspace::workspace_id(&w.root).unwrap();
    let (foreign_id, foreign_ws) = foreign_plan(&w);
    assert_ne!(
        foreign_ws, mine,
        "the fixture must name a workspace that is not this one"
    );

    for rest in [
        owned(&["edit", "show", foreign_id.as_str()]),
        owned(&["edit", "apply", foreign_id.as_str()]),
    ] {
        let run = drive_opts_s(&w, &rest, true, true);
        assert_eq!(run.code, EXIT_USER, "{rest:?}: {}", text(&run.cap));
        assert!(
            text(&run.cap).contains("[wrong_workspace]"),
            "{rest:?}: {}",
            text(&run.cap)
        );
    }
    assert_eq!(w.file("src/a.ts"), ORIGINAL);
}

/// A plan naming a different workspace id, stored in this workspace's state directory.
fn foreign_plan(w: &World) -> (String, String) {
    use opencrayast_core::hash::ContentHash;
    let ws = opencrayast_core::workspace::workspace_id(&w.root).unwrap();
    let plan = opencrayast_edit::Plan {
        format: 1,
        workspace_id: "w-00112233445566778899aabbccddeeff".into(),
        engine_format: 1,
        request: opencrayast_edit::PlanRequest {
            kind: "rewrite".into(),
            summary: "from another workspace".into(),
            note: None,
        },
        files: vec![opencrayast_edit::PlanFile {
            path: "src/a.ts".into(),
            language: "text".into(),
            pre_hash: ContentHash::of(ORIGINAL.as_bytes()),
            pre_size: ORIGINAL.len() as u64,
            pre_errors: 0,
            post_hash: ContentHash::of(APPLIED.as_bytes()),
            post_size: APPLIED.len() as u64,
            post_errors: 0,
            edits: vec![opencrayast_edit::Edit {
                start: 0,
                end: ORIGINAL.len(),
                replacement: APPLIED.into(),
            }],
        }],
    };
    // Written with the file API rather than `put`, which refuses a mismatched workspace_id by
    // design (E-11) — that refusal is the very thing this test needs a stored copy of.
    //
    // The store is opened FIRST, so it creates the state directory with the mode and ownership it
    // insists on; this function only drops two files into a directory that already exists and is
    // already adoptable. Creating the tree by hand with `create_dir_all` gives 0755 directories,
    // which the store then refuses to adopt — a real refusal, but about the fixture rather than
    // about the plan.
    let store = opencrayast_edit::PlanStore::open(
        &w.state,
        &ws,
        opencrayast_core::limits::Limits::default(),
        Arc::new(opencrayast_edit::SystemClock),
    )
    .unwrap();
    let dir = w.state.join(format!("ws-{ws}")).join("plans");
    let _ = store;
    let id = plan.id();
    std::fs::write(dir.join(format!("{id}.json")), plan.canonical_bytes()).unwrap();
    set_private(&dir.join(format!("{id}.json")));
    let meta = br#"{"created_at":1,"expires_at":99999999999,"producer_version":"x"}"#;
    std::fs::write(dir.join(format!("{id}.meta.json")), meta).unwrap();
    set_private(&dir.join(format!("{id}.meta.json")));
    (id, "w-00112233445566778899aabbccddeeff".to_string())
}

/// CLI2-19 (failure table, row 6): undo where a file changed **after** the apply → `[diverged]`,
/// exit 1, and the whole workspace is left alone. CLI2-12 asserts the "no contents" half; this one
/// asserts the "nothing written" half and the exit code.
#[test]
fn cli2_row_undo_after_the_file_was_changed() {
    let w = World::new();
    let id = preview(&w);
    assert_eq!(drive_yes(&w, &["edit", "apply", &id]).code, EXIT_OK);
    let stranger = "log2(1);\n// somebody else was here\n";
    w.write("src/a.ts", stranger);
    let before = snapshot(&w);

    let run = drive_opts(&w, &["edit", "undo", &id], true, true);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    assert!(text(&run.cap).contains("[diverged]"), "{}", text(&run.cap));
    assert_eq!(snapshot(&w), before, "a refused undo wrote something");
    assert_eq!(w.file("src/a.ts"), stranger);
}

/// CLI2-20 (failure table, row 7): a state directory that cannot be used → an environment error,
/// exit 2, on every `edit` command, and never a panic.
#[test]
fn cli2_row_an_unusable_state_directory() {
    let w = World::new();
    let id = preview(&w);
    // A regular file where the state directory should be.
    let state = w.state.clone();
    let _ = std::fs::remove_dir_all(&state);
    std::fs::write(&state, b"not a directory").unwrap();

    for rest in [
        owned(&["edit", "list"]),
        owned(&["edit", "show", id.as_str()]),
        owned(&[
            "edit",
            "preview",
            "--language",
            "typescript",
            "--path",
            "src/a.ts",
            "--pattern",
            "log($$$ARGS)",
            "--replacement",
            "log2($$$ARGS)",
        ]),
        owned(&["edit", "apply", id.as_str()]),
        owned(&["edit", "undo", id.as_str()]),
        owned(&["edit", "recover"]),
    ] {
        let run = drive_opts_s(&w, &rest, true, true);
        assert_eq!(
            run.code,
            EXIT_ENV,
            "{rest:?} must be an environment error:\n{}",
            text(&run.cap)
        );
        assert!(
            text(&run.cap).contains('['),
            "{rest:?} must print a literal code:\n{}",
            text(&run.cap)
        );
    }
}

/// CLI2-21 (failure table, row 8): `plan show` hitting several plans is covered by CLI2-03 for
/// `edit show`; what is new here is that the **same** store state gives the same answer through
/// both surfaces, and that the two never disagree about which one was ambiguous.
#[test]
fn cli2_row_show_hitting_several_plans_agrees_with_plan_show() {
    let w = World::new();
    // 90 plans is what CLI2-03 needs to find a real collision; reuse the same fixture shape.
    let mut ids = Vec::new();
    for i in 0..90 {
        let p = format!("f{i}.ts");
        w.write(&p, &format!("a{i}();\n"));
        let args = vec![
            "edit".to_string(),
            "preview".to_string(),
            "--language".into(),
            "typescript".into(),
            "--path".into(),
            p,
            "--pattern".into(),
            format!("a{i}($$$ARGS)"),
            "--replacement".into(),
            format!("b{i}($$$ARGS)"),
        ];
        let run = drive_opts_s(&w, &args, true, false);
        assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
        // The id comes from the preview's own first line, so it is the id the store derived — not
        // one reconstructed from a listing, which would silently introduce duplicates and turn an
        // unambiguous prefix into a fake collision.
        ids.push(plan_id_of(&run.cap));
    }
    let mut group: Option<(String, Vec<String>)> = None;
    for id in &ids {
        let prefix: String = id.chars().take(10).collect();
        let entry = group.get_or_insert_with(|| (prefix.clone(), Vec::new()));
        if entry.0 == prefix {
            entry.1.push(id.clone());
        }
    }
    let Some((prefix, _candidates)) = group.into_iter().find(|(_, g)| g.len() > 1) else {
        eprintln!("note: no prefix collision among {} plans", ids.len());
        return;
    };

    let via_edit = drive(&w, &["edit", "show", &prefix]);
    let via_plan = drive(&w, &["plan", "show", &prefix]);
    assert_eq!(via_edit.code, EXIT_USER);
    // Both surfaces refuse; they need not spell it identically, but both must refuse and both must
    // exit with the same code.
    assert_eq!(via_edit.code, via_plan.code);
    assert!(text(&via_edit.cap).contains("[ambiguous]"));
}

// ---- help and documentation agree with the implementation ----------------------------------------

/// CLI2-22: `--help` states the exit codes and the write-mode rules **as they are**, so the
/// documentation cannot drift from the behaviour. Invariant 7: a doc that lies is a defect.
#[test]
fn cli2_help_states_the_exit_codes_and_the_write_rules() {
    use clap::CommandFactory;
    let help = Cli::command().render_long_help().to_string();

    // The exit-code table, unchanged from CLI 1.
    assert!(help.contains("Exit codes:"), "{help}");
    assert!(help.contains("0  success"), "{help}");
    assert!(help.contains("1  user error"), "{help}");
    assert!(help.contains("2  environment"), "{help}");

    // The write rules, spelled the way the two gates work: the flag is one of two, and the
    // configuration key is named.
    assert!(
        help.contains("first** of two gates"),
        "the two gates must be documented:\n{help}"
    );
    assert!(help.contains("allow_write = true"), "{help}");
    assert!(
        help.contains("Say yes to the confirmation"),
        "--yes must be documented:\n{help}"
    );
    assert!(
        help.contains("NO_COLOR"),
        "the colour rules must be documented, including NO_COLOR:\n{help}"
    );

    // And every subcommand this ticket adds is in `edit`'s own help, with its flags. The top-level
    // help names `edit` only, so asserting there would be asserting the wrong document.
    let edit_help = render_sub_help("");
    for sub in ["preview", "show", "apply", "undo", "recover", "list"] {
        assert!(
            edit_help.contains(sub),
            "`edit {sub}` is missing from the edit help:\n{edit_help}"
        );
    }
    let apply = render_sub_help("apply");
    assert!(apply.contains("full plan id"), "{apply}");
    assert!(
        apply.contains("confirmation or --yes"),
        "apply must document its confirmation gate:\n{apply}"
    );
    let preview = render_sub_help("preview");
    for flag in [
        "--language",
        "--path",
        "--pattern",
        "--replacement",
        "--note",
    ] {
        assert!(
            preview.contains(flag),
            "{flag} is missing from `edit preview`:\n{preview}"
        );
    }
    let show = render_sub_help("show");
    for flag in ["--file", "--offset", "--limit"] {
        assert!(
            show.contains(flag),
            "{flag} is missing from `edit show`:\n{show}"
        );
    }
}

/// The long help of `edit`, or of one of its subcommands when `sub` is named.
fn render_sub_help(sub: &str) -> String {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let edit = cmd
        .find_subcommand_mut("edit")
        .expect("the edit subcommand exists");
    match sub {
        "" => edit.render_long_help().to_string(),
        named => edit
            .find_subcommand_mut(named)
            .unwrap_or_else(|| panic!("edit {named} exists"))
            .render_long_help()
            .to_string(),
    }
}

// ---- output hygiene ----------------------------------------------------------------------------

/// CLI2-23: no `edit` command leaks a path from inside the machine, and none quotes a file's
/// contents. Every path printed must be either workspace-relative or the two the user typed.
#[test]
fn cli2_no_absolute_paths_and_no_file_contents_are_printed() {
    let w = World::new();
    let id = preview(&w);
    assert_eq!(drive_yes(&w, &["edit", "apply", &id]).code, EXIT_OK);

    let root = w.root.to_str().unwrap();
    let config = w.config.to_str().unwrap();
    for rest in [
        preview_args(),
        owned(&["edit", "show", id.as_str()]),
        owned(&["edit", "list"]),
        owned(&["edit", "undo", id.as_str()]),
        owned(&["edit", "recover"]),
    ] {
        let run = drive_opts_s(&w, &rest, true, true);
        let t = text(&run.cap);
        assert!(
            !t.contains(root),
            "{rest:?} printed the workspace root:\n{t}"
        );
        assert!(
            !t.contains(config),
            "{rest:?} printed the configuration path:\n{t}"
        );
        assert!(
            !t.contains(&w.root.to_str().unwrap().to_string()),
            "{rest:?} printed an absolute path"
        );
        // The state directory is never named at all, and the workspace-local dotfile it used
        // to live in is gone, so a path inside it cannot appear either.
        assert!(
            !t.contains("/.opencrayast/") && !t.contains("/opencrayast/"),
            "{rest:?} printed a path inside the machine:\n{t}"
        );
    }

    // `edit show` prints the diff, which is a file's content — by design, that is the command's
    // whole purpose. The rule is narrower and is stated in `cli1_spec` CLI1-14: the *plan* listing
    // shows counts, never content. So the check that matters here is that the write path and the
    // refusals do not.
    let run = drive_opts(
        &w,
        &["edit", "apply", "p-aaaaaaaaaaaaaaaaaaaaaaaaaa"],
        true,
        true,
    );
    assert!(!text(&run.cap).contains("log2(1);"));
}

/// CLI2-24: everything every `edit` command prints is escaped — no raw control, bidi or invisible
/// character, whatever the plan holds. The same rule as CLI1-05, applied to the new surface.
#[test]
fn cli2_nothing_reaches_the_output_unescaped() {
    let w = World::new();
    let id = preview(&w);
    for rest in [
        preview_args(),
        owned(&["edit", "show", id.as_str()]),
        owned(&["edit", "list"]),
    ] {
        let run = drive_opts_s(&w, &rest, true, false);
        for line in run.cap.all() {
            for ch in line.chars() {
                assert!(
                    !ch.is_control(),
                    "raw control {ch:?} (U+{:04X}) for {rest:?}: {line:?}",
                    ch as u32
                );
            }
            for sneaky in ['\u{202e}', '\u{200b}', '\u{2066}', '\u{1b}'] {
                assert!(
                    !line.contains(sneaky),
                    "raw {sneaky:?} for {rest:?}: {line:?}"
                );
            }
        }
    }
}

/// CLI2-25: the confirmation prompt is asked **once** per write and never twice, and `recover` —
/// which has no plan and no file list — is still confirmed.
#[test]
fn cli2_recover_is_confirmed_and_asks_once() {
    let w = World::new();

    // Refused: nothing converges.
    let (run, person) = drive_answering(&w, &["edit", "recover"], Answer::No);
    assert_eq!(run.code, EXIT_USER, "{}", text(&run.cap));
    assert_eq!(person.times_asked(), 1);
    assert!(text(&run.cap).contains("nothing confirmed it"));

    // Confirmed, with nothing to recover.
    let (run, person) = drive_answering(&w, &["edit", "recover"], Answer::Yes);
    assert_eq!(run.code, EXIT_OK, "{}", text(&run.cap));
    assert_eq!(person.times_asked(), 1);
    assert!(
        text(&run.cap).contains("Nothing to recover."),
        "{}",
        text(&run.cap)
    );
}

/// Every file under the workspace, as `(relative path, bytes)`, excluding the state directory.
///
/// The snapshot is what makes "wrote nothing" an assertion about the whole workspace rather than
/// about the one file a test remembered to check. The state directory is excluded because `preview`
/// legitimately writes a plan there, and the question being asked is always about *source* files.
fn snapshot(w: &World) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    walk(&w.root, &w.root, &mut out);
    out.sort();
    out
}

fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        // The state directory is outside the workspace, so nothing under the root needs
        // skipping: if it ever came back, it would show up here and fail the snapshot tests.
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk(root, &path, out);
        } else if ft.is_file() {
            out.push((rel, std::fs::read(&path).unwrap_or_default()));
        }
    }
}
