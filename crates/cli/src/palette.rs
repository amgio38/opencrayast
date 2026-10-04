//! Colour policy for the human-readable output.
//!
//! Two rules, and they are why this is a type rather than a `bool` sprinkled through the
//! commands:
//!
//! - **Colour comes from this module, never from file content.** Every byte of caller- or
//!   file-supplied text is escaped by [`Out`](crate::out::Out) *before* it reaches a sink, so the
//!   only escape sequences that can ever be written are the ones written here. That is the same
//!   rule `docs/TOOLS.md` §Output sanitising states for the CLI ("colour comes only from the
//!   tool, never from file content"), and it is why the SGR bytes live in the **sink** rather
//!   than being concatenated into the text: the sink owns the terminal, and the escaping funnel
//!   in [`Out`](crate::out::Out) stays single-purpose.
//! - **Colour is off unless it was asked for.** [`Palette::from_env`] honours `NO_COLOR`
//!   (any value, including empty — the no-color.org rule), treats `TERM=dumb` as a terminal that
//!   cannot render SGR, and requires a real terminal otherwise. A `--color always` override
//!   exists so a person piping to `less -R` is not stuck with a plain page.
//!
//! # Determinism in tests
//!
//! Nothing here reads the environment or asks whether the process has a terminal *unless* the
//! caller asks it to: [`Palette::from_env`] takes the environment lookup and the tty answer as
//! arguments. A test therefore never depends on where it is run from, and a golden comparison of
//! the same invocation under two palettes is a comparison of *two explicit values*, not of two
//! environments.

use clap::ValueEnum;
use std::fmt;

/// What `--color` asked for.
///
/// A `ValueEnum` so clap prints the three legal values in `--help` and refuses a fourth one as an
/// unknown argument — a hand-rolled `FromStr` would have had to re-implement that and could
/// disagree with the help text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Colour when the environment says a terminal is there and has not forbidden it.
    Auto,
    /// Colour even when redirected or `NO_COLOR` is set.
    Always,
    /// Never colour, whatever the environment says.
    Never,
}

/// Whether this run writes SGR sequences, and what `--color` asked for.
///
/// The request is kept alongside the answer so a diagnostic can say which of `auto`, `always` or
/// `never` produced plain output — "why is this not coloured?" is otherwise unanswerable from the
/// output alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    choice: ColorChoice,
    enabled: bool,
}

impl Palette {
    /// The palette a caller built by hand. The starting point for [`Palette::from_env`] and the
    /// value every test uses, so no test inherits the environment it happens to run in.
    pub const fn new(enabled: bool) -> Palette {
        Palette {
            choice: ColorChoice::Auto,
            enabled,
        }
    }

    /// Decide from an injected environment and tty answer.
    ///
    /// `get` is called for `NO_COLOR` and `TERM` only; `stdout_is_tty` is the caller's answer to
    /// "is stdout a terminal", so a test passes `false` and gets plain output with no dependence
    /// on where the test binary runs.
    pub fn from_env(
        choice: ColorChoice,
        stdout_is_tty: bool,
        get: impl Fn(&str) -> Option<String>,
    ) -> Palette {
        let enabled = match choice {
            ColorChoice::Never => false,
            ColorChoice::Always => true,
            ColorChoice::Auto => {
                // NO_COLOR, any value including empty (the no-color.org rule).
                get("NO_COLOR").is_none()
                    // A terminal that says `dumb` cannot render SGR, and sending it anyway would
                    // put literal escape text on the page rather than colour.
                    && get("TERM").is_none_or(|t| t != "dumb")
                    && stdout_is_tty
            }
        };
        Palette { choice, enabled }
    }

    /// The palette for this process, from the real environment and the real terminal.
    pub fn detect(choice: ColorChoice) -> Palette {
        use std::io::IsTerminal;
        Palette::from_env(choice, std::io::stdout().is_terminal(), |k| {
            std::env::var(k).ok()
        })
    }

    /// Are SGR sequences written at all?
    pub fn enabled(self) -> bool {
        self.enabled
    }

    /// What `--color` asked for, whether or not the environment then overrode it.
    pub fn choice(self) -> ColorChoice {
        self.choice
    }

    /// The prefix for `colour`, or `None` when colour is off or the colour is [`Colour::Plain`].
    ///
    /// `reset` is appended by the sink, not here, so a sink that cannot colour (the test double)
    /// still receives the same text.
    pub fn sgr(self, colour: Colour) -> Option<&'static str> {
        if self.enabled { colour.sgr() } else { None }
    }
}

/// The kinds of line this CLI paints.
///
/// Deliberately few, and deliberately not a full theme: every variant names a **role**, and the
/// mapping from role to SGR code lives in one place so a person cannot end up with a header in
/// the colour of a removed line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// Ordinary output. Never painted.
    Plain,
    /// A `--- a/` or `+++ b/` diff header.
    FileHeader,
    /// An `@@` hunk header.
    HunkHeader,
    /// A `-` line of a diff.
    Removed,
    /// A `+` line of a diff.
    Added,
    /// A line about what will happen to the workspace.
    Action,
}

impl Colour {
    /// The SGR parameters for this role, without the wrapping ESC `[` / `m`.
    pub fn sgr(self) -> Option<&'static str> {
        match self {
            Colour::Plain => None,
            Colour::FileHeader => Some("1;4"),
            Colour::HunkHeader => Some("36"),
            Colour::Removed => Some("31"),
            Colour::Added => Some("32"),
            Colour::Action => Some("1"),
        }
    }

    /// The role of one line of a rendered diff, or [`Colour::Plain`] for anything else.
    ///
    /// The `---` / `+++` headers are checked **before** the single-character prefixes, because a
    /// `---` line is both a header and, naively, a removed line. Order matters here and there is a
    /// test for it.
    pub fn for_diff_line(line: &str) -> Colour {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            Colour::FileHeader
        } else if line.starts_with("@@") {
            Colour::HunkHeader
        } else if line.starts_with('-') {
            Colour::Removed
        } else if line.starts_with('+') {
            Colour::Added
        } else {
            Colour::Plain
        }
    }
}

/// A short, printable name for each role, for `--help` and for diagnostics.
impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Colour::Plain => "plain",
            Colour::FileHeader => "file header",
            Colour::HunkHeader => "hunk header",
            Colour::Removed => "removed",
            Colour::Added => "added",
            Colour::Action => "action",
        };
        f.write_str(name)
    }
}
