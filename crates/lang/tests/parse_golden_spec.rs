//! Golden parse tests per Tier 1 language: skeleton, symbols and syntax-error reporting.
//! Refs: docs/LANGUAGES.md "Tiers" (Tier 1 requires outline query + golden tests) and
//! REQ Y20261002/REQ-LANG-PARSE acceptance criterion 「每個 Tier 1 語言有黃金檔（骨架、符號、
//! 語法錯誤）測試」; TESTING.md PRS-09.
//!
//! Convention follows `crates/query/tests/outline_spec.rs`: the sources live under
//! `tests/fixtures/` and are pulled in with `include_str!`, and the expectations are compared
//! against the stored constant. Line numbers below refer to those fixture files.
//!
//! What is asserted, per language:
//! 1. **Skeleton** — the full named-node tree of the fixture, indented, compared as one string.
//!    This pins the *shape* of the pinned grammar, not just the totals.
//! 2. **Symbols** — the declaration nodes with their `name` field and their line extents.
//! 3. **Syntax errors** — the exact `ERROR` / `MISSING` nodes a broken source produces, plus
//!    the `error_count` the syntax gate compares (docs/LANGUAGES.md "Syntax errors and parse
//!    behaviour", docs/EDIT-MODEL.md "Gates").
//!
//! These are golden values for the pinned grammar versions in `Cargo.lock`. A grammar upgrade is
//! allowed to change them, but only together with the doc notes that describe the behaviour.
//!
//! Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_lang::{Language, ParseBudget, ParsedFile, parse};
use std::time::Duration;

fn budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 4 * 1024 * 1024,
        timeout: Duration::from_secs(5),
        max_depth: 512,
        max_nodes: 2_000_000,
    }
}

/// Walk the whole tree iteratively (never recursively) and hand each node to `f`.
fn walk(parsed: &ParsedFile, mut f: impl FnMut(tree_sitter::Node<'_>, usize)) {
    let mut cursor = parsed.tree.walk();
    loop {
        f(cursor.node(), cursor.depth() as usize);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// The structural outline: every named node, indented by depth. Anonymous tokens (punctuation,
/// keywords) are left out — they are grammar trivia, the structure is what a golden pins.
fn skeleton(parsed: &ParsedFile) -> String {
    let mut lines = Vec::new();
    walk(parsed, |node, depth| {
        if node.is_named() {
            lines.push(format!("{}{}", "  ".repeat(depth), node.kind()));
        }
    });
    lines.join("\n")
}

/// The symbol nodes of the fixture: kind, `name` field (empty when the grammar has none) and the
/// 1-based line extent. This is the parse-layer view of what the outline collectors consume.
fn symbols(parsed: &ParsedFile, source: &str, kinds: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    walk(parsed, |node, _| {
        if node.is_named() && kinds.contains(&node.kind()) {
            let name = node.child_by_field_name("name").map_or(String::new(), |n| {
                source[n.start_byte()..n.end_byte()].to_string()
            });
            found.push(format!(
                "({}, {:?}, {}, {})",
                node.kind(),
                name,
                node.start_position().row + 1,
                node.end_position().row + 1
            ));
        }
    });
    found
}

/// Every `ERROR` / `MISSING` node with its byte extent. `parse` counts each of these once into
/// `error_count`; this is the same walk, so the two can never disagree silently.
fn error_nodes(parsed: &ParsedFile) -> Vec<String> {
    let mut found = Vec::new();
    walk(parsed, |node, _| {
        if node.is_error() || node.is_missing() {
            found.push(format!(
                "{}@{}..{}",
                if node.is_missing() {
                    "MISSING"
                } else {
                    "ERROR"
                },
                node.start_byte(),
                node.end_byte()
            ));
        }
    });
    found
}

// ---------------------------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------------------------

const RUST_SKELETON: &str = include_str!("fixtures/skeleton.rs");
const RUST_BROKEN: &str = include_str!("fixtures/broken.rs");

const RUST_SKELETON_WANT: &str = "\
source_file
  line_comment
    inner_doc_comment_marker
    doc_comment
  line_comment
    inner_doc_comment_marker
    doc_comment
  line_comment
    outer_doc_comment_marker
    doc_comment
  struct_item
    visibility_modifier
    type_identifier
    field_declaration_list
      field_declaration
        visibility_modifier
        field_identifier
        type_identifier
  enum_item
    visibility_modifier
    type_identifier
    enum_variant_list
      enum_variant
        identifier
      enum_variant
        identifier
  trait_item
    visibility_modifier
    type_identifier
    declaration_list
      function_signature_item
        identifier
        parameters
          self_parameter
            self
        primitive_type
  impl_item
    type_identifier
    declaration_list
      function_item
        visibility_modifier
        identifier
        parameters
          parameter
            identifier
            reference_type
              primitive_type
        generic_type
          type_identifier
          type_arguments
            type_identifier
            type_identifier
        block
          macro_invocation
            identifier
            token_tree
  const_item
    identifier
    primitive_type
    integer_literal
  function_item
    visibility_modifier
    identifier
    parameters
    block";

/// Golden: the Rust skeleton. Note `trait_item` holds a `function_signature_item` (a trait method
/// has no body) while `impl_item` holds a `function_item` — the outline collectors rely on this
/// difference to tell a trait method from an implemented one.
#[test]
fn parse_rust_skeleton_golden() {
    let p = parse(Language::Rust, RUST_SKELETON, &budget()).unwrap();
    assert_eq!(
        p.error_count, 0,
        "the clean Rust fixture must parse without errors"
    );
    assert_eq!(p.tree.root_node().kind(), "source_file");
    assert_eq!(p.tree.root_node().end_byte(), RUST_SKELETON.len());
    assert_eq!(skeleton(&p), RUST_SKELETON_WANT);
}

#[test]
fn parse_rust_symbols_golden() {
    let p = parse(Language::Rust, RUST_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            RUST_SKELETON,
            &[
                "struct_item",
                "enum_item",
                "trait_item",
                "impl_item",
                "function_item",
                "const_item",
                "mod_item",
                "type_item",
            ]
        ),
        vec![
            r#"(struct_item, "Config", 5, 7)"#,
            r#"(enum_item, "Error", 9, 12)"#,
            r#"(trait_item, "Shape", 14, 16)"#,
            // `impl_item` has no `name` field in tree-sitter-rust; the implemented type is read
            // off its type children (crates/query/src/outline/rust.rs). The empty name here is the
            // parse-layer fact behind that decision.
            r#"(impl_item, "", 18, 22)"#,
            r#"(function_item, "load", 19, 21)"#,
            r#"(const_item, "MAX", 24, 24)"#,
            r#"(function_item, "main", 26, 26)"#,
        ]
    );
}

#[test]
fn parse_rust_syntax_error_golden() {
    let p = parse(Language::Rust, RUST_BROKEN, &budget()).unwrap();
    // `let x = ;` — the `=` survives, wrapped in an ERROR node, so the surrounding
    // `let_declaration` is still a real (named) node: recovery is local.
    assert_eq!(p.error_count, 1);
    assert_eq!(error_nodes(&p), vec!["ERROR@123..124"]);
}

// ---------------------------------------------------------------------------------------------
// TypeScript
// ---------------------------------------------------------------------------------------------

const TS_SKELETON: &str = include_str!("fixtures/skeleton.ts");
const TS_BROKEN: &str = include_str!("fixtures/broken.ts");

const TS_SKELETON_WANT: &str = "\
program
  comment
  export_statement
    class_declaration
      type_identifier
      class_body
        public_field_definition
          property_identifier
          type_annotation
            predefined_type
        method_definition
          property_identifier
          formal_parameters
            required_parameter
              identifier
              type_annotation
                predefined_type
          type_annotation
            type_identifier
          statement_block
            return_statement
              new_expression
                identifier
                arguments
  export_statement
    interface_declaration
      type_identifier
      interface_body
        method_signature
          property_identifier
          formal_parameters
          type_annotation
            predefined_type
  export_statement
    enum_declaration
      identifier
      enum_body
        property_identifier
  export_statement
    type_alias_declaration
      type_identifier
      union_type
        predefined_type
        predefined_type
  export_statement
    lexical_declaration
      variable_declarator
        identifier
        number
  export_statement
    function_declaration
      identifier
      formal_parameters
      type_annotation
        predefined_type
      statement_block";

/// Golden: the TypeScript skeleton. Note `method_signature` (ambient, no body) in the interface
/// against `method_definition` in the class — the pair the outline collector tells apart.
#[test]
fn parse_typescript_skeleton_golden() {
    let p = parse(Language::TypeScript, TS_SKELETON, &budget()).unwrap();
    assert_eq!(p.error_count, 0);
    assert_eq!(p.tree.root_node().kind(), "program");
    assert_eq!(p.tree.root_node().end_byte(), TS_SKELETON.len());
    assert_eq!(skeleton(&p), TS_SKELETON_WANT);
}

#[test]
fn parse_typescript_symbols_golden() {
    let p = parse(Language::TypeScript, TS_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            TS_SKELETON,
            &[
                "class_declaration",
                "method_definition",
                "method_signature",
                "interface_declaration",
                "enum_declaration",
                "type_alias_declaration",
                "lexical_declaration",
                "function_declaration",
            ]
        ),
        vec![
            r#"(class_declaration, "Config", 2, 8)"#,
            r#"(method_definition, "load", 5, 7)"#,
            r#"(interface_declaration, "Shape", 10, 12)"#,
            // An ambient interface method is a `method_signature` with no body — a different kind
            // from the class method above, which is how the collector tells them apart.
            r#"(method_signature, "area", 11, 11)"#,
            r#"(enum_declaration, "Color", 14, 16)"#,
            r#"(type_alias_declaration, "Id", 18, 18)"#,
            // `lexical_declaration` carries no `name` field; the declarator inside it does.
            r#"(lexical_declaration, "", 20, 20)"#,
            r#"(function_declaration, "main", 22, 22)"#,
        ]
    );
}

/// Golden: a nested `ERROR` is counted *and* its parent is counted, which is why this broken file
/// reports 2 and not 1. The syntax gate compares this number, so it is pinned exactly.
#[test]
fn parse_typescript_syntax_error_golden() {
    let p = parse(Language::TypeScript, TS_BROKEN, &budget()).unwrap();
    assert_eq!(p.error_count, 2);
    assert_eq!(error_nodes(&p), vec!["ERROR@72..108", "ERROR@105..106"]);
}

// ---------------------------------------------------------------------------------------------
// TSX
// ---------------------------------------------------------------------------------------------

const TSX_SKELETON: &str = include_str!("fixtures/skeleton.tsx");
const TSX_BROKEN: &str = include_str!("fixtures/broken.tsx");

const TSX_SKELETON_WANT: &str = "\
program
  export_statement
    lexical_declaration
      variable_declarator
        identifier
        arrow_function
          formal_parameters
          jsx_element
            jsx_opening_element
              identifier
              jsx_attribute
                property_identifier
                string
                  string_fragment
            jsx_text
            jsx_closing_element
              identifier
  export_statement
    function_declaration
      identifier
      formal_parameters
        required_parameter
          object_pattern
            shorthand_property_identifier_pattern
          type_annotation
            object_type
              property_signature
                property_identifier
                type_annotation
                  predefined_type
      statement_block
        return_statement
          jsx_element
            jsx_opening_element
              identifier
            jsx_expression
              identifier
            jsx_closing_element
              identifier
  export_statement
    class_declaration
      type_identifier
      class_body
        method_definition
          property_identifier
          formal_parameters
          statement_block
            return_statement
              jsx_self_closing_element
                identifier";

/// Golden: the TSX skeleton. TSX is a separate grammar constant of the same crate, so it gets its
/// own golden — the JSX nodes below exist in no other fixture.
#[test]
fn parse_tsx_skeleton_golden() {
    let p = parse(Language::Tsx, TSX_SKELETON, &budget()).unwrap();
    assert_eq!(p.error_count, 0);
    assert_eq!(p.tree.root_node().kind(), "program");
    assert_eq!(p.tree.root_node().end_byte(), TSX_SKELETON.len());
    assert_eq!(skeleton(&p), TSX_SKELETON_WANT);
}

#[test]
fn parse_tsx_symbols_golden() {
    let p = parse(Language::Tsx, TSX_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            TSX_SKELETON,
            &[
                "lexical_declaration",
                "function_declaration",
                "class_declaration",
                "method_definition"
            ]
        ),
        vec![
            r#"(lexical_declaration, "", 1, 1)"#,
            r#"(function_declaration, "Badge", 3, 5)"#,
            r#"(class_declaration, "Panel", 7, 11)"#,
            r#"(method_definition, "render", 8, 10)"#,
        ]
    );
}

/// Golden: the broken JSX attribute keeps the whole `jsx_element` and only the `={` pair is an
/// ERROR. A caller must not assume a JSX error swallows the element.
#[test]
fn parse_tsx_syntax_error_golden() {
    let p = parse(Language::Tsx, TSX_BROKEN, &budget()).unwrap();
    assert_eq!(p.error_count, 1);
    assert_eq!(error_nodes(&p), vec!["ERROR@111..113"]);
}

// ---------------------------------------------------------------------------------------------
// JavaScript
// ---------------------------------------------------------------------------------------------

const JS_SKELETON: &str = include_str!("fixtures/skeleton.js");
const JS_BROKEN: &str = include_str!("fixtures/broken.js");

const JS_SKELETON_WANT: &str = "\
program
  comment
  function_declaration
    identifier
    formal_parameters
      identifier
      identifier
    statement_block
      return_statement
        binary_expression
          identifier
          identifier
  class_declaration
    identifier
    class_body
      method_definition
        property_identifier
        formal_parameters
        statement_block
          expression_statement
            augmented_assignment_expression
              member_expression
                this
                property_identifier
              number
  lexical_declaration
    variable_declarator
      identifier
      number
  lexical_declaration
    variable_declarator
      identifier
      arrow_function
        formal_parameters
          identifier
        binary_expression
          identifier
          number
  lexical_declaration
    variable_declarator
      identifier
      jsx_element
        jsx_opening_element
          identifier
        jsx_text
        jsx_closing_element
          identifier";

/// Golden: the JavaScript skeleton. tree-sitter-javascript parses JSX with no separate grammar, so
/// `jsx_element` appears here; tree-sitter-typescript needs the TSX constant for the same text.
#[test]
fn parse_javascript_skeleton_golden() {
    let p = parse(Language::JavaScript, JS_SKELETON, &budget()).unwrap();
    assert_eq!(p.error_count, 0);
    assert_eq!(p.tree.root_node().kind(), "program");
    assert_eq!(p.tree.root_node().end_byte(), JS_SKELETON.len());
    assert_eq!(skeleton(&p), JS_SKELETON_WANT);
}

#[test]
fn parse_javascript_symbols_golden() {
    let p = parse(Language::JavaScript, JS_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            JS_SKELETON,
            &[
                "function_declaration",
                "class_declaration",
                "method_definition",
                "lexical_declaration",
            ]
        ),
        vec![
            r#"(function_declaration, "add", 2, 4)"#,
            r#"(class_declaration, "Counter", 6, 10)"#,
            r#"(method_definition, "inc", 7, 9)"#,
            r#"(lexical_declaration, "", 12, 12)"#,
            r#"(lexical_declaration, "", 14, 14)"#,
            r#"(lexical_declaration, "", 16, 16)"#,
        ]
    );
}

/// Golden: same shape as the TypeScript break — the two grammars recover alike here, but they are
/// pinned separately because nothing guarantees that for every input.
#[test]
fn parse_javascript_syntax_error_golden() {
    let p = parse(Language::JavaScript, JS_BROKEN, &budget()).unwrap();
    assert_eq!(p.error_count, 2);
    assert_eq!(error_nodes(&p), vec!["ERROR@72..101", "ERROR@98..99"]);
}

// ---------------------------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------------------------

const PY_SKELETON: &str = include_str!("fixtures/skeleton.py");
const PY_BROKEN: &str = include_str!("fixtures/broken.py");

const PY_SKELETON_WANT: &str = "\
module
  expression_statement
    string
      string_start
      string_content
      string_end
  expression_statement
    assignment
      identifier
      integer
  function_definition
    identifier
    parameters
      identifier
      identifier
    block
      expression_statement
        string
          string_start
          string_content
          string_end
      return_statement
        binary_operator
          identifier
          identifier
  class_definition
    identifier
    block
      expression_statement
        string
          string_start
          string_content
          string_end
      function_definition
        identifier
        parameters
          identifier
          identifier
        block
          return_statement
            identifier
  function_definition
    identifier
    parameters
    block
      expression_statement
        call
          identifier
          argument_list
            integer
            integer";

/// Golden: the Python skeleton. Note there is no `:` node in the named skeleton — the block
/// colon is an anonymous token, and the body arrives as a `block` child.
#[test]
fn parse_python_skeleton_golden() {
    let p = parse(Language::Python, PY_SKELETON, &budget()).unwrap();
    assert_eq!(p.error_count, 0);
    assert_eq!(p.tree.root_node().kind(), "module");
    assert_eq!(p.tree.root_node().end_byte(), PY_SKELETON.len());
    assert_eq!(skeleton(&p), PY_SKELETON_WANT);
}

#[test]
fn parse_python_symbols_golden() {
    let p = parse(Language::Python, PY_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            PY_SKELETON,
            &["function_definition", "class_definition"]
        ),
        vec![
            r#"(function_definition, "add", 6, 8)"#,
            r#"(class_definition, "Config", 11, 15)"#,
            // A method is a `function_definition` inside the class body, not a distinct kind.
            r#"(function_definition, "load", 14, 15)"#,
            r#"(function_definition, "main", 18, 19)"#,
        ]
    );
}

/// Golden: Python recovers by *inserting* a `)` — a MISSING node with `start == end == 86`. This
/// is the MISSING leg the acceptance criterion asks for: a zero-width node, not an ERROR span.
#[test]
fn parse_python_syntax_error_golden() {
    let p = parse(Language::Python, PY_BROKEN, &budget()).unwrap();
    assert_eq!(p.error_count, 1);
    assert_eq!(error_nodes(&p), vec!["MISSING@86..86"]);
}

// ---------------------------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------------------------

const GO_SKELETON: &str = include_str!("fixtures/skeleton.go");
const GO_BROKEN: &str = include_str!("fixtures/broken.go");

const GO_SKELETON_WANT: &str = "\
source_file
  package_clause
    package_identifier
  comment
  type_declaration
    type_spec
      type_identifier
      struct_type
        field_declaration_list
          field_declaration
            field_identifier
            type_identifier
  comment
  method_declaration
    parameter_list
      parameter_declaration
        identifier
        pointer_type
          type_identifier
    field_identifier
    parameter_list
      parameter_declaration
        identifier
        type_identifier
    type_identifier
    block
      statement_list
        return_statement
          expression_list
            nil
  const_declaration
    const_spec
      identifier
      expression_list
        int_literal
  function_declaration
    identifier
    parameter_list
      parameter_declaration
        identifier
        identifier
        type_identifier
    type_identifier
    block
      statement_list
        return_statement
          expression_list
            binary_expression
              identifier
              identifier
  function_declaration
    identifier
    parameter_list
    block";

/// Golden: the Go skeleton. A method is one `method_declaration` carrying the receiver in a first
/// `parameter_list` and a `field_identifier` for the method name — there is no `method` node kind.
#[test]
fn parse_go_skeleton_golden() {
    let p = parse(Language::Go, GO_SKELETON, &budget()).unwrap();
    assert_eq!(p.error_count, 0);
    assert_eq!(p.tree.root_node().kind(), "source_file");
    assert_eq!(p.tree.root_node().end_byte(), GO_SKELETON.len());
    assert_eq!(skeleton(&p), GO_SKELETON_WANT);
}

#[test]
fn parse_go_symbols_golden() {
    let p = parse(Language::Go, GO_SKELETON, &budget()).unwrap();
    assert_eq!(
        symbols(
            &p,
            GO_SKELETON,
            &[
                "function_declaration",
                "method_declaration",
                "type_declaration",
                "const_declaration",
            ]
        ),
        vec![
            // The Go grammar also has no `name` field on the declaration wrappers; the outline
            // collector reads the `type_spec` / `const_spec` children instead.
            r#"(type_declaration, "", 4, 6)"#,
            // A Go method DOES carry a `name` field pointing at its `field_identifier`, so the
            // generic name lookup finds "Load" — unlike `type_declaration`/`const_declaration`
            // just above, whose wrapper nodes have no name field at all.
            r#"(method_declaration, "Load", 9, 11)"#,
            r#"(const_declaration, "", 13, 13)"#,
            r#"(function_declaration, "add", 15, 17)"#,
            r#"(function_declaration, "main", 19, 19)"#,
        ]
    );
}

/// Golden: Go also recovers by insertion — a MISSING `)` at 88. Pinned because
/// docs/LANGUAGES.md warns that a Go `function_declaration` break can absorb later siblings, so
/// the count must be observed, not assumed.
#[test]
fn parse_go_syntax_error_golden() {
    let p = parse(Language::Go, GO_BROKEN, &budget()).unwrap();
    assert_eq!(p.error_count, 1);
    assert_eq!(error_nodes(&p), vec!["MISSING@88..88"]);
}

// ---------------------------------------------------------------------------------------------
// Cross-language invariants the acceptance criterion rests on
// ---------------------------------------------------------------------------------------------

/// The golden set covers every Tier 1 language the registry declares, and every one of them is
/// Tier 1. If a language is added, this fails until it gets its own three goldens.
#[test]
fn every_tier_1_language_has_a_golden_fixture() {
    const WITH_GOLDEN: [Language; 6] = [
        Language::Rust,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Python,
        Language::Go,
    ];
    let all: Vec<Language> = Language::all().to_vec();
    assert_eq!(
        all,
        WITH_GOLDEN.to_vec(),
        "Language::all() changed; add goldens for it"
    );
    for lang in all {
        assert_eq!(
            lang.tier(),
            1,
            "{} is Tier 1 and must have golden tests",
            lang.id()
        );
    }
}

/// The error count the gate compares is the same walk the golden lists are taken from: a clean
/// fixture has no error node at all, a broken one has exactly as many as it reports. No
/// language may drift between the two numbers.
#[test]
fn error_count_equals_the_number_of_error_nodes_in_every_golden_fixture() {
    let clean: [(Language, &str); 6] = [
        (Language::Rust, RUST_SKELETON),
        (Language::TypeScript, TS_SKELETON),
        (Language::Tsx, TSX_SKELETON),
        (Language::JavaScript, JS_SKELETON),
        (Language::Python, PY_SKELETON),
        (Language::Go, GO_SKELETON),
    ];
    for (lang, src) in clean {
        let p = parse(lang, src, &budget()).unwrap();
        assert_eq!(p.error_count, 0, "{} clean fixture", lang.id());
        assert!(error_nodes(&p).is_empty(), "{} clean fixture", lang.id());
    }

    let broken: [(Language, &str); 6] = [
        (Language::Rust, RUST_BROKEN),
        (Language::TypeScript, TS_BROKEN),
        (Language::Tsx, TSX_BROKEN),
        (Language::JavaScript, JS_BROKEN),
        (Language::Python, PY_BROKEN),
        (Language::Go, GO_BROKEN),
    ];
    for (lang, src) in broken {
        let p = parse(lang, src, &budget()).unwrap();
        let nodes = error_nodes(&p);
        assert_eq!(p.error_count, nodes.len(), "{} broken fixture", lang.id());
        assert!(
            p.error_count > 0,
            "{} broken fixture must report something",
            lang.id()
        );
    }
}

/// Every golden is deterministic: parsing the same bytes twice in one run yields the identical
/// tree, count and skeleton. A golden that drifts between runs is not a golden.
#[test]
fn golden_results_are_deterministic() {
    let cases: [(Language, &str); 12] = [
        (Language::Rust, RUST_SKELETON),
        (Language::Rust, RUST_BROKEN),
        (Language::TypeScript, TS_SKELETON),
        (Language::TypeScript, TS_BROKEN),
        (Language::Tsx, TSX_SKELETON),
        (Language::Tsx, TSX_BROKEN),
        (Language::JavaScript, JS_SKELETON),
        (Language::JavaScript, JS_BROKEN),
        (Language::Python, PY_SKELETON),
        (Language::Python, PY_BROKEN),
        (Language::Go, GO_SKELETON),
        (Language::Go, GO_BROKEN),
    ];
    for (lang, src) in cases {
        let first = parse(lang, src, &budget()).unwrap();
        let second = parse(lang, src, &budget()).unwrap();
        assert_eq!(skeleton(&first), skeleton(&second), "{}", lang.id());
        assert_eq!(error_nodes(&first), error_nodes(&second), "{}", lang.id());
        assert_eq!(first.error_count, second.error_count, "{}", lang.id());
        assert_eq!(first.node_count, second.node_count, "{}", lang.id());
        assert_eq!(first.max_depth, second.max_depth, "{}", lang.id());
        assert_eq!(
            first.tree.root_node().to_sexp(),
            second.tree.root_node().to_sexp()
        );
    }
}
