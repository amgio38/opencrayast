//! Spec for ISSUE-PATTERN-RULES. Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{
    CompiledRule, Pattern, Rule, RuleOperand, SearchBudget, VarConstraint, search,
};
use std::time::Duration;

const JS: Language = Language::JavaScript;

fn run(pat: &str, rule: &Rule, src: &str) -> Vec<String> {
    let p = Pattern::compile(JS, pat).unwrap();
    let r = CompiledRule::compile(JS, rule).unwrap();
    let parsed = parse(
        JS,
        src,
        &ParseBudget {
            max_bytes: 1 << 24,
            timeout: Duration::from_secs(10),
            max_depth: 8192,
            max_nodes: 10_000_000,
        },
    )
    .unwrap();
    search(&parsed, src, &p, Some(&r), &SearchBudget::default())
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.text)
        .collect()
}

fn kind(k: &str) -> Rule {
    Rule {
        kind: Some(k.to_string()),
        ..Default::default()
    }
}
fn rule(r: Rule) -> Option<Box<RuleOperand>> {
    Some(Box::new(RuleOperand::Rule(Box::new(r))))
}
fn pat(p: &str) -> Option<Box<RuleOperand>> {
    Some(Box::new(RuleOperand::Pattern(p.to_string())))
}

const SRC: &str = "function a() { log(1); }\nlog(2);\nfunction test_b() { log(3); }\n";

#[test]
fn kind_constrains_the_matched_node() {
    assert_eq!(
        run("log($$$A)", &kind("call_expression"), SRC),
        ["log(1)", "log(2)", "log(3)"]
    );
    assert!(run("log($$$A)", &kind("identifier"), SRC).is_empty());
}

#[test]
fn inside_requires_a_proper_ancestor_that_satisfies_the_operand() {
    let r = Rule {
        inside: rule(kind("function_declaration")),
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &r, SRC), ["log(1)", "log(3)"]);
    // a pattern operand works too: inside any call of `wrap(...)`
    let r = Rule {
        inside: pat("wrap($$$X)"),
        ..Default::default()
    };
    assert_eq!(
        run("log($$$A)", &r, "wrap(log(1)); log(2); wrap(() => log(3));"),
        ["log(1)", "log(3)"]
    );
}

#[test]
fn not_negates_its_operand() {
    let outside = Rule {
        not: rule(Rule {
            inside: rule(kind("function_declaration")),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &outside, SRC), ["log(2)"]);
}

#[test]
fn has_requires_a_proper_descendant_that_satisfies_the_operand() {
    let r = Rule {
        has: pat("log($$$A)"),
        ..Default::default()
    };
    assert_eq!(
        run(
            "function $NAME($$$P) { $$$B }",
            &r,
            "function a() { log(1); }\nfunction b() { other(); }\n"
        ),
        ["function a() { log(1); }"]
    );
    // a node is not its own descendant
    let own = Rule {
        has: pat("log($$$A)"),
        ..Default::default()
    };
    assert!(run("log($$$A)", &own, "log(1);").is_empty());
}

#[test]
fn all_any_and_nesting() {
    let r = Rule {
        all: vec![
            kind("call_expression"),
            Rule {
                not: rule(Rule {
                    inside: rule(kind("function_declaration")),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &r, SRC), ["log(2)"]);
    let any = Rule {
        any: vec![kind("string"), kind("call_expression")],
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &any, SRC).len(), 3);
    let none = Rule {
        any: vec![kind("string"), kind("number")],
        ..Default::default()
    };
    assert!(run("log($$$A)", &none, SRC).is_empty());
    assert_eq!(
        run(
            "log($$$A)",
            &Rule {
                any: vec![],
                ..Default::default()
            },
            SRC
        )
        .len(),
        3,
        "an empty `any` is no constraint"
    );
}

#[test]
fn where_constrains_captures_by_regex_and_kind() {
    let src = "function test_a() {}\nfunction b() {}\nfunction test_c(x) { y(); }\n";
    let starts = Rule {
        where_: vec![(
            "$NAME".into(),
            VarConstraint {
                regex: Some("^test_".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert_eq!(
        run("function $NAME($$$P) { $$$B }", &starts, src),
        ["function test_a() {}", "function test_c(x) { y(); }"]
    );
    let ident = Rule {
        where_: vec![(
            "$NAME".into(),
            VarConstraint {
                regex: None,
                kind: Some("identifier".into()),
            },
        )],
        ..Default::default()
    };
    assert_eq!(run("function $NAME($$$P) { $$$B }", &ident, src).len(), 3);
    let number = Rule {
        where_: vec![(
            "$NAME".into(),
            VarConstraint {
                regex: None,
                kind: Some("number".into()),
            },
        )],
        ..Default::default()
    };
    assert!(run("function $NAME($$$P) { $$$B }", &number, src).is_empty());
    // a regex is unanchored unless it anchors itself, and runs over a list capture's whole text
    let any_x = Rule {
        where_: vec![(
            "$P".into(),
            VarConstraint {
                regex: Some("x".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert_eq!(
        run("function $NAME($$$P) { $$$B }", &any_x, src),
        ["function test_c(x) { y(); }"]
    );
}

#[test]
fn a_rule_with_no_keys_is_no_constraint() {
    assert_eq!(run("log($$$A)", &Rule::default(), SRC).len(), 3);
}

fn compile_err(r: &Rule) -> opencrayast_query::pattern::PatternError {
    CompiledRule::compile(JS, r).expect_err("must be invalid")
}

#[test]
fn invalid_rules_are_refused_with_a_suggestion() {
    let e = compile_err(&kind("no_such_kind"));
    assert!(
        e.message.contains("no_such_kind") && !e.suggestion.is_empty(),
        "{e:?}"
    );
    // a close misspelling suggests the real name
    let e = compile_err(&kind("call_expresion"));
    assert!(e.suggestion.contains("call_expression"), "{e:?}");
    assert!(
        CompiledRule::compile(
            JS,
            &Rule {
                inside: pat("function ("),
                ..Default::default()
            }
        )
        .is_err(),
        "operand pattern must compile"
    );
    let bad_regex = Rule {
        where_: vec![(
            "$X".into(),
            VarConstraint {
                regex: Some("(".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert!(CompiledRule::compile(JS, &bad_regex).is_err());
    let long = Rule {
        where_: vec![(
            "$X".into(),
            VarConstraint {
                regex: Some("a".repeat(2000)),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert!(
        CompiledRule::compile(JS, &long).is_err(),
        "regex longer than 1024 bytes"
    );
    let no_dollar = Rule {
        where_: vec![("X".into(), VarConstraint::default())],
        ..Default::default()
    };
    assert!(
        CompiledRule::compile(JS, &no_dollar).is_err(),
        "where keys are `$NAME`"
    );
    let mut deep = kind("call_expression");
    for _ in 0..9 {
        deep = Rule {
            not: rule(deep),
            ..Default::default()
        };
    }
    assert!(
        CompiledRule::compile(JS, &deep).is_err(),
        "nesting deeper than 8"
    );
    let wide = Rule {
        all: (0..70).map(|_| kind("call_expression")).collect(),
        ..Default::default()
    };
    assert!(
        CompiledRule::compile(JS, &wide).is_err(),
        "more than 64 rule nodes"
    );
}

#[test]
fn backtracking_regex_syntax_is_refused_not_executed() {
    // look-around and back-references do not exist in the linear-time engine
    for re in ["(?=a)a", "(a)\\1", "(?<!x)y"] {
        let r = Rule {
            where_: vec![(
                "$X".into(),
                VarConstraint {
                    regex: Some(re.into()),
                    kind: None,
                },
            )],
            ..Default::default()
        };
        assert!(CompiledRule::compile(JS, &r).is_err(), "{re}");
    }
    // a classic catastrophic pattern is fine here because it cannot backtrack catastrophically
    let r = Rule {
        where_: vec![(
            "$X".into(),
            VarConstraint {
                regex: Some("(a+)+$".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    let src = format!("foo({}b);", "a".repeat(5000));
    let t = std::time::Instant::now();
    assert!(run("foo($X)", &r, &src).is_empty());
    assert!(
        t.elapsed() < Duration::from_secs(10),
        "took {:?}",
        t.elapsed()
    );
}
