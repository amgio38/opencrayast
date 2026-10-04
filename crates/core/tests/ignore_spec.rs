//! Spec for ISSUE-CORE-IGNORE (IgnoreRules). Add cases; never weaken these.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use opencrayast_core::walk::IgnoreRules;

fn rules() -> IgnoreRules {
    IgnoreRules::parse(
        "# comment\n\n*.log\n!keep.log\nbuild/\n/root_only.txt\ndocs/*.md\n**/gen/*.rs\nout/**\nsrc/**/tmp\n",
    )
}

#[test]
fn star_matches_at_any_depth_and_negation_wins_when_last() {
    let r = rules();
    assert_eq!(r.matches("a.log", false), Some(true));
    assert_eq!(r.matches("x/y/a.log", false), Some(true));
    assert_eq!(r.matches("keep.log", false), Some(false));
    assert_eq!(r.matches("x/keep.log", false), Some(false));
    assert_eq!(r.matches("a.rs", false), None);
}

#[test]
fn trailing_slash_is_directory_only() {
    let r = rules();
    assert_eq!(r.matches("build", true), Some(true));
    assert_eq!(r.matches("src/build", true), Some(true));
    assert_eq!(r.matches("build", false), None);
}

#[test]
fn leading_slash_and_inner_slash_anchor() {
    let r = rules();
    assert_eq!(r.matches("root_only.txt", false), Some(true));
    assert_eq!(r.matches("sub/root_only.txt", false), None);
    assert_eq!(r.matches("docs/a.md", false), Some(true));
    assert_eq!(r.matches("docs/sub/a.md", false), None);
    assert_eq!(r.matches("other/docs/a.md", false), None);
}

#[test]
fn double_star_forms() {
    let r = rules();
    assert_eq!(r.matches("src/gen/x.rs", false), Some(true));
    assert_eq!(r.matches("gen/x.rs", false), Some(true)); // `**/` matches zero components
    assert_eq!(r.matches("a/b/gen/x.rs", false), Some(true));
    assert_eq!(r.matches("gen/sub/x.rs", false), None); // `*` does not cross `/`
    assert_eq!(r.matches("out/a", false), Some(true)); // trailing `/**`
    assert_eq!(r.matches("out/a/b", false), Some(true));
    assert_eq!(r.matches("src/tmp", true), Some(true)); // `/**/` matches zero components
    assert_eq!(r.matches("src/a/b/tmp", true), Some(true));
}

#[test]
fn question_mark_and_literals_and_garbage_never_panic() {
    let r = IgnoreRules::parse("a?c\n[ab].txt\n\\#x\n   \n!\n/\n**\n****\n");
    assert_eq!(r.matches("abc", false), Some(true));
    assert_eq!(r.matches("a/c", false), None);
    // `[ab]` is a literal here (unsupported class), so it matches the literal name only
    assert_eq!(r.matches("[ab].txt", false), Some(true));
    assert_eq!(r.matches("a.txt", false), None);
    for p in ["", "/", "//", "a//b", "\u{0}", "é/ü", &"a/".repeat(500)] {
        let _ = r.matches(p, false);
        let _ = r.matches(p, true);
    }
}

#[test]
fn pathological_patterns_are_linear_time() {
    let pat = format!("{}b\n", "a*".repeat(60));
    let r = IgnoreRules::parse(&pat);
    let t = std::time::Instant::now();
    let text = "a".repeat(2000);
    assert_eq!(r.matches(&text, false), None);
    assert!(
        t.elapsed().as_secs() < 2,
        "pattern matching must not backtrack exponentially"
    );
}
