//! Extra cases for ISSUE-PATTERN-RULES: the properties a rule must have whatever it says
//! (a rule narrows, never widens), the equivalences that pin the shape of the evaluator down,
//! and the budget that keeps one rule from becoming an expensive search.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::error::ErrorCode;
use opencrayast_lang::{Language, ParseBudget, parse};
use opencrayast_query::pattern::{
    CaptureKind, CompiledRule, Pattern, Rule, RuleOperand, SearchBudget, VarConstraint, search,
};
use std::time::Duration;

const JS: Language = Language::JavaScript;

/// The parser budget the tests use: generous, because the budget under test is the search's.
fn parse_budget() -> ParseBudget {
    ParseBudget {
        max_bytes: 1 << 26,
        timeout: Duration::from_secs(30),
        max_depth: 8192,
        max_nodes: 20_000_000,
    }
}

fn run_with(pat: &str, rule: &Rule, src: &str, budget: &SearchBudget) -> Vec<String> {
    let p = Pattern::compile(JS, pat).unwrap();
    let r = CompiledRule::compile(JS, rule).unwrap();
    let parsed = parse(JS, src, &parse_budget()).unwrap();
    search(&parsed, src, &p, Some(&r), budget)
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.text)
        .collect()
}

fn run(pat: &str, rule: &Rule, src: &str) -> Vec<String> {
    run_with(pat, rule, src, &SearchBudget::default())
}

fn kind_node(kind: &str) -> Rule {
    Rule {
        kind: Some(kind.to_string()),
        ..Default::default()
    }
}

fn nested(r: Rule) -> Option<Box<RuleOperand>> {
    Some(Box::new(RuleOperand::Rule(Box::new(r))))
}

fn pattern_op(text: &str) -> Option<Box<RuleOperand>> {
    Some(Box::new(RuleOperand::Pattern(text.to_string())))
}

const SRC: &str = "function a() { log(1); }\nlog(2);\nfunction test_b() { log(3); }\n";

/// A small deterministic PRNG, so a failure can be reproduced from its seed alone. No
/// dependency, and no dependence on the platform's `HashMap` order.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random rule of depth at most 4, built only from keys that are valid for JavaScript, so the
/// property below is about evaluation and never about compilation.
fn random_rule(rng: &mut Rng, depth: usize) -> Rule {
    let mut rule = Rule::default();
    match rng.below(if depth >= 4 { 5 } else { 8 }) {
        0 => rule.kind = Some("call_expression".to_string()),
        1 => rule.kind = Some("identifier".to_string()),
        2 => {
            rule.inside = nested(kind_node("function_declaration"));
        }
        3 => {
            rule.has = pattern_op("log($$$A)");
        }
        4 => {
            rule.not = nested(Rule {
                inside: nested(kind_node("function_declaration")),
                ..Default::default()
            });
        }
        5 => {
            rule.all = vec![kind_node("call_expression"), random_rule(rng, depth + 1)];
        }
        6 => {
            rule.any = vec![kind_node("string"), random_rule(rng, depth + 1)];
        }
        _ => {
            rule.where_ = vec![(
                "$A".to_string(),
                VarConstraint {
                    regex: Some("^[0-9]".to_string()),
                    kind: None,
                },
            )];
        }
    }
    rule
}

/// The invariant the whole rule language exists for: adding a rule can only remove matches.
/// Checked over 2000 random rule trees against random sources.
#[test]
fn a_random_rule_only_ever_removes_matches() {
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let pattern = "log($$$A)";
    for case in 0..2000 {
        let rule = random_rule(&mut rng, 0);
        let src = random_source(&mut rng);
        let compiled = CompiledRule::compile(JS, &rule).unwrap_or_else(|e| {
            panic!("case {case}: a generated rule must compile: {e:?}\n{rule:?}")
        });

        let p = Pattern::compile(JS, pattern).unwrap();
        let parsed = parse(JS, &src, &parse_budget()).unwrap();
        let without = search(&parsed, &src, &p, None, &SearchBudget::default()).unwrap();
        let with = search(&parsed, &src, &p, Some(&compiled), &SearchBudget::default()).unwrap();

        for m in &with.matches {
            assert!(
                without
                    .matches
                    .iter()
                    .any(|w| { w.start_byte == m.start_byte && w.end_byte == m.end_byte }),
                "case {case}: the rule ADDED a match that was not there without it\n\
                 rule: {rule:?}\nsource: {src:?}\nmatch: {:?}",
                m.text
            );
        }
    }
}

/// A deterministic source with the shapes the rules talk about: calls at the top level and
/// inside functions, with numbers, strings and identifiers as arguments.
fn random_source(rng: &mut Rng) -> String {
    let mut out = String::new();
    for i in 0..1 + rng.below(4) {
        let arg = match rng.below(4) {
            0 => format!("{}", rng.below(1000)),
            1 => format!("\"s{}\"", rng.below(10)),
            2 => format!("v{}", rng.below(10)),
            _ => format!("{}_{}", i, rng.below(100)),
        };
        let call = format!("log({arg});");
        if rng.below(2) == 0 {
            out.push_str(&format!("function f{i}() {{ {call} }}\n"));
        } else {
            out.push_str(&call);
            out.push('\n');
        }
    }
    out
}

/// `inside` and `has` look at PROPER relatives: a node is not inside itself and not its own
/// descendant. This is the case the spec test cannot reach, because there the operand's kind
/// (a function declaration) could never be the node's own kind (a call): an evaluator that
/// wrongly includes the node itself would answer the same. Here the operand's kind IS the
/// node's own kind, so only a correct evaluator keeps the two matches apart.
#[test]
fn a_node_is_neither_inside_itself_nor_its_own_descendant() {
    let src = "log(log(1));\nlog(2);\n";
    let inside_same_kind = Rule {
        inside: nested(kind_node("call_expression")),
        ..Default::default()
    };
    // Only `log(1)` is inside another call expression. The outer `log(log(1))` is a call
    // expression too, and it must not satisfy the rule by being its own ancestor.
    assert_eq!(
        run("log($$$A)", &inside_same_kind, src),
        ["log(1)"],
        "a node is not its own ancestor"
    );

    let has_same_kind = Rule {
        has: nested(kind_node("call_expression")),
        ..Default::default()
    };
    // `log(2)` has no call inside it, and it is not a call inside itself.
    assert_eq!(
        run("log($$$A)", &has_same_kind, src),
        ["log(log(1))"],
        "a node is not its own descendant"
    );
}

/// `not not R` is `R`, `all: [R]` is `R` and `any: [R]` is `R`. Each of these is a way to write
/// the same thing, and an evaluator that gets one of them wrong silently changes what a rule
/// means.
#[test]
fn the_shapes_of_a_rule_are_equivalent_to_the_rule() {
    let base = Rule {
        inside: nested(kind_node("function_declaration")),
        ..Default::default()
    };
    let expected = run("log($$$A)", &base, SRC);

    let negated_twice = Rule {
        not: nested(Rule {
            not: nested(base.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &negated_twice, SRC), expected, "not not R");

    let in_all = Rule {
        all: vec![base.clone()],
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &in_all, SRC), expected, "all [R]");

    let in_any = Rule {
        any: vec![base.clone()],
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &in_any, SRC), expected, "any [R]");

    // An empty `any` is "no constraint", not "nothing holds".
    let empty_any = Rule {
        any: vec![],
        ..Default::default()
    };
    assert_eq!(run("log($$$A)", &empty_any, SRC).len(), 3);
    // An `any` nothing satisfies removes everything, which is the difference the last two lines
    // are about.
    let unsatisfiable = Rule {
        any: vec![kind_node("class_declaration")],
        ..Default::default()
    };
    assert!(run("log($$$A)", &unsatisfiable, SRC).is_empty());
}

/// A rule is evaluated per candidate match, so it is charged to the same budget as the matching
/// itself: an expensive rule on a big file must stop the search rather than run to the end.
#[test]
fn an_expensive_rule_is_cut_off_by_the_step_budget() {
    // A file big enough that walking its nodes costs more steps than allowed. Each call has a
    // `target` call INSIDE it, so the rule below really does hold for every match.
    let mut src = String::new();
    for i in 0..1500 {
        src.push_str(&format!("function f{i}() {{ log(target({i})); }}\n"));
    }
    let rule = Rule {
        // `has` walks every descendant of every candidate and tries the operand at each, which
        // is the most expensive rule there is.
        has: pattern_op("target($$$A)"),
        ..Default::default()
    };
    // `max_matches` defaults to 1000, so a generous run has to raise it as well or it would
    // stop at 1000 matches and this test would be measuring the wrong thing.
    let generous = run_with(
        "log($$$A)",
        &rule,
        &src,
        &SearchBudget {
            max_matches: 5000,
            ..SearchBudget::default()
        },
    );
    assert_eq!(
        generous.len(),
        1500,
        "the rule does hold, with budget to spare"
    );

    let tight = SearchBudget {
        max_steps: 2000,
        ..SearchBudget::default()
    };
    let e = run_with_err("log($$$A)", &rule, &src, &tight);
    assert_eq!(e.code, ErrorCode::BudgetExceeded, "{e}");
    assert!(e.message.contains("steps"), "{e}");
}

fn run_with_err(pat: &str, rule: &Rule, src: &str, budget: &SearchBudget) -> ToolError {
    let p = Pattern::compile(JS, pat).unwrap();
    let r = CompiledRule::compile(JS, rule).unwrap();
    let parsed = parse(JS, src, &parse_budget()).unwrap();
    search(&parsed, src, &p, Some(&r), budget).unwrap_err()
}

use opencrayast_core::ToolError;

/// A capture larger than a megabyte is not worth a regex pass: the constraint is unsatisfied
/// rather than the search spending its budget on it.
#[test]
fn a_regex_over_a_huge_capture_is_unsatisfied_not_run() {
    let huge = "x".repeat(1024 * 1024 + 16);
    let src = format!("log(\"{huge}\");");
    let rule = Rule {
        where_: vec![(
            "$A".to_string(),
            VarConstraint {
                regex: Some("x".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    // The pattern binds the string literal; the rule says its text matches /x/ - and it does,
    // except that the capture is over the limit, so nothing is returned.
    assert!(run("log($A)", &rule, &src).is_empty());

    // The same rule on a capture under the limit does match, so the limit is what did it.
    let small = "x".repeat(1024);
    let src = format!("log(\"{small}\");");
    assert_eq!(run("log($A)", &rule, &src).len(), 1);
}

/// Unicode and CRLF are ordinary bytes to a rule: names, patterns and captures work the same,
/// and a regex sees the text as written.
#[test]
fn unicode_and_crlf_are_ordinary_to_rules() {
    let src = "function \u{65e5}\u{672c}() {\r\n  log(\"\u{1f600}\u{00e9}\");\r\n}\r\nlog(2);\r\n";
    assert_eq!(
        run("log($$$A)", &kind_node("call_expression"), src).len(),
        2
    );
    assert_eq!(
        run("log($$$A)", &kind_node("function_declaration"), src).len(),
        0,
        "the declaration is not a call"
    );
    let named = Rule {
        where_: vec![(
            "$NAME".to_string(),
            VarConstraint {
                regex: Some("^\u{65e5}".into()),
                kind: None,
            },
        )],
        ..Default::default()
    };
    assert_eq!(
        run("function $NAME() { $$$B }", &named, src),
        ["function \u{65e5}\u{672c}() {\r\n  log(\"\u{1f600}\u{00e9}\");\r\n}"],
        "a regex over a unicode name, in a CRLF file"
    );
    // A rule mentioning a variable the pattern never binds cannot widen anything.
    let unbound = Rule {
        where_: vec![(
            "$NEVER_USED".to_string(),
            VarConstraint {
                regex: Some(".*".into()),
                kind: Some("identifier".into()),
            },
        )],
        ..Default::default()
    };
    assert!(
        run("log($$$A)", &unbound, src).is_empty(),
        "a `where` on a variable the pattern does not bind can only narrow"
    );
}

/// A capture's kind is the node it covers: `$NAME` is a node, `$$$NAMES` is several.
#[test]
fn capture_kind_is_available_to_the_rule() {
    let p = Pattern::compile(JS, "log($A)").unwrap();
    let kinds: Vec<CaptureKind> = p.metavars().iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [CaptureKind::One]);

    let p = Pattern::compile(JS, "log($$$A)").unwrap();
    let kinds: Vec<CaptureKind> = p.metavars().iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [CaptureKind::List]);
}
