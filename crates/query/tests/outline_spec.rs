//! Spec for the outline tickets (ISSUE-QUERY-OUTLINE-*). Add cases; never weaken these.
//! Line numbers refer to the files under tests/fixtures/.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::SymbolKind::*;
use opencrayast_query::{OutlineOptions, Symbol, SymbolKind, find_symbols, outline, symbol_text};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

fn run(lang: Language, src: &str, opts: &OutlineOptions) -> Vec<Symbol> {
    let p = parse(lang, src, &budget()).unwrap();
    outline(&p, src, opts)
}

type Row = (
    usize,
    SymbolKind,
    &'static str,
    &'static str,
    usize,
    usize,
    Option<&'static str>,
);

fn check(lang: Language, src: &str, want: &[Row]) {
    let got = run(lang, src, &OutlineOptions::default());
    let g: Vec<(usize, SymbolKind, &str, &str, usize, usize)> = got
        .iter()
        .map(|s| {
            (
                s.depth,
                s.kind,
                s.name.as_str(),
                s.qualified.as_str(),
                s.start_line,
                s.end_line,
            )
        })
        .collect();
    let w: Vec<(usize, SymbolKind, &str, &str, usize, usize)> = want
        .iter()
        .map(|r| (r.0, r.1, r.2, r.3, r.4, r.5))
        .collect();
    assert_eq!(g, w, "{lang:?} symbols");
    for (s, r) in got.iter().zip(want) {
        if let Some(sig) = r.6 {
            assert_eq!(s.signature, sig, "{lang:?} signature of {}", s.qualified);
        }
        assert_eq!(
            &src[s.start_byte..s.end_byte].lines().count(),
            &(s.end_line - s.start_line + 1),
            "{lang:?} byte/line extent of {}",
            s.qualified
        );
    }
}

const RS: &str = include_str!("fixtures/sample.rs");
const TS: &str = include_str!("fixtures/sample.ts");
const JS: &str = include_str!("fixtures/sample.js");
const PY: &str = include_str!("fixtures/sample.py");
const GO: &str = include_str!("fixtures/sample.go");

#[test]
fn kind_names_roundtrip() {
    for k in [
        Module, Namespace, Class, Struct, Enum, Interface, Trait, Impl, Fn, Method, Const, Static,
        Type, Field, Variable, Macro,
    ] {
        assert_eq!(SymbolKind::parse(k.as_str()), Some(k));
    }
    assert_eq!(Fn.as_str(), "fn");
    assert_eq!(SymbolKind::parse("function"), None);
    assert_eq!(SymbolKind::parse("FN"), None);
}

#[test]
fn rust_outline() {
    check(
        Language::Rust,
        RS,
        &[
            (
                1,
                Struct,
                "Config",
                "Config",
                4,
                6,
                Some("pub struct Config"),
            ),
            (1, Impl, "Config", "Config", 8, 17, Some("impl Config")),
            (
                2,
                Method,
                "load",
                "Config::load",
                10,
                12,
                Some("pub fn load(path: &str) -> Result<Config, Error>"),
            ),
            (
                2,
                Method,
                "validate",
                "Config::validate",
                14,
                16,
                Some("fn validate(&self) -> bool"),
            ),
            (1, Enum, "Error", "Error", 19, 22, Some("pub enum Error")),
            (1, Trait, "Shape", "Shape", 24, 26, Some("pub trait Shape")),
            (
                2,
                Method,
                "area",
                "Shape::area",
                25,
                25,
                Some("fn area(&self) -> f64"),
            ),
            (
                1,
                Const,
                "MAX",
                "MAX",
                28,
                28,
                Some("const MAX: usize = 10"),
            ),
            (1, Module, "inner", "inner", 30, 32, Some("mod inner")),
            (
                2,
                Fn,
                "helper",
                "inner::helper",
                31,
                31,
                Some("pub fn helper()"),
            ),
            (1, Fn, "main", "main", 34, 34, Some("pub fn main()")),
        ],
    );
}

#[test]
fn typescript_outline() {
    check(
        Language::TypeScript,
        TS,
        &[
            (
                1,
                Class,
                "Config",
                "Config",
                2,
                13,
                Some("export class Config"),
            ),
            (
                2,
                Method,
                "load",
                "Config.load",
                6,
                8,
                Some("static load(path: string): Config"),
            ),
            (
                2,
                Method,
                "validate",
                "Config.validate",
                10,
                12,
                Some("validate(): boolean"),
            ),
            (
                1,
                Interface,
                "Shape",
                "Shape",
                15,
                17,
                Some("export interface Shape"),
            ),
            (
                2,
                Method,
                "area",
                "Shape.area",
                16,
                16,
                Some("area(): number"),
            ),
            (1, Enum, "Color", "Color", 19, 22, Some("export enum Color")),
            (
                1,
                Type,
                "Id",
                "Id",
                24,
                24,
                Some("export type Id = string | number"),
            ),
            (
                1,
                Const,
                "MAX",
                "MAX",
                26,
                26,
                Some("export const MAX = 10"),
            ),
            (
                1,
                Fn,
                "main",
                "main",
                28,
                28,
                Some("export function main(): void"),
            ),
            (1, Namespace, "Util", "Util", 30, 32, Some("namespace Util")),
            (
                2,
                Fn,
                "help",
                "Util.help",
                31,
                31,
                Some("export function help()"),
            ),
        ],
    );
}

#[test]
fn javascript_outline() {
    check(
        Language::JavaScript,
        JS,
        &[
            (1, Fn, "add", "add", 2, 4, Some("function add(a, b)")),
            (1, Class, "Counter", "Counter", 6, 14, Some("class Counter")),
            (
                2,
                Method,
                "constructor",
                "Counter.constructor",
                7,
                9,
                Some("constructor()"),
            ),
            (2, Method, "inc", "Counter.inc", 11, 13, Some("inc()")),
            (1, Const, "MAX", "MAX", 16, 16, Some("const MAX = 10")),
            (1, Fn, "twice", "twice", 18, 18, None), // arrow function bound to a const is a `fn`
        ],
    );
}

#[test]
fn python_outline() {
    check(
        Language::Python,
        PY,
        &[
            (1, Const, "MAX", "MAX", 3, 3, Some("MAX = 10")),
            (1, Fn, "add", "add", 6, 8, Some("def add(a, b)")),
            (1, Class, "Config", "Config", 11, 19, Some("class Config")),
            (
                2,
                Method,
                "load",
                "Config.load",
                14,
                15,
                Some("def load(self, path)"),
            ),
            (2, Method, "make", "Config.make", 17, 19, Some("def make()")), // starts at the decorator
            (
                1,
                Fn,
                "decorated",
                "decorated",
                22,
                24,
                Some("def decorated()"),
            ),
        ],
    );
}

#[test]
fn go_outline() {
    check(
        Language::Go,
        GO,
        &[
            (
                1,
                Struct,
                "Config",
                "Config",
                4,
                6,
                Some("type Config struct"),
            ),
            (
                1,
                Method,
                "Load",
                "Config.Load",
                9,
                11,
                Some("func (c *Config) Load(path string) error"),
            ),
            (
                1,
                Interface,
                "Shape",
                "Shape",
                13,
                15,
                Some("type Shape interface"),
            ),
            (
                2,
                Method,
                "Area",
                "Shape.Area",
                14,
                14,
                Some("Area() float64"),
            ),
            (1, Const, "Max", "Max", 17, 17, Some("const Max = 10")),
            (1, Fn, "Main", "Main", 19, 19, Some("func Main()")),
        ],
    );
}

#[test]
fn tsx_outline_sees_a_component() {
    let src = "export function App(): JSX.Element {\n  return <div/>;\n}\n";
    let s = run(Language::Tsx, src, &OutlineOptions::default());
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].kind, s[0].name.as_str()), (Fn, "App"));
}

#[test]
fn depth_cap_and_kind_filter() {
    let d1 = run(
        Language::Rust,
        RS,
        &OutlineOptions {
            max_depth: 1,
            ..Default::default()
        },
    );
    assert!(d1.iter().all(|s| s.depth == 1));
    assert_eq!(d1.len(), 7); // Config, impl, Error, Shape, MAX, inner, main
    let only_methods = run(
        Language::Rust,
        RS,
        &OutlineOptions {
            kinds: Some(vec![Method]),
            ..Default::default()
        },
    );
    let names: Vec<&str> = only_methods.iter().map(|s| s.qualified.as_str()).collect();
    assert_eq!(names, ["Config::load", "Config::validate", "Shape::area"]);
}

#[test]
fn docs_are_first_lines_only_and_only_when_requested() {
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let rs = run(Language::Rust, RS, &o);
    let doc = |v: &Vec<Symbol>, q: &str| {
        v.iter()
            .find(|s| s.qualified == q)
            .unwrap()
            .doc_first_line
            .clone()
    };
    assert_eq!(doc(&rs, "Config").as_deref(), Some("A configuration."));
    assert_eq!(doc(&rs, "Config::load").as_deref(), Some("Load it."));
    assert_eq!(doc(&rs, "Config::validate"), None);
    let ts = run(Language::TypeScript, TS, &o);
    assert_eq!(doc(&ts, "Config").as_deref(), Some("A config."));
    let py = run(Language::Python, PY, &o);
    assert_eq!(doc(&py, "add").as_deref(), Some("Adds."));
    assert_eq!(doc(&py, "Config").as_deref(), Some("A config."));
    let go = run(Language::Go, GO, &o);
    assert_eq!(
        doc(&go, "Config").as_deref(),
        Some("Config holds settings.")
    );
    assert_eq!(doc(&go, "Config.Load").as_deref(), Some("Load reads it."));
    let plain = run(Language::Rust, RS, &OutlineOptions::default());
    assert!(plain.iter().all(|s| s.doc_first_line.is_none()));
}

#[test]
fn a_file_with_syntax_errors_is_still_outlined() {
    let src = "pub fn good() {}\n\nfn broken( {\n\npub fn also_good() {}\n";
    let s = run(Language::Rust, src, &OutlineOptions::default());
    let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
    assert!(names.contains(&"good"), "{names:?}");
    assert!(names.contains(&"also_good"), "{names:?}");
}

#[test]
fn find_symbols_by_name_and_qualified_name() {
    let p = parse(Language::Rust, RS, &budget()).unwrap();
    assert_eq!(find_symbols(&p, RS, "load").len(), 1);
    assert_eq!(find_symbols(&p, RS, "Config::load").len(), 1);
    assert_eq!(
        find_symbols(&p, RS, "Config.load").len(),
        1,
        "either separator is accepted"
    );
    assert_eq!(
        find_symbols(&p, RS, "helper").len(),
        1,
        "found even below the outline depth cap"
    );
    assert_eq!(
        find_symbols(&p, RS, "Config").len(),
        2,
        "the struct and the impl block"
    );
    assert!(find_symbols(&p, RS, "nope").is_empty());
    assert!(find_symbols(&p, RS, "config").is_empty(), "case-sensitive");
    let two = "fn a() {}\nmod m { pub fn a() {} }\n";
    let p2 = parse(Language::Rust, two, &budget()).unwrap();
    assert_eq!(find_symbols(&p2, two, "a").len(), 2);
    assert_eq!(find_symbols(&p2, two, "m::a").len(), 1);
}

#[test]
fn symbol_text_cuts_whole_lines_with_doc_and_context() {
    let p = parse(Language::Rust, RS, &budget()).unwrap();
    let load = &find_symbols(&p, RS, "Config::load")[0];
    let plain = symbol_text(RS, load, false, 0).unwrap();
    assert_eq!((plain.first_line, plain.last_line), (10, 12));
    assert!(
        plain.text.starts_with("    pub fn load("),
        "{:?}",
        plain.text
    );
    assert!(plain.text.ends_with("    }"), "{:?}", plain.text);
    let with_doc = symbol_text(RS, load, true, 0).unwrap();
    assert_eq!((with_doc.first_line, with_doc.last_line), (9, 12));
    assert!(with_doc.text.starts_with("    /// Load it."));
    let ctx = symbol_text(RS, load, true, 2).unwrap();
    assert_eq!((ctx.first_line, ctx.last_line), (7, 14));
    let top = &find_symbols(&p, RS, "Config")[0];
    let clamp = symbol_text(RS, top, true, 99).unwrap();
    assert_eq!(clamp.first_line, 1);
    assert_eq!(clamp.last_line, RS.lines().count());
}

#[test]
fn python_decorators_belong_to_the_symbol_and_docstrings_need_nothing_extra() {
    let p = parse(Language::Python, PY, &budget()).unwrap();
    let make = &find_symbols(&p, PY, "Config.make")[0];
    let t = symbol_text(PY, make, true, 0).unwrap();
    assert_eq!((t.first_line, t.last_line), (17, 19));
    assert!(t.text.trim_start().starts_with("@staticmethod"));
}

#[test]
fn python_nested_async_property_init_lambda_and_class_attr() {
    // Supplemental cases for ISSUE-QUERY-OUTLINE-PYGO.
    let src = r#"
class Outer:
    class Inner:
        pass

    X_attr = 1
    MAX_V = 2

    @a
    @b
    @property
    def prop(self):
        """Prop doc."""
        return 1

    def __init__(self):
        self.x = 1

    async def run(self):
        pass

    def outer_fn(self):
        def nested():
            pass
        return nested

TOP = 1
f = lambda x: x
"#;
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let got = run(Language::Python, src, &o);
    let row = |q: &str| got.iter().find(|s| s.qualified == q).unwrap();

    assert_eq!(row("Outer.Inner").kind, Class);
    assert_eq!(row("Outer.Inner").depth, 2);

    assert_eq!(row("Outer.X_attr").kind, Variable);
    assert_eq!(row("Outer.MAX_V").kind, Const);

    let prop = row("Outer.prop");
    assert_eq!(prop.kind, Method);
    assert!(
        prop.start_line < prop.end_line,
        "multi-decorator extent includes @a/@b/@property"
    );
    assert_eq!(prop.signature, "def prop(self)");
    assert_eq!(prop.doc_first_line.as_deref(), Some("Prop doc."));
    // Extent starts at the first decorator line.
    assert!(
        src.lines()
            .nth(prop.start_line - 1)
            .unwrap()
            .trim_start()
            .starts_with("@a"),
        "start at first decorator"
    );

    assert_eq!(row("Outer.__init__").kind, Method);
    assert_eq!(row("Outer.__init__").signature, "def __init__(self)");

    let run_m = row("Outer.run");
    assert_eq!(run_m.signature, "async def run(self)");

    // Nested function inside a method must not appear.
    assert!(
        got.iter().all(|s| s.name != "nested"),
        "nested fn must be omitted: {:?}",
        got.iter().map(|s| s.qualified.as_str()).collect::<Vec<_>>()
    );

    // Judgment: `f = lambda x: x` is an assignment, not a function_definition → Variable.
    let f = row("f");
    assert_eq!(f.kind, Variable, "lambda assignment is Variable, not Fn");
    assert_eq!(f.signature, "f = lambda x: x");

    assert_eq!(row("TOP").kind, Const);
}

#[test]
fn python_and_go_docs_when_requested() {
    // Isolated from the Rust-outline cases in docs_are_first_lines_… (still unimplemented).
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let py = run(Language::Python, PY, &o);
    let doc = |v: &Vec<Symbol>, q: &str| {
        v.iter()
            .find(|s| s.qualified == q)
            .unwrap()
            .doc_first_line
            .clone()
    };
    assert_eq!(doc(&py, "add").as_deref(), Some("Adds."));
    assert_eq!(doc(&py, "Config").as_deref(), Some("A config."));
    let go = run(Language::Go, GO, &o);
    assert_eq!(
        doc(&go, "Config").as_deref(),
        Some("Config holds settings.")
    );
    assert_eq!(doc(&go, "Config.Load").as_deref(), Some("Load reads it."));
    let plain = run(Language::Go, GO, &OutlineOptions::default());
    assert!(plain.iter().all(|s| s.doc_first_line.is_none()));
}

#[test]
fn go_const_group_generics_embed_init_and_error_file() {
    let src = r#"
package main

const (
	A = 1
	B = 2
)

// Box is generic.
type Box[T any] struct {
	T
}

func Generic[T any](x T) T { return x }

func init() {}

func good() {}

func broken( {

func also_good() {}
"#;
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let got = run(Language::Go, src, &o);
    let names: Vec<&str> = got.iter().map(|s| s.qualified.as_str()).collect();

    let a = got.iter().find(|s| s.name == "A").unwrap();
    let b = got.iter().find(|s| s.name == "B").unwrap();
    assert_eq!(a.kind, Const);
    assert_eq!(a.signature, "const A = 1");
    assert_eq!(b.signature, "const B = 2");

    let box_t = got.iter().find(|s| s.name == "Box").unwrap();
    assert_eq!(box_t.kind, Struct);
    assert!(
        box_t.signature.contains("Box") && box_t.signature.contains("struct"),
        "{}",
        box_t.signature
    );
    assert!(box_t.signature.contains('['), "generic type params in sig");
    assert_eq!(box_t.doc_first_line.as_deref(), Some("Box is generic."));
    // Embedded field `T` is not a separate outline symbol this milestone.
    assert!(got.iter().all(|s| s.kind != Field));

    let generic_fn = got.iter().find(|s| s.name == "Generic").unwrap();
    assert_eq!(generic_fn.kind, Fn);
    assert!(
        generic_fn.signature.contains("Generic[T any]"),
        "{}",
        generic_fn.signature
    );

    let init = got.iter().find(|s| s.name == "init").unwrap();
    assert_eq!(init.kind, Fn);
    assert_eq!(init.signature, "func init()");

    assert!(
        names.contains(&"good"),
        "ERROR file still outlines symbols before the break: {names:?}"
    );
    // tree-sitter-go recovery folds `also_good` into the broken function_declaration's
    // parameter_list (observed in AST dump); we still must not panic and must keep `good`.
    // ISSUE-PRS-09-GO: that folding used to DROP `also_good` from the outline outright. The
    // collector now recovers it, so the symbol is asserted present rather than absent.
    assert!(
        names.contains(&"also_good"),
        "also_good is absorbed by ERROR recovery but must still be outlined: {names:?}"
    );
}

#[test]
fn go_doc_start_line_includes_comment_block() {
    let p = parse(Language::Go, GO, &budget()).unwrap();
    let load = &find_symbols(&p, GO, "Config.Load")[0];
    let with_doc = symbol_text(GO, load, true, 0).unwrap();
    assert_eq!(with_doc.first_line, 8); // "// Load reads it."
    assert!(with_doc.text.starts_with("// Load reads it."));
}

fn assert_symbols_sane(src: &str, syms: &[Symbol]) {
    for s in syms {
        assert!(
            !s.name.trim().is_empty(),
            "empty name in {:?} sig={:?}",
            s.kind,
            s.signature
        );
        assert!(s.start_byte <= s.end_byte, "{s:?}");
        assert!(s.end_byte <= src.len(), "{s:?}");
        assert!(
            src.is_char_boundary(s.start_byte) && src.is_char_boundary(s.end_byte),
            "byte range not on char boundary for {}",
            s.qualified
        );
        assert!(s.start_line >= 1, "{s:?}");
        assert!(s.end_line >= s.start_line, "{s:?}");
    }
}

#[test]
fn pygo_broken_inputs_never_emit_empty_names() {
    // CR repros: broken trees that previously emitted Variable/Const with name "".
    // (1) Claude's pasted probe input (ellipsis was in the CR text; full case triggers
    //     MISSING identifier LHS). (2) Second independent MISSING-identifier assignment.
    let py_inputs: &[&str] = &[
        " ) : ''' ) \n ) else é @dec : else def ...",
        ":passif+ifreturnclassclass@dec=@.defasync'''''']--:NAMEawait@dec{forreturn{*await\"\"\"classlambda,=)@dec \nindef)éx=",
        "#\n=\"\"\"\nelse=*/import+asyncdef[@dec",
    ];
    for src in py_inputs {
        let got = run(Language::Python, src, &OutlineOptions::default());
        assert_symbols_sane(src, &got);
    }
    let go_inputs: &[&str] = &[
        "package main\n=packagemain\nconst;xé/*{ Name",
        "package main\nconst;package,é=string* stringconst&=&typex*/func * stringerrortypeNamepackageint * int typetypeerror{   }[Namefunc [type/*[interface main",
    ];
    for src in go_inputs {
        let got = run(Language::Go, src, &OutlineOptions::default());
        assert_symbols_sane(src, &got);
    }
}

#[test]
fn pygo_seeded_random_broken_sources_stay_sane() {
    // Fixed-seed adversarial strings (≥1000 each). Must not panic; every symbol has a
    // non-empty name and a legal byte/line extent on a char boundary.
    let py_alpha: &[&str] = &[
        "def", "class", "else", "if", "return", "async", "await", "(", ")", ":", "=", "@", "'''",
        "\"\"\"", "\n", "é", "NAME", "x", " ", "[", "]", "{", "}", ",", ".", "*", "/", "+", "-",
        "pass", "lambda", "for", "in", "import", "from", "#", "@dec",
    ];
    let go_alpha: &[&str] = &[
        "func",
        "type",
        "const",
        "var",
        "struct",
        "interface",
        "package",
        "main",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "=",
        "*",
        "&",
        ",",
        ";",
        "\n",
        " ",
        "Name",
        "x",
        "error",
        "string",
        "int",
        "//",
        "/*",
        "*/",
        "é",
    ];
    let mut x: u64 = 0xA5A5_C0FF_EE42_u64;
    for i in 0..1200u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = 8 + (x % 48) as usize;
        let mut py = String::new();
        for _ in 0..n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            py.push_str(py_alpha[(x as usize) % py_alpha.len()]);
            if x.is_multiple_of(7) {
                py.push(' ');
            }
        }
        let got = run(Language::Python, &py, &OutlineOptions::default());
        assert_symbols_sane(&py, &got);

        x ^= x.wrapping_add(i.wrapping_mul(0x9E37_79B9));
        let mut go = String::from("package main\n");
        for _ in 0..n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            go.push_str(go_alpha[(x as usize) % go_alpha.len()]);
            if x.is_multiple_of(7) {
                go.push(' ');
            }
        }
        let got = run(Language::Go, &go, &OutlineOptions::default());
        assert_symbols_sane(&go, &got);
    }
}

#[test]
fn ecma_export_default_async_generator_abstract_decorator_namespace_declare() {
    let src = r#"
/** Doc. */
@dec
export default abstract class Foo {
  @m
  async bar() { return 1; }
  *gen() { yield 1; }
}
export default function baz() {}
declare function decl(x: number): void;
namespace A {
  namespace B {
    export const X = 1;
  }
}
"#;
    let o = OutlineOptions {
        include_docs: true,
        ..Default::default()
    };
    let got = run(Language::TypeScript, src, &o);
    let row = |q: &str| {
        got.iter().find(|s| s.qualified == q).unwrap_or_else(|| {
            panic!(
                "missing {q} in {:?}",
                got.iter().map(|s| s.qualified.as_str()).collect::<Vec<_>>()
            )
        })
    };

    let foo = row("Foo");
    assert_eq!(foo.kind, Class);
    assert!(
        foo.signature.contains("abstract class Foo"),
        "{}",
        foo.signature
    );
    assert!(foo.signature.contains("export"), "{}", foo.signature);
    assert_eq!(foo.doc_first_line.as_deref(), Some("Doc."));
    // Extent includes the decorator line.
    assert!(
        src.lines()
            .nth(foo.start_line - 1)
            .unwrap()
            .trim_start()
            .starts_with("@dec"),
        "start at decorator"
    );

    let bar = row("Foo.bar");
    assert_eq!(bar.kind, Method);
    assert_eq!(bar.signature, "async bar()");
    assert!(
        src.lines()
            .nth(bar.start_line - 1)
            .unwrap()
            .trim_start()
            .starts_with("@m"),
        "method extent includes decorator"
    );

    let gen_m = row("Foo.gen");
    assert_eq!(gen_m.kind, Method);
    assert!(gen_m.signature.contains("gen()"), "{}", gen_m.signature);

    assert_eq!(row("baz").kind, Fn);
    assert!(row("baz").signature.contains("export default function baz"));

    let decl = row("decl");
    assert_eq!(decl.kind, Fn);
    assert!(decl.signature.contains("declare function decl"));

    assert_eq!(row("A").kind, Namespace);
    assert_eq!(row("A.B").kind, Namespace);
    assert_eq!(row("A.B.X").kind, Const);
}

#[test]
fn ecma_tsx_jsx_dts_and_error_file() {
    let tsx = "export function Widget(props: { n: number }) {\n  return <div>{props.n}</div>;\n}\n";
    let w = run(Language::Tsx, tsx, &OutlineOptions::default());
    assert_eq!(w.len(), 1);
    assert_eq!((w[0].kind, w[0].name.as_str()), (Fn, "Widget"));

    let dts = "export declare class C {\n  m(): void;\n}\nexport type T = string;\n";
    let d = run(Language::TypeScript, dts, &OutlineOptions::default());
    let names: Vec<&str> = d.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"C"), "{names:?}");
    assert!(names.contains(&"T"), "{names:?}");

    let bad = "export function good() {}\n\nfunction broken( {\n\nexport function also() {}\n";
    let b = run(Language::TypeScript, bad, &OutlineOptions::default());
    let bn: Vec<&str> = b.iter().map(|s| s.name.as_str()).collect();
    assert!(bn.contains(&"good"), "{bn:?}");
    assert_symbols_sane(bad, &b);
}

#[test]
fn ecma_seeded_random_broken_sources_stay_sane() {
    let alpha: &[&str] = &[
        "function",
        "class",
        "const",
        "export",
        "default",
        "async",
        "interface",
        "type",
        "enum",
        "namespace",
        "declare",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "=",
        "=>",
        ";",
        ":",
        ",",
        "*",
        "/",
        "+",
        "-",
        "\n",
        " ",
        "Name",
        "x",
        "é",
        "/*",
        "*/",
        "//",
        "<",
        ">",
        "/",
        "@",
        "extends",
        "implements",
    ];
    let mut x: u64 = 0x00EC_A5E1_D042_u64;
    for i in 0..1200u64 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let n = 8 + (x % 48) as usize;
        let mut src = String::new();
        for _ in 0..n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            src.push_str(alpha[(x as usize) % alpha.len()]);
            if x.is_multiple_of(7) {
                src.push(' ');
            }
        }
        // Alternate among the three grammars.
        let lang = match i % 3 {
            0 => Language::TypeScript,
            1 => Language::Tsx,
            _ => Language::JavaScript,
        };
        let got = run(lang, &src, &OutlineOptions::default());
        assert_symbols_sane(&src, &got);
    }
}
