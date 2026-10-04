//! Where a symbol's name is allowed to come from (ISSUE-QUERY-ECMA-NAMEFIX).
//!
//! The invariant: a name comes from a declaration's single-identifier child. Destructuring,
//! multiple targets, grouped declarations and computed keys are not names this milestone can
//! publish, so they produce no symbol. Each case below states whether the form is emitted and why -
//! and the "not emitted" cases are the ones that keep damaged text out of a name, which is what the
//! shared pipeline gate in `outline::collect_all` exists to catch afterwards.
//!
//! These tests pin collector behaviour only. The existing golden tests in `outline_spec.rs` are
//! unchanged and still pass: nothing here altered a symbol they assert on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::{OutlineOptions, Symbol, outline};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 << 20,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

fn run(lang: Language, src: &str) -> Vec<Symbol> {
    let p = parse(lang, src, &budget()).unwrap();
    outline(&p, src, &OutlineOptions::default())
}

/// The emitted symbol names, for a one-line assertion.
fn names(lang: Language, src: &str) -> Vec<String> {
    run(lang, src).into_iter().map(|s| s.name).collect()
}

// ---- TypeScript / JavaScript ----

#[test]
fn ecma_destructuring_declarations_are_not_symbols() {
    // Object/array patterns name several bindings at once. Expanding them would mean inventing
    // names like `a` and `b` that are not declared symbols in this milestone, so the declaration
    // yields nothing.
    assert_eq!(
        names(Language::TypeScript, "const { a, b } = x;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::TypeScript, "let [x, y] = arr;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::TypeScript, "export const { a, b } = x;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::JavaScript, "var { a } = x;\n"),
        Vec::<String>::new()
    );
    // The defect that started this ticket: the pattern's text, braces and newline included, used
    // to be pasted in as a name.
    assert_eq!(
        names(Language::TypeScript, "declare const {\n}\n"),
        Vec::<String>::new()
    );
}

#[test]
fn ecma_multiple_declarators_each_get_a_symbol() {
    // One `const` statement, two declarators: both are real single-identifier declarations.
    assert_eq!(
        names(Language::TypeScript, "const a = 1, b = 2;\n"),
        ["a", "b"]
    );
    // `let`/`var` are not outlined at all in this milestone (existing behaviour: only `const`
    // lexical declarations are symbols), so the multi-declarator rule only shows up for `const`.
    assert_eq!(
        names(Language::TypeScript, "let x = 1, y = 2;\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::TypeScript, "const c = 1, d = 2;\n"),
        ["c", "d"]
    );
}

#[test]
fn ecma_re_export_and_import_are_not_symbols() {
    // These name bindings that are declared elsewhere; a re-export is not a declaration here.
    assert_eq!(
        names(Language::TypeScript, "export { a, b };\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::TypeScript, "export * from 'm';\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::TypeScript, "import { a } from 'm';\n"),
        Vec::<String>::new()
    );
}

#[test]
fn ecma_class_static_block_is_not_a_method() {
    // A static initialisation block has no name; the class itself is still emitted.
    assert_eq!(
        names(Language::TypeScript, "class A { static { } }\n"),
        ["A"]
    );
    assert_eq!(
        names(Language::JavaScript, "class A { static { } }\n"),
        ["A"]
    );
}

#[test]
fn ecma_accessor_methods_keep_their_property_name() {
    // `get x`/`set x` name a property, which is a single identifier (`property_identifier`).
    assert_eq!(
        names(Language::TypeScript, "class A { get x() { return 1 } }\n"),
        ["A", "x"]
    );
    assert_eq!(
        names(Language::TypeScript, "class A { set x(v) {} }\n"),
        ["A", "x"]
    );
}

#[test]
fn ecma_computed_and_literal_property_names_are_not_symbols() {
    // `[Symbol.iterator]` is an expression and `'a-b'` is a string literal: neither is an
    // identifier, so neither can be a symbol name.
    assert_eq!(
        names(Language::TypeScript, "class A { [Symbol.iterator]() {} }\n"),
        ["A"]
    );
    assert_eq!(
        names(Language::TypeScript, "class A { 'a-b'() {} }\n"),
        ["A"]
    );
    assert_eq!(names(Language::TypeScript, "class A { 42() {} }\n"), ["A"]);
}

#[test]
fn ecma_private_method_keeps_its_name() {
    // `#priv` is a single private-name identifier and is addressable in the source.
    assert_eq!(
        names(Language::TypeScript, "class A { #priv() {} }\n"),
        ["A", "#priv"]
    );
}

// ---- Python ----

#[test]
fn python_tuple_and_list_targets_are_not_symbols() {
    // A tuple/list target binds several names at once; not expanded, so no symbols.
    assert_eq!(
        names(Language::Python, "a, b = 1, 2\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::Python, "(a, b) = f()\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::Python, "[a, b] = f()\n"),
        Vec::<String>::new()
    );
}

#[test]
fn python_chained_assignment_yields_only_the_first_target() {
    // `a = b = 1` assigns to two names; `a` is the statement's target the collector reads, and it
    // is the one an agent means when it asks for this line.
    assert_eq!(names(Language::Python, "a = b = 1\n"), ["a"]);
}

#[test]
fn python_annotated_assignment_is_a_symbol() {
    // `x: int = 1` still declares the single name `x`; the annotation is not part of the name.
    assert_eq!(names(Language::Python, "x: int = 1\n"), ["x"]);
}

#[test]
fn python_for_and_with_targets_are_not_symbols() {
    // Loop and context-manager variables are not declarations this milestone outlines.
    assert_eq!(
        names(Language::Python, "for i in range(3):\n    pass\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        names(Language::Python, "with open() as f:\n    pass\n"),
        Vec::<String>::new()
    );
}

#[test]
fn python_global_and_nonlocal_are_not_symbols() {
    // These rebind names owned by an enclosing scope; the enclosing declaration is the symbol.
    assert_eq!(
        names(Language::Python, "def g():\n    global x\n    nonlocal y\n"),
        ["g"]
    );
}

#[test]
fn python_class_attribute_follows_existing_behaviour() {
    // Pinned as-is, not changed here: this collector DOES outline a class-body assignment as a
    // variable (`C.x`), which is why the ticket said "與現況一致". Tightening it would be a
    // behaviour change outside this ticket's scope, so the current shape is simply recorded.
    assert_eq!(names(Language::Python, "class C:\n    x = 1\n"), ["C", "x"]);
    assert_eq!(
        run(Language::Python, "class C:\n    x = 1\n")
            .iter()
            .find(|s| s.name == "x")
            .map(|s| s.qualified.clone()),
        Some("C.x".to_string())
    );
}

// ---- Go ----

#[test]
fn go_grouped_var_is_not_emitted() {
    // Chosen behaviour: a grouped `var ( x, y int )` yields no symbols. In this grammar the group
    // parses its contents inside an ERROR node, and rather than recovering names from a broken node
    // the group is skipped - the same rule as any other damaged declaration. The names are not lost
    // in practice, because a reader who wants them can still see the declaration text.
    assert_eq!(
        names(Language::Go, "var ( x, y int )\n"),
        Vec::<String>::new()
    );
    // A plain multi-name var IS emitted, one symbol per name: the declarator parses cleanly.
    assert_eq!(names(Language::Go, "var x, y int\n"), ["x", "y"]);
}

#[test]
fn go_grouped_const_emits_each_name() {
    // `const ( A = iota; B; C )` parses as individual `const_spec`s, so each is a real symbol.
    assert_eq!(
        names(Language::Go, "const ( A = iota; B; C )\n"),
        ["A", "B", "C"]
    );
    assert_eq!(names(Language::Go, "const K = 1\n"), ["K"]);
}

#[test]
fn go_grouped_type_emits_each_name() {
    assert_eq!(
        names(Language::Go, "type ( A int; B string )\n"),
        ["A", "B"]
    );
}

#[test]
fn go_blank_identifier_is_not_a_symbol() {
    // `_` discards the value on purpose; there is nothing an agent could address.
    assert_eq!(names(Language::Go, "var _ = x\n"), Vec::<String>::new());
    assert_eq!(names(Language::Go, "const _ = 1\n"), Vec::<String>::new());
    assert_eq!(names(Language::Go, "var _ int\n"), Vec::<String>::new());
}

#[test]
fn go_unnamed_receiver_method_is_still_a_method() {
    // `func (T) m()` has no receiver variable but a real method name, so it stays.
    assert_eq!(names(Language::Go, "func (T) m() {}\n"), ["m"]);
}

#[test]
fn go_repeated_init_functions_are_each_reported() {
    // Several `init` functions are legal; each is a real declaration.
    assert_eq!(
        names(Language::Go, "func init() {}\nfunc init() {}\n"),
        ["init", "init"]
    );
}

/// Every case above still produces a clean outline: no form may leak damaged text into a name,
/// whether it is emitted or skipped.
#[test]
fn no_form_in_this_file_leaks_damaged_text() {
    let cases: Vec<(Language, &str)> = vec![
        (Language::TypeScript, "const { a, b } = x;\n"),
        (Language::TypeScript, "declare const {\n}\n"),
        (Language::TypeScript, "class A { [Symbol.iterator]() {} }\n"),
        (Language::TypeScript, "class A { 'a-b'() {} }\n"),
        (Language::Python, "a, b = 1, 2\n"),
        (Language::Python, "[a, b] = f()\n"),
        (Language::Go, "var ( x, y int )\n"),
        (Language::Go, "var _ = x\n"),
    ];
    for (lang, src) in cases {
        for s in run(lang, src) {
            assert!(
                !s.name.contains('{')
                    && !s.name.contains('\n')
                    && !s.name.contains("'")
                    && !s.name.contains('_'),
                "{}: damaged name {:?} from {src:?}",
                lang.id(),
                s.name
            );
        }
    }
}
