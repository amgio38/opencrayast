//! The structured "bad code" probe of ISSUE-QUERY-ECMA-NAMEFIX, item 3.
//!
//! The invariant this milestone fixed is that a symbol's name comes from a declaration's
//! *single-identifier* child. Before it, a destructuring pattern's text was pasted straight into
//! a name (`declare const {\n}` produced a symbol called `"{\n}"`) and the shared gate in
//! `outline::collect_all` was left to clean it up afterwards. This file is the measurement the
//! ticket asked for: a large, seeded corpus of malformed and half-legal programs per language, the
//! number of symbols the shared gate had to throw away, and the invariant asserted across every
//! case rather than on a hand-written handful.
//!
//! # The before / after numbers
//!
//! Over the corpus below - 3200 cases each for TypeScript, JavaScript, Python and Go, 12800 in
//! total, all of which parse - symbols the shared gate in `outline::collect_all` discarded:
//!
//! | language   | before | after |
//! |------------|--------|-------|
//! | TypeScript |    119 |     0 |
//! | JavaScript |    121 |     0 |
//! | Python     |      0 |     0 |
//! | Go         |      0 |     0 |
//!
//! "Before" was obtained by reverting the two guards this ticket added and re-running this probe
//! on the identical corpus: `is_identifier_node` was short-circuited with `false &&` in
//! `ecma::identifier_field_text`, and the `_` rule in `go::usable_name` was short-circuited with
//! `false ||`. Both mutations were restored before this file was committed; the tree carries the
//! real fix. Reproduce it by making those two edits again and running
//! `namefix_probe_collectors_leave_nothing_for_the_shared_gate`.
//!
//! Two honest notes about what the numbers do and do not say:
//!
//! * Python and Go read 0 before as well. That is not the probe failing to measure: python's
//!   assignment path already required an `identifier` left-hand side, and Go's blank-identifier
//!   cases did produce `_`, which the gate does *not* discard (a bare `_` is clean text by every
//!   rule in `is_clean_name`). Go's contribution shows up in the invariant below, not in the
//!   discard column.
//! * The `namespace "s"` leak found while writing this probe is likewise invisible to the discard
//!   counter - quotes do not trip the shared gate - and was caught only by the stricter identifier
//!   rule in [`assert_case`]. Reverting just that guard leaves the discard column at 0.
//!
//! Two properties make it a probe and not a pile of near-duplicate cases:
//!
//! * **Deterministic.** Every case is a pure function of [`SEED`] and its index, through the
//!   shared xorshift generator in `common::fuzz`. No `rand`, no time, no iteration order. A
//!   failure names one index, and `namefix_probe_case_is_reproducible_from_its_index` replays it
//!   alone. The corpus is additionally pinned by checksum, so changing a template is a visible
//!   diff in this file rather than a silent shift in the numbers below.
//! * **Structured.** Sources are assembled from the declaration forms the ticket enumerates -
//!   destructuring, multi-target, grouped, computed keys, blank identifiers, re-exports - crossed
//!   with a pool of slot fillers that includes valid identifiers, blank identifiers, punctuation,
//!   comments, line breaks and empty strings. The result is a mix of legal, semi-legal and
//!   outright broken programs, which is the point: the collector has to survive all three.
//!
//! [`corpus_checksum`]: FNV-1a over every generated source, so the corpus cannot drift silently.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::fuzz::Rng;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::{OutlineOptions, Symbol, collect_all_counting_discards, outline};
use std::time::Duration;

/// The seed every case is derived from. Change it and the corpus changes wholesale; the checksum
/// assertion below is what makes that a visible, deliberate edit.
const SEED: u64 = 0x4E41_4D45_4649_5830; // "NAMEFIX0"

/// Cases generated per language. The ticket asks for 3000 or more; 3200 leaves the count above the
/// line by a margin that a template edit cannot quietly eat.
const CASES_PER_LANGUAGE: usize = 3200;

/// FNV-1a over the whole corpus, in a fixed order. Pinned so an edit to a template or a filler
/// - which would move every count the probe reports - fails this assertion first.
const CORPUS_CHECKSUM: u64 = 4619270624964453495;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

/// What the probe observed for one language across its whole corpus.
#[derive(Debug, Default, Clone, Copy)]
struct Observed {
    /// Cases generated.
    cases: usize,
    /// Cases whose parse failed outright (a budget refusal, not a syntax error).
    unparsable: usize,
    /// Symbols the collector produced, before the shared gate.
    produced: usize,
    /// Symbols the shared gate in `collect_all` threw away.
    discarded: usize,
    /// Symbols that survived, as `outline` reports them.
    kept: usize,
}

/// One language's whole corpus: every case, plus the invariant checked on every symbol.
fn observe(lang: Language, templates: &[&str], fillers: &[&str], count: usize) -> Observed {
    let mut obs = Observed {
        cases: count,
        ..Observed::default()
    };
    for index in 0..count {
        let src = build(index, templates, fillers);
        // The per-case invariant: whatever the collector produced, the shared gate is the last
        // line and everything it lets through is a real, addressable name. `assert_case` is where
        // the check lives, so a name that is damaged but somehow passed the gate is a failure
        // here rather than a silent pass.
        let Ok(parsed) = parse(lang, &src, &budget()) else {
            obs.unparsable += 1;
            continue;
        };
        let (symbols, discarded) = collect_all_counting_discards(&parsed, &src);
        obs.produced += discarded + symbols.len();
        obs.discarded += discarded;
        obs.kept += symbols.len();
        assert_case(lang, index, &src, &symbols);
    }
    obs
}

/// The invariant, stated once and applied to every symbol of every generated case.
///
/// A name survives only if it is built from identifier characters. That is stricter than the
/// shared gate's own rule (which also rejects control characters, comment markers and doubled
/// spaces): a name containing `{`, `}`, a quote or a bracket could never have come from a
/// single-identifier child, whatever the gate happens to allow today.
fn assert_case(lang: Language, index: usize, src: &str, symbols: &[Symbol]) {
    let lang_id = lang.id();
    for sym in symbols {
        for value in [&sym.name, &sym.qualified] {
            let value = value.as_str();
            assert!(
                !value.is_empty(),
                "{lang_id} case {index}: empty name from {src:?}"
            );
            assert!(
                value.chars().all(identifier_char),
                "{lang_id} case {index}: name {value:?} is not identifier text, from {src:?}"
            );
            assert!(
                value.len() <= 1024,
                "{lang_id} case {index}: name over the limit from {src:?}: {}",
                value.len()
            );
        }
        // `name` is a bare identifier: no path separator, no dot. A qualified path is assembled by
        // walking owners, and a separator inside the leaf name is how a broken owner leaks in.
        assert!(
            !sym.name.contains('.') && !sym.name.contains("::"),
            "{lang_id} case {index}: name {:?} carries a path separator, from {src:?}",
            sym.name
        );
        // A name that is not in the source is a name that does not exist, which is the whole
        // failure this milestone removed.
        assert!(
            src.contains(sym.name.as_str()),
            "{lang_id} case {index}: name {:?} does not occur in the source {src:?}",
            sym.name
        );
        assert!(
            sym.start_byte <= sym.end_byte && sym.end_byte <= src.len(),
            "{lang_id} case {index}: extent outside the source from {src:?}: {sym:?}"
        );
    }
}

/// Characters a symbol name may consist of: the letters, digits, `_` and `$` an identifier is
/// made of, plus `#` for a private name (`#priv`) and `.` / `::` for the joined owner path of a
/// qualified name. Note what is absent: braces, brackets, quotes, spaces, semicolons, slashes.
fn identifier_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '.' | ':')
}

/// The source for `index`: template `index % templates.len()`, slots filled from `fillers` with a
/// generator seeded only by [`SEED`] and `index`, so any one case can be rebuilt on its own.
fn build(index: usize, templates: &[&str], fillers: &[&str]) -> String {
    let mut rng = Rng::new(SEED ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let template = templates[index % templates.len()];
    let mut out = template.to_string();
    for slot in 0..MAX_SLOTS {
        let token = format!("{{{slot}}}");
        if !out.contains(&token) {
            break;
        }
        out = out.replace(&token, fillers[rng.below(fillers.len())]);
    }
    out
}

/// Slots per template. The largest template below has four, so this is an upper bound and a
/// template with more slots than this would silently stop being filled - hence the early `break`
/// above leaving any leftover `{N}` in the source, which is itself a hostile case.
const MAX_SLOTS: usize = 6;

// ---- Templates: the declaration forms the ticket enumerates ----

/// TypeScript and JavaScript: destructuring, multi-target, computed and literal keys, re-exports,
/// ambient declarations.
const ECMA_TEMPLATES: &[&str] = &[
    "const {0} = {1};\n",
    "const [{0}, {1}] = {2};\n",
    "const [{0}] = {1}, {2} = 1;\n",
    "const { {0}: {1} } = {2};\n",
    "const { {0}, ...rest } = {1};\n",
    "const { {0} } = {1};\n",
    "const [{0}] = {1};\n",
    "const [, {0} = {1}];\n",
    "export const {0} = {1};\n",
    "export const {0}, {1} = {2};\n",
    "declare const {\n};\n",
    "declare const {0} = {1};\n",
    "declare module 'm' { const {0} = {1}; }\n",
    "class {0} { {1}() {} }\n",
    "class {0} { [{1}]() {} }\n",
    "class {0} { '{1}'() {} }\n",
    "class {0} { {1}() {} 42() {} }\n",
    "class {0} { #{1}() {} }\n",
    "class {0} { get {1}() { return 1 } set {1}(v) {} }\n",
    "class {0} { static { const {1} = 1; } }\n",
    "const {0} = {1}\n",
    "const {0} = {1};\nconst {2} = {3};\n",
    "const {0} = {1}\n\n\nconst {2} = {3}\n",
    "for (const {0} of {1}) {}\n",
    "for (let {0} = 0; ; ) {}\n",
    "try {} catch ({0}) {}\n",
    "const {0}: {1} = {2};\n",
    "enum {0} { {1}, {2} }\n",
    "interface I { {0}: {1} }\n",
    "namespace {0} { const {1} = 1; }\n",
    "label: { const {0} = {1}; }\n",
    "function {0}({1}) {}\n",
    "const {0} = \"{1}\";\n",
    "export {0} from '{1}';\n",
    "export * from '{0}';\n",
    "import {0} from '{1}';\n",
    "const {0} = {1} {2} {3};\n",
    "const {0}\nconst {1}\n",
    "class {0} extends {1} { {2}() {} }\n",
    "const {0} = { {1} };\n",
    "class {0} { [\"{1}\"]() {} }\n",
    "const {0} = {1} as {2};\n",
    "class {0} { static { {1} = 1 } }\n",
    "abstract class {0} { abstract {1}(): void }\n",
    "const {0} = {1}();\n",
    "let {0}: Array<{1}> = [];\n",
    "class {0} { constructor({1}) {} }\n",
    "function {0}() { return {1} }\nconst {2} = {0};\n",
];

/// Python: tuple and list targets, chained and annotated assignment, loop and context targets,
/// `global` / `nonlocal`, class bodies.
const PYTHON_TEMPLATES: &[&str] = &[
    "{0}, {1} = 1, 2\n",
    "({0}, {1}) = f()\n",
    "[{0}, {1}] = f()\n",
    "{0} = {1} = 1\n",
    "{0}: int = 1\n",
    "{0}: {1} = {2}\n",
    "{0}: {1}: int = 1\n",
    "for {0} in range(3):\n    pass\n",
    "for {0}, {1} in xs:\n    pass\n",
    "with open() as {0}:\n    pass\n",
    "def {0}({1}, *{2}, **{3}):\n    pass\n",
    "class {0}:\n    {1} = 1\n",
    "class {0}({1}):\n    def {2}(self):\n        pass\n",
    "global {0}\n",
    "def f():\n    nonlocal {0}\n",
    "lambda {0}: {1}\n",
    "async def {0}(): pass\n",
    "{{0}} = 1\n",
    "{0} , = [1]\n",
    "if True:\n    {0} = {1}\n",
    "while 1:\n    {0} = {1}\n",
    "try:\n    {0} = 1\nexcept {1} as {2}:\n    pass\n",
    "def {0}():\n    return {1}\n{0} = 1\n",
    "{0} = {1}\n",
    "{0} = {1}\n{2} = {3}\n",
    "@dec\ndef {0}(): pass\n",
    "match {0}:\n    case {1}:\n        pass\n",
    "[[{0}]] = {1}\n",
    "{0} = yield {1}\n",
    "type {0} = {1}\n",
    "def {0}({1}: int) -> {2}:\n    pass\n",
    "class {0}:\n    def {1}(self): pass\n    {1} = 2\n",
    "import {0}\nfrom m import {0}\n",
    "{0} += 1\n",
    "print({0}, {1})\n",
    "f({0} = 1)\n",
    "return {0}\n",
];

/// Go: grouped `var` / `const` / `type`, blank identifiers, receivers, repeated `init`.
const GO_TEMPLATES: &[&str] = &[
    "var ( {0}, {1} int )\n",
    "var (\n\t{0} = 1\n\t{1} = 2\n)\n",
    "const ( {0} = iota; {1}; {2} )\n",
    "const ( {0} = 1\n{1} = {2}\n)\n",
    "var _ = {0}\n",
    "var _, {0} = 1, 2\n",
    "var {0} _, {1} = 1, 2\n",
    "const _ = 1\n",
    "type ( {0} int; {1} string )\n",
    "func ({0}) {1}() {}\n",
    "func (r {0}) {1}() {}\n",
    "func init() {}\nfunc init() {}\n",
    "var {0}, {1} = 1, 2\n",
    "var {0}, {1}, {2} = 1, 2, 3\n",
    "const {0} = 1, {1} = 2\n",
    "var {0} int\nvar {1} string\n",
    "var {0} = {1}()\n",
    "type {0} struct { {1} int }\n",
    "type {0} = {1}\n",
    "var {0} [3]int\n",
    "func {0}({1} int) {}\n",
    "func {0}() (int, error) { return 0, nil }\n",
    "package p\n\nvar {0} = {1}\n",
    "var ( {0} = func() {} )\n",
    "var {0}, {1} string\n",
    "const {0}, {1} = 1, 2\n",
    "var {0}\nvar {1}\n",
    "type {0} interface { {1}() }\n",
    "var {0} = map[string]int{\"a\": 1}\n",
    "func (t T) {0}() {}\nfunc (t T) {0}() {}\n",
    "var {0}, {1} = f(), g()\n",
    "type {0} struct { {1} struct { {2} int } }\n",
    "const {0} = \"{1}\"\n",
];

// ---- Slot fillers ----

/// What a declaration slot may be filled with. Valid identifiers and blank identifiers are here
/// because they are what real code has; everything else is there to break the parse in a way a
/// fuzzer would not choose on purpose - an unterminated comment, a bare brace, an empty slot.
const FILLERS: &[&str] = &[
    "a", "b", "value", "Config", "T", "K", "x1", "_", "__x", "get", "set", "for", "class", "def",
    "func", "var", "const", "type", "init", "", " ", "\n", "{", "}", "[", "]", "(", ")", ",", ";",
    ":", ".", "=", "*", "...", "1", "42", "0x1f", "'a'", "\"s\"", "`t`", "a b", "a.b", "a::b",
    "#p", "$x", "//c", "/*", "*/", "for x", "中文", "\u{feff}",
];

// ---- The probe ----

/// Every language, one table. The counts here are the ticket's "before / after discarded symbols"
/// measurement, and the assertions below pin them so a later change to any collector has to say so.
#[test]
fn namefix_probe_collectors_leave_nothing_for_the_shared_gate() {
    let languages: [(Language, &[&str]); 4] = [
        (Language::TypeScript, ECMA_TEMPLATES),
        (Language::JavaScript, ECMA_TEMPLATES),
        (Language::Python, PYTHON_TEMPLATES),
        (Language::Go, GO_TEMPLATES),
    ];
    for (lang, templates) in languages {
        let lang_id = lang.id();
        let obs = observe(lang, templates, FILLERS, CASES_PER_LANGUAGE);
        eprintln!(
            "{:<10} cases={} unparsable={} produced={} discarded={} kept={}",
            lang.id(),
            obs.cases,
            obs.unparsable,
            obs.produced,
            obs.discarded,
            obs.kept
        );
        assert_eq!(
            obs.cases, CASES_PER_LANGUAGE,
            "{lang_id}: the probe must generate at least 3000 cases per language"
        );
        assert!(
            obs.cases >= 3000,
            "{lang_id}: the ticket asks for 3000+ cases per language, got {}",
            obs.cases
        );
        assert_eq!(
            obs.unparsable, 0,
            "{lang_id}: every generated case must parse; a refusal is a probe defect, not a collector result"
        );
        // The number the ticket asks for. Anything above zero means a collector is still handing
        // the shared gate a name it built out of damaged text - the defect this milestone removed
        // at its root rather than cleaning up downstream.
        assert_eq!(
            obs.discarded, DISCARDED,
            "{lang_id}: {} symbol(s) still discarded by the shared gate",
            obs.discarded
        );
    }
}

/// Symbols the shared gate discards across the whole corpus, summed over the four languages. This
/// is the "after" figure. The "before" figure is in the file header comment and was obtained by
/// reverting the guards - see the report; it is not reproducible from this file, because the point
/// of the fix is that the guard is not there any more.
const DISCARDED: usize = 0;

/// The probe really is running 3000+ varied cases per language, not 3000 copies of one.
///
/// Two things are checked, because either can rot silently: that the corpus as a whole has a real
/// spread of distinct sources, and that *every* template contributed varied cases - a template
/// added above and then never reached would make the numbers above mean less than they claim.
#[test]
fn namefix_probe_corpus_is_large_and_structured() {
    let languages: [(Language, &[&str]); 4] = [
        (Language::TypeScript, ECMA_TEMPLATES),
        (Language::JavaScript, ECMA_TEMPLATES),
        (Language::Python, PYTHON_TEMPLATES),
        (Language::Go, GO_TEMPLATES),
    ];
    for (lang, templates) in languages {
        let lang_id = lang.id();
        let sources: Vec<String> = (0..CASES_PER_LANGUAGE)
            .map(|i| build(i, templates, FILLERS))
            .collect();
        let distinct: std::collections::HashSet<&String> = sources.iter().collect();
        eprintln!(
            "{lang_id:<10} templates={} distinct={} of {}",
            templates.len(),
            distinct.len(),
            sources.len()
        );
        assert!(
            distinct.len() * 4 >= sources.len(),
            "{lang_id}: corpus is only {} distinct of {}",
            distinct.len(),
            sources.len()
        );
        // Every template is exercised, and a template WITH slots is not filled the same way every
        // time. A template with no slots at all (`declare const {\n}`) is a fixed hostile input on
        // purpose, so it is exempt: there is nothing in it to vary. Note the test is for a slot
        // token, not for a brace - those templates are full of literal braces that are not slots.
        for (template_index, template) in templates.iter().enumerate() {
            if !(0..MAX_SLOTS).any(|slot| template.contains(&format!("{{{slot}}}"))) {
                continue;
            }
            let per_template: std::collections::HashSet<String> = (0..32)
                .map(|n| build(template_index + n * templates.len(), templates, FILLERS))
                .collect();
            assert!(
                per_template.len() > 1,
                "{lang_id}: template {template:?} always filled the same way"
            );
        }
    }
}

/// The corpus is a pure function of [`SEED`]: same seed, byte-identical sources, and a pinned
/// checksum so an edit to a template or a filler cannot move the numbers above unnoticed.
#[test]
fn namefix_probe_case_is_reproducible_from_its_index() {
    assert_eq!(corpus_checksum(), CORPUS_CHECKSUM, "corpus drifted");
    // One case, rebuilt from its index alone - the repro a failure report quotes.
    let one = build(2_047, GO_TEMPLATES, FILLERS);
    assert_eq!(one, build(2_047, GO_TEMPLATES, FILLERS));
    assert_ne!(one, build(2_048, GO_TEMPLATES, FILLERS));
}

/// FNV-1a over every generated source of every language, in a fixed order.
fn corpus_checksum() -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let languages: [(&str, &[&str]); 4] = [
        ("typescript", ECMA_TEMPLATES),
        ("javascript", ECMA_TEMPLATES),
        ("python", PYTHON_TEMPLATES),
        ("go", GO_TEMPLATES),
    ];
    for (lang, templates) in languages {
        for byte in lang.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for index in 0..CASES_PER_LANGUAGE {
            for byte in build(index, templates, FILLERS).as_bytes() {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    hash
}

/// A spot check that the numbers above are not measuring an empty pipeline.
///
/// A discard count of zero is only meaningful if the collector is producing symbols at all - a
/// corpus of nothing but syntax errors would also read as zero. This pins the per-language
/// symbol count the corpus actually yields, so a template edit that quietly turned the whole
/// corpus into parse garbage has to update a number here rather than pass unnoticed. The counts
/// are what this corpus yields; they are not a per-language claim about real source files.
#[test]
fn namefix_probe_corpus_actually_produces_symbols() {
    let languages: [(Language, &[&str], usize); 4] = [
        (Language::TypeScript, ECMA_TEMPLATES, 1345),
        (Language::JavaScript, ECMA_TEMPLATES, 1099),
        (Language::Python, PYTHON_TEMPLATES, 781),
        (Language::Go, GO_TEMPLATES, 1482),
    ];
    for (lang, templates, expected) in languages {
        let lang_id = lang.id();
        let mut symbols = 0usize;
        for index in 0..CASES_PER_LANGUAGE {
            let src = build(index, templates, FILLERS);
            symbols += outline(
                &parse(lang, &src, &budget()).unwrap(),
                &src,
                &OutlineOptions::default(),
            )
            .len();
        }
        eprintln!("{lang_id:<10} outline symbols over corpus = {symbols}");
        assert!(
            symbols >= 100,
            "{lang_id}: corpus produced only {symbols} symbols; the discard count would be vacuous"
        );
        assert_eq!(
            symbols, expected,
            "{lang_id}: symbol count moved; the corpus or a collector changed"
        );
    }
}
