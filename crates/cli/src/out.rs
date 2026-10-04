//! One place where everything the CLI prints goes.
//!
//! Two rules, both from the ticket (invariant 5):
//!
//! - **No raw control characters.** A plan note, a path or an error message can contain ESC, NUL,
//!   bidi overrides or zero-width characters, and printing those to a terminal is how a file's
//!   contents get to lie about themselves (SECURITY-MODEL T-34). Every line is escaped with the L0
//!   [`escape_inline`], so a note that contains `\u{1b}[31m` prints as those seven characters.
//! - **Paths are shown the way the user typed them**, never as an absolute path from inside the
//!   machine.
//!
//! # Where the escaping happens, and why it is here
//!
//! **In [`Out::line`] and [`Out::diag`]**, which is the single funnel every command writes through.
//! It is NOT in a sink implementation. The first version of this module escaped inside
//! `impl Sink for Capture` — the TEST DOUBLE — while `impl Sink for Stdout`, the one that ships,
//! called `println!` on the raw text. The shipping binary therefore had no output sanitisation at
//! all, while this module's comment claimed the opposite: a test asserting "the output is escaped"
//! passed, and a real `opencrayast doctor` printed raw ESC bytes. That is the shape of bug a test
//! double can hide, so the rule now lives above the sinks and [`Sink`] receives text that is
//! already escaped.
//!
//! [`Stdout`] writes through a `Write` rather than through `println!`, so a test can hand it a
//! buffer and assert on the exact BYTES the shipping sink produces. [`cli1_spec`] does exactly that,
//! and also runs the real binary.
//!
//! # Why colour lives below the escaping funnel
//!
//! [`Out::coloured`] escapes exactly as [`Out::line`] does and only then asks the sink to wrap the
//! result in SGR bytes. Two consequences, both required by `docs/TOOLS.md` §Output sanitising
//! ("colour comes only from the tool, never from file content"):
//!
//! - An ESC inside a file name is escaped to the seven characters `\u{1b}` **before** anything can
//!   be painted, so file content cannot introduce a terminal sequence of its own.
//! - The colour is a property of the sink and the palette, not of the text. A sink that ignores the
//!   SGR argument still gets readable plain text, which is what makes `--color never`, `NO_COLOR`
//!   and a redirected stdout identical in meaning to a terminal that renders them.
//!
//! The default [`Sink::line_with`] drops the colour and calls [`Sink::line`], so every existing
//! implementation keeps compiling and keeps its meaning.

use opencrayast_core::render::escape_inline;
use std::io::Write;

/// A sink for CLI output.
///
/// Implementations receive text that has **already been escaped** by [`Out`]; they must write it
/// verbatim. A sink that escapes again would double-escape, so do not add that here.
pub trait Sink {
    /// A line of ordinary output (to stdout).
    fn line(&mut self, text: &str);
    /// A line of diagnostics (to stderr).
    fn diag(&mut self, text: &str);

    /// A line of ordinary output in the SGR colour named by `sgr`.
    ///
    /// `sgr` is `None` whenever colour is off for this run — `--color never`, `NO_COLOR`, a
    /// terminal that cannot render it — and then this is exactly [`Sink::line`]. The default
    /// implementation ignores it, which is the right behaviour for a sink that has no colour of its
    /// own; the shipping sinks implement it.
    fn line_with(&mut self, text: &str, sgr: Option<&'static str>) {
        let _ = sgr;
        self.line(text);
    }
}

/// The bytes that make one colour: ESC `[`, the SGR parameters, `m`.
///
/// A function so the three shipping sinks cannot drift on the byte sequence — an off-by-one here
/// would produce output that renders as literal escape text on some terminals and as colour on
/// others, and that difference is invisible in a `contains` assertion.
fn sgr_open(sgr: &str) -> String {
    format!("\u{1b}[{sgr}m")
}

/// The byte sequence that ends any colour.
const SGR_RESET: &str = "\u{1b}[0m";

/// Wrap `text` in `sgr`, or return it unchanged when `sgr` is `None`.
fn painted(text: &str, sgr: Option<&'static str>) -> String {
    match sgr {
        Some(sgr) => format!("{}{}{}", sgr_open(sgr), text, SGR_RESET),
        None => text.to_string(),
    }
}

/// Write to the real process streams, or to any `Write` a test provides.
///
/// Holding the streams as `Write` rather than calling `println!` is what lets a test see the exact
/// bytes the shipping sink emits — which is the only way to catch "the real sink does not do what
/// the module claims".
pub struct Streams<W: Write, E: Write> {
    /// Where ordinary output goes.
    pub out: W,
    /// Where diagnostics go.
    pub err: E,
}

impl<W: Write, E: Write> Streams<W, E> {
    /// Ordinary output to `out`, diagnostics to `err`.
    pub fn new(out: W, err: E) -> Streams<W, E> {
        Streams { out, err }
    }
}

/// The shipping sink: the process's own stdout and stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stdout;

impl Sink for Stdout {
    fn line(&mut self, text: &str) {
        // Escaped by `Out` before it reaches here; write the bytes as they are.
        let mut handle = std::io::stdout().lock();
        let _ = writeln!(handle, "{text}");
    }

    fn line_with(&mut self, text: &str, sgr: Option<&'static str>) {
        let mut handle = std::io::stdout().lock();
        let _ = writeln!(handle, "{}", painted(text, sgr));
    }

    fn diag(&mut self, text: &str) {
        let mut handle = std::io::stderr().lock();
        let _ = writeln!(handle, "{text}");
    }
}

impl<W: Write, E: Write> Sink for Streams<W, E> {
    fn line(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }

    fn line_with(&mut self, text: &str, sgr: Option<&'static str>) {
        let _ = writeln!(self.out, "{}", painted(text, sgr));
    }

    fn diag(&mut self, text: &str) {
        let _ = writeln!(self.err, "{text}");
    }
}

/// Collect output in memory, for tests that want lines rather than bytes.
#[derive(Debug, Default, Clone)]
pub struct Capture {
    /// Ordinary lines.
    pub lines: Vec<String>,
    /// Diagnostic lines.
    pub diags: Vec<String>,
}

impl Sink for Capture {
    fn line(&mut self, text: &str) {
        self.lines.push(text.to_string());
    }

    /// Records the **painted** bytes, so a golden test compares what a terminal would receive
    /// rather than a stripped-down stand-in for it. With `sgr: None` this is identical to
    /// [`Sink::line`], which is what keeps every colour-off assertion in `cli1_spec` valid.
    fn line_with(&mut self, text: &str, sgr: Option<&'static str>) {
        self.lines.push(painted(text, sgr));
    }

    fn diag(&mut self, text: &str) {
        self.diags.push(text.to_string());
    }
}

impl Capture {
    /// Everything printed, ordinary lines first.
    pub fn all(&self) -> Vec<String> {
        let mut v = self.lines.clone();
        v.extend(self.diags.iter().cloned());
        v
    }
}

/// The single funnel every command writes through.
pub struct Out<'a> {
    sink: &'a mut dyn Sink,
}

impl<'a> Out<'a> {
    /// Wrap a sink.
    pub fn new(sink: &'a mut dyn Sink) -> Out<'a> {
        Out { sink }
    }

    /// An ordinary line. **Escaped here**, which is the only place escaping happens.
    pub fn line(&mut self, text: &str) {
        self.sink.line(&escape_line(text));
    }

    /// A diagnostic line. **Escaped here**, like [`Out::line`].
    pub fn diag(&mut self, text: &str) {
        self.sink.diag(&escape_line(text));
    }

    /// An ordinary line in the SGR colour `palette` assigns to `colour`.
    ///
    /// **Escaped here**, before the colour is applied — the order is the whole point: the colour
    /// comes from the tool, and the text it wraps has already been made inert.
    ///
    /// With colour off, or for a role that has no colour of its own ([`Colour::Plain`]), this is
    /// [`Out::line`] and nothing changes about the output.
    pub fn coloured(&mut self, text: &str, colour: Colour, palette: Palette) {
        let escaped = escape_line(text);
        match palette.sgr(colour) {
            Some(sgr) => self.sink.line_with(&escaped, Some(sgr)),
            None => self.sink.line(&escaped),
        }
    }
}

/// Escape a whole line: every control, bidi and invisible character becomes a visible `\u{..}`.
/// Newlines and tabs are escaped too, because a "line" that contains one is not a line.
///
/// Idempotent for text that has no such characters, and safe to apply twice: the output of
/// `escape_inline` contains only ASCII, so escaping it again is a no-op. Call sites that already
/// escaped do not double-escape.
pub fn escape_line(text: &str) -> String {
    escape_inline(text).0
}

/// Re-exported so `lib.rs` does not have to name two modules for one concept, and so a reader of
/// [`Out::coloured`] sees where the colour decision actually comes from.
pub use crate::palette::{Colour, Palette};
