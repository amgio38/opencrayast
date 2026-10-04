//! Output sanitising and fencing (docs/TOOLS.md "Output sanitising"; SECURITY-MODEL S-9, T-34;
//! tests OUT-01, OUT-04, OUT-05).
//!
//! Source files, file names, symbol names and plan notes are attacker-controlled bytes. Every
//! rendering of them for an agent or a person goes through this module.
//!
//! # Escaped character catalogue (T-34)
//!
//! | Class | Code points | Why |
//! |---|---|---|
//! | Control | C0 except `\n`/`\t`; DEL; C1 `U+0080`–`U+009F`; `U+2028`/`U+2029` | ANSI/OSC, CR overwrite, line-break spoofing |
//! | Bidi | `U+202A`–`U+202E`, `U+2066`–`U+206F` (LRI/RLI/FSI/PDI plus the isolate controls), `U+200E`/`U+200F`, `U+061C` | Trojan Source / directional overrides |
//! | Invisible | `U+200B`–`U+200D`, `U+2060`–`U+2064`, `U+2065`, `U+00AD`, `U+FEFF`, `U+FFF9`–`U+FFFB`; Tags `U+E0000`–`U+E007F`; VS Supp. `U+E0100`–`U+E01EF`; `U+034F`, `U+180E`, `U+115F`, `U+1160`, `U+17B4`, `U+17B5`, `U+3164`, `U+FFA0`, `U+202F`, `U+205F`, `U+0600`–`U+0605`, `U+06DD`, `U+070F`, `U+0890`–`U+0891`, `U+08E2`, `U+180B`–`U+180D`, `U+110BD`, `U+110CD`, `U+2800` | Zero-width / fillers / format controls; **Tags ASCII smuggling** (invisible prompt injection) |
//!
//! The Bidi and Invisible classes are the *full* Unicode format/isolate/bidi-control set, not the
//! subset a Trojan-Source advisory names. An earlier version of this table listed `U+2066`–`U+2069`
//! and `U+2060`–`U+2064` while [`classify`] implemented those same truncated bounds, so the isolate
//! controls `U+206A`–`U+206F`, `U+2065`, and the Arabic/Syriac/Kaithi prepend marks and spaces
//! passed through both renderers verbatim. A name carrying an isolate renders as nothing and
//! reverses the direction of everything after it - which is the same Trojan-Source outcome the
//! table above claims to prevent. `render::ESCAPE_CATALOGUE` is the machine-readable form of this
//! table, and [`catalogue_is_exhaustive`] fails if the two ever disagree again.
//!
//! **Not escaped:** BMP variation selectors `U+FE00`–`U+FE0F` (emoji presentation; escaping would break normal content).

/// Counts of the characters that were escaped, for the risk summary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscapeCounts {
    /// C0/C1 control characters (including ESC) and `\r`, DEL; also U+2028/U+2029.
    pub control: usize,
    /// Bidirectional controls: U+202A-202E, U+2066-2069, U+200E/U+200F, U+061C.
    pub bidi: usize,
    /// Invisible / smuggling: ZW*, Tags U+E0000-E007F, VS Supp. U+E0100-E01EF, fillers (see module docs).
    pub invisible: usize,
}

impl EscapeCounts {
    /// Total number of escaped characters.
    pub fn total(&self) -> usize {
        self.control + self.bidi + self.invisible
    }
}

/// Which bucket a character belongs to when it must be escaped.
///
/// Public because [`ESCAPE_CATALOGUE`] is public and names this in each of its ranges, so the
/// fuzz target can assert a per-class count rather than only a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeClass {
    /// C0/C1 control characters, DEL, `\r`, `U+2028`/`U+2029`.
    Control,
    /// Bidirectional controls and isolates.
    Bidi,
    /// Invisible, zero-width and filler characters.
    Invisible,
}

/// Classify a character that is neither plain text nor (in text mode) a kept `\n`/`\t`.
fn classify(ch: char) -> Option<EscapeClass> {
    match ch {
        // C0 (excluding \n/\t which callers handle), DEL, C1.
        '\u{00}'..='\u{08}'
        | '\u{0b}'..='\u{1f}'
        | '\u{7f}'..='\u{9f}'
        // Line/paragraph separators: not Unicode Cc, but they break line layout the same way
        // control characters do (SECURITY-MODEL T-34 display deception). Treat as control.
        | '\u{2028}'
        | '\u{2029}' => Some(EscapeClass::Control),
        '\u{202a}'..='\u{202e}'
        // U+2066-U+206F is the whole isolate/format block: LRI, RLI, FSI, PDI and the five
        // deprecated-but-still-interpreted isolate controls (LRI..PDI stop at U+2069; U+206A-U+206F
        // are INHIBIT SYMMETRIC SWAPPING, INHIBIT ARABIC FORM SHAPING, NOMINAL DIGIT SHAPES,
        // NOMINAL BRACKET DIGITS and, at U+206F, the isolate initiator). Truncating this range at
        // U+2069 let an isolate run straight through to the terminal.
        | '\u{2066}'..='\u{206f}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{061c}' => Some(EscapeClass::Bidi),
        '\u{200b}'..='\u{200d}'
        // U+2060-U+2064 are the word-joiner family; U+2065 is the reserved code point directly
        // above it and renders as nothing wherever a terminal honours it.
        | '\u{2060}'..='\u{2065}'
        | '\u{00ad}'
        | '\u{feff}'
        // Interlinear annotation anchor/separator/terminator: format controls that reposition text.
        | '\u{fff9}'..='\u{fffb}'
        // Arabic number sign / thousands separator / number marks / end-of-ayah, Syriac abbreviation
        // mark, the Arabic prepend marks above U+0800, and the Kaithi number signs. All Cf with a
        // directional effect; all of them reorder a line while looking like ordinary script.
        | '\u{0600}'..='\u{0605}'
        | '\u{06dd}'
        | '\u{070f}'
        | '\u{0890}'..='\u{0891}'
        | '\u{08e2}'
        | '\u{110bd}'
        | '\u{110cd}'
        // Mongolian free variation selectors: invisible glyph substitution.
        | '\u{180b}'..='\u{180d}'
        // Braille pattern blank: renders as nothing in a proportional font but reads as content to
        // a screen reader or to a diff of the source.
        | '\u{2800}'
        // Spaces that are not a space: both carry a directional class (CS / WS) and both render at
        // the width of nothing on a proportional terminal.
        | '\u{202f}'
        | '\u{205f}'
        // Tags block: each code point mirrors an ASCII byte but is invisible (ASCII smuggling).
        | '\u{e0000}'..='\u{e007f}'
        // Variation Selectors Supplement (data hiding); BMP U+FE00–FE0F are NOT escaped.
        | '\u{e0100}'..='\u{e01ef}'
        | '\u{034f}'
        | '\u{180e}'
        | '\u{115f}'
        | '\u{1160}'
        | '\u{17b4}'
        | '\u{17b5}'
        | '\u{3164}'
        | '\u{ffa0}' => Some(EscapeClass::Invisible),
        _ => None,
    }
}

fn push_unicode_escape(out: &mut String, ch: char) {
    // Lowercase hex, no leading zeros (e.g. `\u{d}`, `\u{1b}`, `\u{202e}`).
    use std::fmt::Write as _;
    let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
}

fn bump(counts: &mut EscapeCounts, class: EscapeClass) {
    match class {
        EscapeClass::Control => counts.control += 1,
        EscapeClass::Bidi => counts.bidi += 1,
        EscapeClass::Invisible => counts.invisible += 1,
    }
}

fn escape_impl(input: &str, inline: bool) -> (String, EscapeCounts) {
    // Single pass, linear in input length (and in number of escapes for output growth).
    let mut out = String::with_capacity(input.len());
    let mut counts = EscapeCounts::default();
    for ch in input.chars() {
        if inline && ch == '\n' {
            out.push_str("\\n");
            counts.control += 1;
            continue;
        }
        if inline && ch == '\t' {
            out.push_str("\\t");
            counts.control += 1;
            continue;
        }
        if !inline && (ch == '\n' || ch == '\t') {
            out.push(ch);
            continue;
        }
        match classify(ch) {
            Some(class) => {
                push_unicode_escape(&mut out, ch);
                bump(&mut counts, class);
            }
            None => out.push(ch),
        }
    }
    (out, counts)
}

/// One documented range of the escaped character catalogue, with the bucket it lands in.
///
/// The catalogue is data rather than prose so that the escape table in the module docs, the fuzz
/// target and the spec tests cannot drift apart: every range here is classified by [`classify`],
/// and [`catalogue_is_exhaustive`] checks that the two agree in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeRange {
    /// First code point of the inclusive range.
    pub first: u32,
    /// Last code point of the inclusive range.
    pub last: u32,
    /// The bucket a character in this range is counted under.
    pub class: EscapeClass,
}

/// Every range of the documented escaped class, in the order the module table lists them.
///
/// `\n` and `\t` are deliberately absent: whether they are escaped depends on the renderer
/// ([`escape_inline`] versus [`escape_text`]), so they are not a fixed property of the class.
/// The readers are the fuzz target, which enumerates this instead of hand-picking characters, and
/// `render_spec`, which pins each range to its expected bucket.
pub const ESCAPE_CATALOGUE: &[EscapeRange] = &[
    // Control
    EscapeRange {
        first: 0x00,
        last: 0x08,
        class: EscapeClass::Control,
    },
    EscapeRange {
        first: 0x0b,
        last: 0x1f,
        class: EscapeClass::Control,
    },
    EscapeRange {
        first: 0x7f,
        last: 0x9f,
        class: EscapeClass::Control,
    },
    EscapeRange {
        first: 0x2028,
        last: 0x2029,
        class: EscapeClass::Control,
    },
    // Bidi: directional overrides, the full isolate block, and the explicit marks.
    EscapeRange {
        first: 0x202a,
        last: 0x202e,
        class: EscapeClass::Bidi,
    },
    EscapeRange {
        first: 0x2066,
        last: 0x206f,
        class: EscapeClass::Bidi,
    },
    EscapeRange {
        first: 0x200e,
        last: 0x200e,
        class: EscapeClass::Bidi,
    },
    EscapeRange {
        first: 0x200f,
        last: 0x200f,
        class: EscapeClass::Bidi,
    },
    EscapeRange {
        first: 0x061c,
        last: 0x061c,
        class: EscapeClass::Bidi,
    },
    // Invisible: zero-width and word-joiner family, plus the format controls and fillers.
    EscapeRange {
        first: 0x200b,
        last: 0x200d,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x2060,
        last: 0x2065,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x00ad,
        last: 0x00ad,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0xfeff,
        last: 0xfeff,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0xfff9,
        last: 0xfffb,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x0600,
        last: 0x0605,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x06dd,
        last: 0x06dd,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x070f,
        last: 0x070f,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x0890,
        last: 0x0891,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x08e2,
        last: 0x08e2,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x180b,
        last: 0x180d,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x110bd,
        last: 0x110bd,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x110cd,
        last: 0x110cd,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x2800,
        last: 0x2800,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x202f,
        last: 0x202f,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x205f,
        last: 0x205f,
        class: EscapeClass::Invisible,
    },
    // Tags block: invisible ASCII smuggling.
    EscapeRange {
        first: 0xe0000,
        last: 0xe007f,
        class: EscapeClass::Invisible,
    },
    // Variation Selectors Supplement. BMP U+FE00-FE0F are deliberately NOT here.
    EscapeRange {
        first: 0xe0100,
        last: 0xe01ef,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x034f,
        last: 0x034f,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x180e,
        last: 0x180e,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x115f,
        last: 0x115f,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x1160,
        last: 0x1160,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x17b4,
        last: 0x17b4,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x17b5,
        last: 0x17b5,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0x3164,
        last: 0x3164,
        class: EscapeClass::Invisible,
    },
    EscapeRange {
        first: 0xffa0,
        last: 0xffa0,
        class: EscapeClass::Invisible,
    },
];

/// True when [`classify`] and [`ESCAPE_CATALOGUE`] classify exactly the same code points.
///
/// The catalogue is the documented class; `classify` is what actually runs. This is the check that
/// makes a truncated range in the `match` - the defect that let the isolate controls through -
/// fail the suite instead of quietly narrowing the guarantee.
pub fn catalogue_is_exhaustive() -> bool {
    (0u32..=0x10FFFF)
        .filter_map(char::from_u32)
        .filter(|ch| *ch != '\n' && *ch != '\t')
        .all(|ch| {
            let by_classify = classify(ch);
            let by_catalogue = ESCAPE_CATALOGUE
                .iter()
                .find(|r| (r.first..=r.last).contains(&(ch as u32)))
                .map(|r| r.class);
            by_classify == by_catalogue
        })
}

/// True when `ch` is in the documented escaped class. Convenient for callers that only want the
/// yes/no answer (the fuzz target's reachability check, for one).
pub fn is_escaped(ch: char) -> bool {
    ch != '\n' && ch != '\t' && classify(ch).is_some()
}

/// Escape control, bidi and invisible characters as visible `\u{..}` escapes.
///
/// Rules (exact, tested):
/// - `\n` and `\t` are KEPT as they are in multi-line text (use [`escape_inline`] for a single line).
/// - every other control character (including `\r`, ESC U+001B, NUL, DEL, U+0080-009F) becomes
///   `\u{1b}`-style: backslash, `u`, `{`, lowercase hex without leading zeros, `}`.
/// - bidi and invisible characters (see [`EscapeCounts`] and the module catalogue) become the same
///   escape form.
/// - a literal backslash is NOT doubled (output is for display, not for parsing back).
/// - everything else, including all other non-ASCII and BMP variation selectors U+FE00–FE0F, is
///   unchanged.
pub fn escape_text(input: &str) -> (String, EscapeCounts) {
    escape_impl(input, false)
}

/// Like [`escape_text`] but also escapes `\n` (as `\n`) and `\t` (as `\t`): for names, paths and
/// notes that must stay on one line.
pub fn escape_inline(input: &str) -> (String, EscapeCounts) {
    escape_impl(input, true)
}

/// Keep only characters allowed in a Markdown fence info string: `[A-Za-z0-9_+-]`.
fn sanitize_info(info: &str) -> String {
    info.chars()
        .filter(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '+' | '-'))
        .collect()
}

/// Longest run of consecutive backticks in `s` (0 if none).
fn max_backtick_run(s: &str) -> usize {
    let mut max = 0usize;
    let mut cur = 0usize;
    for ch in s.chars() {
        if ch == '`' {
            cur += 1;
            if cur > max {
                max = cur;
            }
        } else {
            cur = 0;
        }
    }
    max
}

/// Wrap `code` in a Markdown code fence whose fence is longer than any run of backticks inside
/// the code (minimum three), with `info` (a language id, may be empty) after the opening fence.
/// `code` is passed through [`escape_text`] first. The result always ends with a newline after the
/// closing fence. Output never places code on a line that does not belong to the fence.
pub fn fenced_block(code: &str, info: &str) -> (String, EscapeCounts) {
    let (escaped, counts) = escape_text(code);
    let fence_len = max_backtick_run(&escaped).saturating_add(1).max(3);
    let fence: String = "`".repeat(fence_len);
    let info_clean = sanitize_info(info);

    // Opening: <fence><info>\n  (info omitted when empty after sanitising)
    // Body: escaped code
    // Closing: \n<fence>\n
    let mut out = String::with_capacity(escaped.len() + fence_len * 2 + info_clean.len() + 4);
    out.push_str(&fence);
    out.push_str(&info_clean);
    out.push('\n');
    out.push_str(&escaped);
    out.push('\n');
    out.push_str(&fence);
    out.push('\n');
    (out, counts)
}
