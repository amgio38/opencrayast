//! Shared corpus: ≥40 (pattern × source) cases per language.

use opencrayast_lang::Language as OcLang;

use super::DiffLang;

/// One differential case.
#[derive(Debug, Clone)]
pub struct Case {
    pub id: String,
    pub lang: DiffLang,
    pub pattern: String,
    pub source: String,
    /// When both matchers exist, results must agree (unless listed in KNOWN_DIVERGENCES).
    pub expect_agree: bool,
}

fn push(
    out: &mut Vec<Case>,
    lang: OcLang,
    id: &str,
    pattern: &str,
    source: &str,
    expect_agree: bool,
) {
    out.push(Case {
        id: format!("{}:{id}", lang.id()),
        lang: DiffLang(lang),
        pattern: pattern.to_string(),
        source: source.to_string(),
        expect_agree,
    });
}

fn pad_to(
    out: &mut Vec<Case>,
    lang: OcLang,
    target: usize,
    expect_agree: bool,
    mk: impl Fn(usize) -> (String, String),
) {
    let mut i = 0usize;
    while out.iter().filter(|c| c.lang.0 == lang).count() < target {
        let (pat, src) = mk(i);
        push(out, lang, &format!("pad-{i}"), &pat, &src, expect_agree);
        i += 1;
    }
}

fn js_cases(out: &mut Vec<Case>) {
    let l = OcLang::JavaScript;
    let hand: &[(&str, &str, &str)] = &[
        (
            "single-meta",
            "console.log($X)",
            "console.log(1);\nconsole.log(x);",
        ),
        ("repeat-meta", "f($A, $A)", "f(1, 1);"), // known: repeat-metavar-capture-span
        ("list-empty", "f($$$A)", "f();\nf(1);"),
        ("list-multi", "f($$$ARGS)", "f(1, 2, 3);"),
        ("list-ends", "f($A, $$$M, $Z)", "f(1, 2, 3, 4);"), // known: asg-miss-list-ends
        ("nested-call", "f(g($X))", "f(g(1));\nf(h(1));"),
        ("comment", "f($A)", "f(/*x*/1);"),
        ("string-lit", "f($S)", "f(\"hi\");\nf('x');"),
        ("number-lit", "$N + 1", "2 + 1;\nx + 1;"),
        ("binop", "$A + $B", "a + b;\na - b;"),
        ("stmt-expr", "return $X;", "function f() { return 1; }"),
        ("member-chain", "$O.$A.$B", "a.b.c;\na.b;"),
        ("arrow", "($A) => $B", "const f = (x) => x + 1;"),
        ("destructure", "const { $A } = $B;", "const { a } = obj;"),
        ("template", "`$A`", "const s = `hi`;"),
        ("optional-chain", "$O?.$M", "a?.b;"),
        ("new-call", "new $C($$$A)", "new Foo(1, 2);"),
        ("assign", "$A = $B", "x = 1;"),
        ("ternary", "$A ? $B : $C", "a ? b : c;"),
        ("array", "[$$$E]", "[1, 2, 3];"),
        ("object", "{ $K: $V }", "({ a: 1 });"),
        ("if-stmt", "if ($C) $B", "if (x) y;"),
        (
            "for-of",
            "for (const $A of $B) $$$BODY",
            "for (const x of xs) { f(x); }",
        ),
        (
            "try-catch",
            "try { $$$A } catch ($E) { $$$B }",
            "try { f(); } catch (e) { g(); }",
        ),
        (
            "class-method",
            "class $C { $M($$$A) { $$$B } }",
            "class A { m(x) { return x; } }",
        ),
        (
            "async-fn",
            "async function $F($$$A) { $$$B }",
            "async function f(a) { await a; }",
        ),
        ("await", "await $X", "async function f() { await p; }"),
        ("spread", "f(...$A)", "f(...xs);"),
        ("regex-lit", "$R.test($S)", "/a/.test(s);"),
        ("broken-src", "f($X)", "f(1;\ngood(2);"),
        ("nested-list", "f(g($$$A))", "f(g(1, 2));"),
        ("double-dollar-lit", "f($$)", "f($);"),
        ("anon-meta", "f($_)", "f(1);\nf(x);"),
        ("multi-stmt-src", "f($X)", "f(1);\nf(2);\nf(3);"),
        ("call-member", "$O.f($$$A)", "console.log(1);\nobj.f(a, b);"),
    ];
    for (id, pat, src) in hand {
        push(out, l, id, pat, src, true);
    }
    pad_to(out, l, 40, true, |i| {
        (
            "console.log($X)".into(),
            format!("console.log({i});\nvoid {i};"),
        )
    });
}

fn ts_cases(out: &mut Vec<Case>) {
    let l = OcLang::TypeScript;
    let hand: &[(&str, &str, &str)] = &[
        (
            "typed-fn",
            "function $F($A: $T): $R { $$$B }",
            "function f(a: number): number { return a; }",
        ),
        (
            "generic-fn",
            "function $F<$T>($A: $T) { $$$B }",
            "function f<T>(a: T) { return a; }",
        ),
        (
            "interface",
            "interface $I { $M: $T }",
            "interface I { m: number }",
        ),
        ("type-alias", "type $N = $T;", "type N = string;"),
        (
            "class-field",
            "class $C { $F: $T }",
            "class C { f: number }",
        ),
        (
            "decorator",
            "@$D class $C { $$$B }",
            "@dec class C { x = 1 }",
        ),
        ("enum", "enum $E { $$$V }", "enum E { A, B }"),
        ("as-expr", "$X as $T", "const x = a as string;"),
        (
            "satisfies",
            "$X satisfies $T",
            "const x = a satisfies string;",
        ),
        (
            "ns",
            "namespace $N { $$$B }",
            "namespace N { export const x = 1; }",
        ),
        (
            "import-type",
            "import type { $A } from $M",
            "import type { A } from 'm';",
        ),
        (
            "export-type",
            "export type $N = $T;",
            "export type N = number;",
        ),
        (
            "param-prop",
            "constructor(public $A: $T) {}",
            "class C { constructor(public a: number) {} }",
        ),
        (
            "readonly",
            "readonly $A: $T",
            "class C { readonly a: number }",
        ),
        ("optional-prop", "$A?: $T", "type T = { a?: number };"),
        ("union", "type $N = $A | $B;", "type N = string | number;"),
        ("intersect", "type $N = $A & $B;", "type N = A & B;"),
        ("keyof", "type $N = keyof $T;", "type N = keyof T;"),
        (
            "mapped",
            "type $N = { [K in $K]: $V };",
            "type N = { [K in Keys]: number };",
        ),
        (
            "arrow-typed",
            "($A: $T) => $B",
            "const f = (a: number) => a;",
        ),
        ("call", "f($$$A)", "f(1, 2);"),
        ("member", "$O.$M", "obj.m;"),
        ("new", "new $C($$$A)", "new C(1);"),
        ("type-param-call", "f<$T>($X)", "f<number>(1);"),
        ("comment", "f($A)", "f(/*t*/1);"),
        ("string", "f($S)", "f(\"x\");"),
        ("number", "$N * 2", "3 * 2;"),
        ("binop", "$A && $B", "a && b;"),
        ("list-empty", "f($$$A)", "f();"),
        ("list-multi", "f($$$A)", "f(1, 2, 3);"),
        ("nested", "f(g($X))", "f(g(1));"),
        ("repeat", "f($A, $A)", "f(1, 1);"), // known: same as repeat-metavar-capture-span
        (
            "async",
            "async function $F() { $$$B }",
            "async function f() { await 1; }",
        ),
        ("await", "await $X", "async function f() { await p; }"),
        ("broken", "f($X)", "f(;"),
        (
            "class-method",
            "class $C { $M() { $$$B } }",
            "class C { m() { return 1; } }",
        ),
        (
            "interface-method",
            "interface $I { $M($$$A): $R }",
            "interface I { m(a: number): void }",
        ),
        ("type-query", "type $N = typeof $X;", "type N = typeof x;"),
        (
            "cond-type",
            "type $N = $A extends $B ? $C : $D;",
            "type N = A extends B ? C : D;",
        ),
        ("tuple", "type $N = [$A, $B];", "type N = [string, number];"),
    ];
    for (id, pat, src) in hand {
        push(out, l, id, pat, src, true);
    }
    pad_to(out, l, 40, true, |i| {
        (
            "f($X)".into(),
            format!("function f(x: number) {{ return x + {i}; }}\nf({i});"),
        )
    });
}

fn py_cases(out: &mut Vec<Case>) {
    let l = OcLang::Python;
    let hand: &[(&str, &str, &str)] = &[
        ("call", "print($X)", "print(1)\nprint(x)"),
        ("repeat", "f($A, $A)", "f(1, 1)\nf(1, 2)"),
        ("list-empty", "f($$$A)", "f()\nf(1)"),
        ("list-multi", "f($$$ARGS)", "f(1, 2, 3)"),
        ("nested", "f(g($X))", "f(g(1))"),
        ("comment", "f($A)", "f(1)  # c"),
        ("string", "f($S)", "f('hi')\nf(\"x\")"),
        ("number", "$N + 1", "2 + 1"),
        ("binop", "$A + $B", "a + b"),
        ("return", "return $X", "def f():\n    return 1"),
        ("attr", "$O.$A", "a.b"),
        ("lambda", "lambda $A: $B", "f = lambda x: x + 1"),
        ("destruct", "$A, $B = $C", "a, b = xs"),
        (
            "decorator",
            "@$D\ndef $F($$$A):\n    $$$B",
            "@dec\ndef f(a):\n    return a",
        ),
        (
            "with",
            "with $C as $A:\n    $$$B",
            "with open('f') as f:\n    f.read()",
        ),
        (
            "for",
            "for $A in $B:\n    $$$BODY",
            "for x in xs:\n    print(x)",
        ),
        ("if", "if $C:\n    $$$B", "if x:\n    y"),
        ("class", "class $C:\n    $$$B", "class A:\n    x = 1"),
        (
            "async-def",
            "async def $F($$$A):\n    $$$B",
            "async def f(a):\n    await a",
        ),
        ("await", "await $X", "async def f():\n    await p"),
        ("list-lit", "[$$$E]", "[1, 2, 3]"),
        ("dict", "{ $K: $V }", "{'a': 1}"),
        ("fstring", "f($S)", "f(f'hi')"),
        ("star", "f(*$A)", "f(*xs)"),
        ("kw", "f($A=$B)", "f(a=1)"),
        ("comp", "[$A for $B in $C]", "[x for x in xs]"),
        (
            "try",
            "try:\n    $$$A\nexcept $E:\n    $$$B",
            "try:\n    f()\nexcept E:\n    g()",
        ),
        ("import", "import $M", "import os"),
        ("from-import", "from $M import $A", "from os import path"),
        ("assign", "$A = $B", "x = 1"),
        ("augassign", "$A += $B", "x += 1"),
        ("broken", "f($X)", "f(1\ngood(2)"),
        ("nested-list", "f(g($$$A))", "f(g(1, 2))"),
        ("anon", "f($_)", "f(1)"),
        ("multi-call", "f($X)", "f(1)\nf(2)\nf(3)"),
        ("method", "$O.f($$$A)", "obj.f(1, 2)"),
        ("slice", "$A[$B:$C]", "a[1:2]"),
        ("ternary", "$A if $C else $B", "a if c else b"),
        ("with-multi", "with $A, $B:\n    $$$C", "with a, b:\n    c"),
        (
            "match-case",
            "match $X:\n    case $P:\n        $$$B",
            "match x:\n    case 1:\n        y",
        ),
    ];
    for (id, pat, src) in hand {
        push(out, l, id, pat, src, true);
    }
    pad_to(out, l, 40, true, |i| {
        ("print($X)".into(), format!("print({i})\nx = {i}"))
    });
}

fn go_cases(out: &mut Vec<Case>) {
    let l = OcLang::Go;
    let hand: &[(&str, &str, &str)] = &[
        (
            "call",
            "fmt.Println($X)",
            "package p\nimport \"fmt\"\nfunc f() { fmt.Println(1) }",
        ),
        (
            "repeat",
            "f($A, $A)",
            "package p\nfunc g() { f(1, 1); f(1, 2) }",
        ),
        ("list-empty", "f($$$A)", "package p\nfunc g() { f(); f(1) }"),
        (
            "list-multi",
            "f($$$ARGS)",
            "package p\nfunc g() { f(1, 2, 3) }",
        ),
        ("nested", "f(g($X))", "package p\nfunc h() { f(g(1)) }"),
        ("comment", "f($A)", "package p\nfunc g() { f(/*c*/1) }"),
        ("string", "f($S)", "package p\nfunc g() { f(\"hi\") }"),
        ("number", "$N + 1", "package p\nfunc g() { _ = 2 + 1 }"),
        ("binop", "$A + $B", "package p\nfunc g() { _ = a + b }"),
        (
            "return",
            "return $X",
            "package p\nfunc f() int { return 1 }",
        ),
        ("selector", "$O.$A", "package p\nfunc g() { _ = a.b }"),
        ("go-stmt", "go $F($$$A)", "package p\nfunc g() { go f(1) }"),
        (
            "defer",
            "defer $F($$$A)",
            "package p\nfunc g() { defer f(1) }",
        ),
        (
            "struct-lit",
            "$T{$$$F}",
            "package p\nfunc g() { _ = T{A: 1} }",
        ),
        (
            "type-struct",
            "type $T struct { $$$F }",
            "package p\ntype T struct { A int }",
        ),
        (
            "interface",
            "type $T interface { $$$M }",
            "package p\ntype T interface { M() }",
        ),
        (
            "if",
            "if $C { $$$B }",
            "package p\nfunc g() { if x { y() } }",
        ),
        (
            "for-range",
            "for $A := range $B { $$$C }",
            "package p\nfunc g() { for i := range xs { f(i) } }",
        ),
        (
            "switch",
            "switch $X { $$$B }",
            "package p\nfunc g() { switch x { case 1: } }",
        ),
        (
            "select",
            "select { $$$B }",
            "package p\nfunc g() { select { case <-c: } }",
        ),
        (
            "go-func",
            "func $F($$$A) { $$$B }",
            "package p\nfunc f(a int) { _ = a }",
        ),
        (
            "method",
            "func ($R $T) $M($$$A) { $$$B }",
            "package p\ntype T struct{}\nfunc (r T) M(a int) { _ = a }",
        ),
        ("assign", "$A = $B", "package p\nfunc g() { a = 1 }"),
        ("short-decl", "$A := $B", "package p\nfunc g() { a := 1 }"),
        ("slice", "$A[$B:$C]", "package p\nfunc g() { _ = a[1:2] }"),
        (
            "map-lit",
            "map[$K]$V{$$$E}",
            "package p\nfunc g() { _ = map[string]int{\"a\": 1} }",
        ),
        (
            "chan",
            "make(chan $T)",
            "package p\nfunc g() { _ = make(chan int) }",
        ),
        (
            "go-routine-lit",
            "go func() { $$$B }()",
            "package p\nfunc g() { go func() { f() }() }",
        ),
        ("broken", "f($X)", "package p\nfunc g() { f(1\n}"),
        (
            "nested-list",
            "f(g($$$A))",
            "package p\nfunc h() { f(g(1, 2)) }",
        ),
        ("anon", "f($_)", "package p\nfunc g() { f(1) }"),
        ("multi", "f($X)", "package p\nfunc g() { f(1); f(2); f(3) }"),
        (
            "call-sel",
            "$O.f($$$A)",
            "package p\nfunc g() { obj.f(1, 2) }",
        ),
        ("pointer", "&$X", "package p\nfunc g() { _ = &x }"),
        ("star", "*$X", "package p\nfunc g() { _ = *p }"),
        (
            "type-assert",
            "$X.($T)",
            "package p\nfunc g() { _, _ = x.(T) }",
        ),
        (
            "composite",
            "$T{ $F: $V }",
            "package p\nfunc g() { _ = T{F: 1} }",
        ),
        ("import", "import $P", "package p\nimport \"fmt\"\n"),
        ("const", "const $N = $V", "package p\nconst N = 1"),
        ("var", "var $N $T", "package p\nvar N int"),
    ];
    for (id, pat, src) in hand {
        push(out, l, id, pat, src, true);
    }
    pad_to(out, l, 40, true, |i| {
        ("f($X)".into(), format!("package p\nfunc g() {{ f({i}) }}"))
    });
}

fn rust_cases(out: &mut Vec<Case>) {
    let l = OcLang::Rust;
    // Intentional / OursBug rows stay expect_agree=true and are skipped via KNOWN_DIVERGENCES.
    let hand: &[(&str, &str, &str, bool)] = &[
        ("call", "println!($X)", "fn f() { println!(1); }", true),
        ("repeat", "f($A, $A)", "fn g() { f(1, 1); f(1, 2); }", true), // known: repeat-metavar
        ("list-empty", "f($$$A)", "fn g() { f(); f(1); }", true),
        ("list-multi", "f($$$ARGS)", "fn g() { f(1, 2, 3); }", true),
        ("nested", "f(g($X))", "fn h() { f(g(1)); }", true),
        ("comment", "f($A)", "fn g() { f(/*c*/1); }", true),
        ("string", "f($S)", "fn g() { f(\"hi\"); }", true),
        ("number", "$N + 1", "fn g() { let _ = 2 + 1; }", true),
        ("binop", "$A + $B", "fn g() { let _ = a + b; }", true),
        ("return", "return $X;", "fn f() -> i32 { return 1; }", true),
        ("method", "$O.$M($$$A)", "fn g() { obj.m(1, 2); }", true),
        ("closure", "|$A| $B", "fn g() { let f = |x| x + 1; }", true),
        ("struct", "struct $S { $$$F }", "struct S { a: i32 }", true),
        (
            "impl-body",
            "impl $T { $$$B }",
            "impl Foo { fn bar(&self) {} }",
            true,
        ),
        (
            "trait-body",
            "trait $T { $$$B }",
            "trait T { fn m(&self); }",
            true,
        ),
        ("enum", "enum $E { $$$V }", "enum E { A, B }", true),
        (
            "match",
            "match $X { $$$A }",
            "fn g() { match x { 1 => {} _ => {} } }",
            true,
        ),
        (
            "if-let",
            "if let $P = $X { $$$B }",
            "fn g() { if let Some(a) = x { f(a); } }",
            true,
        ),
        (
            "macro-call",
            "vec![$$$E]",
            "fn g() { let _ = vec![1, 2]; }",
            true,
        ),
        (
            "macro-rules",
            "macro_rules! $N { $$$B }",
            "macro_rules! m { () => {}; }",
            true,
        ),
        (
            "async",
            "async fn $F($$$A) { $$$B }",
            "async fn f(a: i32) { let _ = a; }",
            true,
        ),
        ("await", "$X.await", "async fn f() { x.await; }", true),
        ("use", "use $P;", "use std::io;", true),
        ("let", "let $A = $B;", "fn g() { let a = 1; }", true),
        (
            "let-mut",
            "let mut $A = $B;",
            "fn g() { let mut a = 1; }",
            true,
        ),
        ("ref", "&$X", "fn g() { let _ = &x; }", true),
        (
            "turbofish",
            "$F::<$T>($$$A)",
            "fn g() { f::<i32>(1); }",
            true,
        ),
        ("path", "$A::$B", "fn g() { let _ = a::b; }", true),
        ("broken", "f($X)", "fn g() { f(1\n}", true), // known: rust-error-broken
        ("nested-list", "f(g($$$A))", "fn h() { f(g(1, 2)); }", true),
        ("anon", "f($_)", "fn g() { f(1); }", true),
        ("multi", "f($X)", "fn g() { f(1); f(2); f(3); }", true),
        (
            "for",
            "for $A in $B { $$$C }",
            "fn g() { for x in xs { f(x); } }",
            true,
        ),
        (
            "while",
            "while $C { $$$B }",
            "fn g() { while x { f(); } }",
            true,
        ),
        ("loop", "loop { $$$B }", "fn g() { loop { break; } }", true),
        (
            "unsafe",
            "unsafe { $$$B }",
            "fn g() { unsafe { f(); } }",
            true,
        ),
        ("const", "const $N: $T = $V;", "const N: i32 = 1;", true),
        ("type-alias", "type $N = $T;", "type N = i32;", true),
        (
            "attr",
            "#[$A] fn $F() { $$$B }",
            "#[inline] fn f() {}",
            true,
        ),
        (
            "where",
            "fn $F<T>() where T: $B { $$$C }",
            "fn f<T>() where T: Clone { }",
            true,
        ),
    ];
    for (id, pat, src, agree) in hand {
        push(out, l, id, pat, src, *agree);
    }
    pad_to(out, l, 40, true, |i| {
        ("f($X)".into(), format!("fn g() {{ f({i}); }}"))
    });
}

/// All corpus cases (≥40 per language).
pub fn corpus_all() -> Vec<Case> {
    let mut out = Vec::with_capacity(220);
    js_cases(&mut out);
    ts_cases(&mut out);
    py_cases(&mut out);
    go_cases(&mut out);
    rust_cases(&mut out);
    out
}

/// Per-language counts for the status comment.
pub fn corpus_counts() -> Vec<(OcLang, usize)> {
    let all = corpus_all();
    [
        OcLang::JavaScript,
        OcLang::TypeScript,
        OcLang::Python,
        OcLang::Go,
        OcLang::Rust,
    ]
    .into_iter()
    .map(|l| (l, all.iter().filter(|c| c.lang.0 == l).count()))
    .collect()
}
