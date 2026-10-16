//! Spec for edit behaviour that the tool descriptions promise and that a reviewer relies on:
//! undo for as long as the journal is kept, `ast_plan_show` after an apply, `replace_body` found
//! in the syntax tree (and in Python), and the preview saying when a rewrite differs from a search.
//!
//! Every assertion goes through the public handlers and compares the string a client receives.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_edit::{Clock, JournalStore, PlanStore};
use opencrayast_tools::context::{Mode, ToolContext};
use opencrayast_tools::{
    ApplyArgs, EditTools, PlanListArgs, PlanShowArgs, PreviewArgs, UndoArgs, ast_edit_apply,
    ast_edit_preview, ast_plan_list, ast_plan_show, ast_undo,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const WS: &str = "w-00112233445566778899aabbccddeeff";

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_secs(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A write-mode workspace with both stores and a clock the test can move.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: PathBuf,
    tools: ToolContext,
    plans: PlanStore,
    journals: JournalStore,
    clock: Arc<FakeClock>,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        let state = dir.path().join("state");
        let limits = Limits::default();
        let clock = Arc::new(FakeClock(AtomicU64::new(1_000_000)));
        let plans = PlanStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
        let journals = JournalStore::open(&state, WS, limits.clone(), clock.clone()).unwrap();
        let write = opencrayast_core::config::Settings::parse("[policy]\nallow_write = true\n")
            .expect("the fixture's own configuration parses")
            .write_permission()
            .map(opencrayast_tools::WriteCap::mint);
        Fx {
            tools: ToolContext {
                boundary: Boundary::new(BoundaryConfig::new(root.clone(), limits.clone())).unwrap(),
                limits,
                mode: Mode::Write,
                write,
                version: "0.20261002.1".into(),
                workspace_id: WS.into(),
                respect_gitignore: true,
                extra_ignore: Vec::new(),
                config_source: Default::default(),
            },
            plans,
            journals,
            clock,
            _dir: dir,
            root,
            state,
        }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn edit(&self) -> EditTools<'_> {
        EditTools {
            tools: &self.tools,
            plans: &self.plans,
            journals: &self.journals,
            state_dir: &self.state,
            lock_timeout: std::time::Duration::from_millis(200),
        }
    }

    fn preview(&self, args: PreviewArgs) -> Result<String, opencrayast_core::error::ToolError> {
        ast_edit_preview(&self.edit(), &args)
    }

    /// Preview and return the plan id (the second token of the first line).
    fn plan(&self, args: PreviewArgs) -> String {
        let out = self.preview(args).unwrap();
        out.split_whitespace().nth(1).unwrap().to_string()
    }

    fn apply(&self, id: &str) -> String {
        ast_edit_apply(
            &self.edit(),
            &ApplyArgs {
                plan_id: id.to_string(),
            },
        )
        .unwrap()
    }

    fn show(&self, id: &str) -> String {
        ast_plan_show(
            &self.edit(),
            &PlanShowArgs {
                plan_id: id.to_string(),
                ..PlanShowArgs::default()
            },
        )
        .unwrap()
    }
}

fn rewrite(language: &str, path: &str, pattern: &str, replacement: &str) -> PreviewArgs {
    PreviewArgs {
        kind: "rewrite".into(),
        language: Some(language.into()),
        paths: Some(vec![path.into()]),
        pattern: Some(pattern.into()),
        replacement: Some(replacement.into()),
        ..PreviewArgs::default()
    }
}

fn replace_body(path: &str, symbol: &str, text: &str) -> PreviewArgs {
    PreviewArgs {
        kind: "symbol".into(),
        path: Some(path.into()),
        symbol: Some(symbol.into()),
        operation: Some("replace_body".into()),
        text: Some(text.into()),
        ..PreviewArgs::default()
    }
}

// ---- undo lasts as long as the journal ---------------------------------------------------------

/// The apply output promises an undo window of `journal_retention_days`. The plan store drops the
/// plan after `plan_ttl_minutes`, so an undo that needed the plan store failed after a quarter of
/// an hour; the journal now carries the plan and the promise holds.
#[test]
fn an_undo_days_after_the_plan_expired_restores_the_original() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let id = fx.plan(rewrite("typescript", "src/a.ts", "log($$$A)", "log2($$$A)"));
    let applied = fx.apply(&id);
    assert!(applied.contains("(kept 7 days)"), "{applied}");
    assert_eq!(fx.read("src/a.ts"), "log2(1);\n");

    // Two days later, with the plan store swept: the plan is gone from it.
    fx.clock.0.fetch_add(2 * 24 * 3600, Ordering::SeqCst);
    fx.plans.sweep().unwrap();
    let listed = ast_plan_list(&fx.edit(), &PlanListArgs { limit: Some(20) }).unwrap();
    assert!(
        !listed.contains(&id),
        "precondition: the plan has left the store\n{listed}"
    );

    let undone = ast_undo(
        &fx.edit(),
        &UndoArgs {
            plan_id: id.clone(),
        },
    )
    .unwrap_or_else(|e| panic!("undo must still work from the journal: {e}"));
    assert!(undone.contains(&id), "{undone}");
    assert_eq!(fx.read("src/a.ts"), "log(1);\n", "the original is back");
}

// ---- ast_plan_show after an apply ---------------------------------------------------------------

/// A same-length edit used to be laid over its own result after the apply, so the diff showed the
/// new text on both sides. The original now comes from the journal.
#[test]
fn plan_show_after_an_apply_still_shows_the_original_on_the_removed_side() {
    let fx = Fx::new();
    fx.write("src/a.ts", "fa(1);\n");
    let id = fx.plan(rewrite("typescript", "src/a.ts", "fa($$$A)", "fb($$$A)"));
    let before = fx.show(&id);
    fx.apply(&id);
    let after = fx.show(&id);
    assert!(
        after.contains("-fa(1);"),
        "removed side is the original:\n{after}"
    );
    assert!(
        after.contains("+fb(1);"),
        "added side is the result:\n{after}"
    );
    assert!(
        !after.contains("-fb(1);"),
        "the result is never shown as removed:\n{after}"
    );
    assert_eq!(
        before, after,
        "the stored plan renders the same before and after the apply"
    );
}

/// A different-length edit used to answer `[no diff …: StalePlan]` once applied.
#[test]
fn plan_show_after_an_apply_of_a_longer_edit_renders_the_diff() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let id = fx.plan(rewrite(
        "typescript",
        "src/a.ts",
        "log($$$A)",
        "logger.debug($$$A)",
    ));
    fx.apply(&id);
    let after = fx.show(&id);
    assert!(!after.contains("no diff"), "{after}");
    assert!(
        after.contains("-log(1);") && after.contains("+logger.debug(1);"),
        "{after}"
    );
}

/// A file someone else changed is neither the plan's input nor its output: no diff can be laid
/// over it, and the answer says so instead of rendering one.
#[test]
fn plan_show_of_a_file_changed_since_the_preview_shows_no_diff_for_it() {
    let fx = Fx::new();
    fx.write("src/a.ts", "fa(1);\n");
    let id = fx.plan(rewrite("typescript", "src/a.ts", "fa($$$A)", "fb($$$A)"));
    fx.write("src/a.ts", "fz(1);\n");
    let shown = fx.show(&id);
    assert!(
        shown.contains("[no diff for src/a.ts: StalePlan]"),
        "{shown}"
    );
    assert!(!shown.contains("-fz(1);"), "{shown}");
}

// ---- replace_body comes from the syntax tree ----------------------------------------------------

/// The first `{` of this function belongs to a destructuring parameter. Scanning text picked it
/// and the preview failed the syntax gate; the tree names the real body.
#[test]
fn replace_body_ignores_braces_in_the_signature() {
    let fx = Fx::new();
    fx.write(
        "src/a.ts",
        "function f({ a, b }: { a: number; b: number }) {\n  return a + b;\n}\n",
    );
    let id = fx.plan(replace_body("src/a.ts", "f", "{\n  return a * b;\n}"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.ts"),
        "function f({ a, b }: { a: number; b: number }) {\n  return a * b;\n}\n"
    );
}

/// An attribute is part of a Rust symbol's extent; the body is still the function's.
#[test]
fn replace_body_of_an_attributed_rust_function() {
    let fx = Fx::new();
    fx.write(
        "src/lib.rs",
        "#[inline]\nfn f() -> i32 { 1 }\n\nfn g() {}\n",
    );
    let id = fx.plan(replace_body("src/lib.rs", "f", "{ 2 }"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/lib.rs"),
        "#[inline]\nfn f() -> i32 { 2 }\n\nfn g() {}\n"
    );
}

/// A Go type declaration has no `body` field; its braces are still the body.
#[test]
fn replace_body_of_a_go_struct_type() {
    let fx = Fx::new();
    fx.write("src/a.go", "package a\n\ntype T struct {\n\tA int\n}\n");
    let id = fx.plan(replace_body("src/a.go", "T", "{\n\tB string\n}"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.go"),
        "package a\n\ntype T struct {\n\tB string\n}\n"
    );
}

/// An arrow function with an expression body has no braced body to replace.
#[test]
fn replace_body_refuses_an_expression_bodied_arrow_function() {
    let fx = Fx::new();
    fx.write("src/a.ts", "const f = (x: number) => x + 1;\n");
    let err = fx
        .preview(replace_body("src/a.ts", "f", "{ return x; }"))
        .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::InvalidArgs, "{err}");
    assert!(err.to_string().contains("no body between braces"), "{err}");
}

/// Python bodies have no braces. The text is the body's statements, written at any indentation,
/// and lands at the body's indentation.
#[test]
fn replace_body_in_python_reindents_the_statements_to_the_body() {
    let fx = Fx::new();
    fx.write(
        "src/a.py",
        "class A:\n    def m(self, x):\n        \"\"\"Doc.\"\"\"\n        return x\n\n\ndef g():\n    pass\n",
    );
    let id = fx.plan(replace_body(
        "src/a.py",
        "A.m",
        "    y = x + 1\n\n    return y\n",
    ));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.py"),
        "class A:\n    def m(self, x):\n        y = x + 1\n\n        return y\n\n\ndef g():\n    pass\n"
    );
}

/// Python text written without indentation works the same way.
#[test]
fn replace_body_in_python_accepts_unindented_text() {
    let fx = Fx::new();
    fx.write("src/a.py", "def f():\n    return 1\n");
    let id = fx.plan(replace_body("src/a.py", "f", "a = 2\nreturn a"));
    fx.apply(&id);
    assert_eq!(fx.read("src/a.py"), "def f():\n    a = 2\n    return a\n");
}

// ---- the preview says where a rewrite differs from a search -------------------------------------

#[test]
fn a_rewrite_with_nested_matches_says_the_inner_ones_were_left_unchanged() {
    let fx = Fx::new();
    fx.write("src/a.ts", "h(h(1));\n");
    let out = fx
        .preview(rewrite("typescript", "src/a.ts", "h($X)", "k($X)"))
        .unwrap();
    assert!(
        out.contains(
            "nested: 1 match inside a larger match was left unchanged (only the outermost match is rewritten)"
        ),
        "{out}"
    );
    assert!(!out.contains("parenthesised:"), "{out}");
}

#[test]
fn a_rewrite_that_adds_parentheses_says_so() {
    let fx = Fx::new();
    fx.write("src/a.ts", "g(a + b);\ng(c + d);\n");
    let out = fx
        .preview(rewrite("typescript", "src/a.ts", "g($X)", "$X * 2"))
        .unwrap();
    assert!(
        out.contains(
            "parenthesised: a captured expression was wrapped in parentheses in 2 matches to keep its meaning"
        ),
        "{out}"
    );
    assert!(out.contains("+(a + b) * 2;"), "{out}");
    assert!(!out.contains("nested:"), "{out}");
}

/// The refusal names only what a caller can do: no surface lets a caller allow comment loss.
#[test]
fn a_comment_loss_refusal_does_not_offer_an_option_that_does_not_exist() {
    let fx = Fx::new();
    fx.write("src/a.ts", "f(a, /* keep */ b);\n");
    let err = fx
        .preview(rewrite("typescript", "src/a.ts", "f($A, $B)", "f($B, $A)"))
        .unwrap_err();
    assert_eq!(err.code, opencrayast_core::ErrorCode::CommentLoss, "{err}");
    let text = err.to_string();
    assert!(!text.contains("comment loss allowed"), "{text}");
    assert!(
        text.contains("capture the region around the comment"),
        "{text}"
    );
}

/// The preview's CLI hint is a command that works: without `--write` it answers
/// `write_disabled`.
#[test]
fn the_preview_hint_for_the_cli_includes_write() {
    let fx = Fx::new();
    fx.write("src/a.ts", "log(1);\n");
    let out = fx
        .preview(rewrite("typescript", "src/a.ts", "log($$$A)", "log2($$$A)"))
        .unwrap();
    assert!(out.contains("`opencrayast --write edit apply p-"), "{out}");
}

// ---- Python bodies: placement and indentation are checked, not left to the syntax gate ----------

/// A body on the header's line becomes a block below it. Continuing the header's line put every
/// line after the first outside the function, and the syntax gate could not see it.
#[test]
fn replace_body_of_a_one_line_python_def_writes_a_block_below_the_header() {
    let fx = Fx::new();
    fx.write(
        "src/a.py",
        "def one(): return 1\n\nclass K:\n    def m(self): return 2\n",
    );
    let id = fx.plan(replace_body("src/a.py", "one", "x = 1\nreturn x"));
    fx.apply(&id);
    let id = fx.plan(replace_body("src/a.py", "K.m", "y = 2\nreturn y"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.py"),
        "def one():\n    x = 1\n    return x\n\nclass K:\n    def m(self):\n        y = 2\n        return y\n"
    );
}

/// A first line pasted straight after the colon, followed by indented lines.
#[test]
fn replace_body_in_python_handles_a_first_line_without_indentation() {
    let fx = Fx::new();
    fx.write("src/a.py", "def f(x):\n    return x\n");
    let id = fx.plan(replace_body(
        "src/a.py",
        "f",
        "y = x\n    if y:\n        return y\n    return 0",
    ));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.py"),
        "def f(x):\n    y = x\n    if y:\n        return y\n    return 0\n"
    );
}

/// Lines inside a triple-quoted string are part of its value and are copied unchanged.
#[test]
fn replace_body_in_python_leaves_multi_line_strings_alone() {
    let fx = Fx::new();
    fx.write("src/a.py", "def f():\n    return 1\n");
    let id = fx.plan(replace_body("src/a.py", "f", "s = '''\nraw\n'''\nreturn s"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.py"),
        "def f():\n    s = '''\nraw\n'''\n    return s\n"
    );
}

/// Body text Python would reject is refused at preview, with nothing stored.
#[test]
fn replace_body_in_python_refuses_text_python_would_reject() {
    let fx = Fx::new();
    fx.write("src/a.py", "def h():\n    return 1\n");
    for (text, why) in [
        ("", "empty"),
        ("\n   \n", "blank"),
        ("x = 1\n  y = 2\n    z = 3", "unexpected indent"),
        ("if x:\nreturn 1", "expected an indented block"),
        ("for a in b:", "empty block at the end"),
        ("if a:\n        b\n    c", "unindent that matches no level"),
    ] {
        let err = fx
            .preview(replace_body("src/a.py", "h", text))
            .expect_err(why);
        assert_eq!(
            err.code,
            opencrayast_core::ErrorCode::InvalidArgs,
            "{why}: {err}"
        );
        assert!(
            err.to_string().contains("is not valid Python"),
            "{why}: {err}"
        );
    }
    let listed = ast_plan_list(&fx.edit(), &PlanListArgs { limit: Some(20) }).unwrap();
    assert!(
        listed.starts_with("0 plans"),
        "nothing was stored:\n{listed}"
    );
}

// ---- replace_body only ever replaces the symbol's own body ---------------------------------------

/// A symbol with no body of its own is refused; the body of something nested inside it (a method
/// in an object literal, a lambda) is never replaced in its place.
#[test]
fn replace_body_never_replaces_the_body_of_something_nested_in_the_symbol() {
    let fx = Fx::new();
    fx.write(
        "src/a.ts",
        "const api = {\n  get() { return 1; },\n  put: 2,\n};\n",
    );
    fx.write("src/b.py", "X = lambda: 3\n");
    for (path, symbol) in [("src/a.ts", "api"), ("src/b.py", "X")] {
        let err = fx
            .preview(replace_body(path, symbol, "{ x: 1 }"))
            .expect_err(symbol);
        assert_eq!(
            err.code,
            opencrayast_core::ErrorCode::InvalidArgs,
            "{symbol}: {err}"
        );
    }
    assert_eq!(
        fx.read("src/a.ts"),
        "const api = {\n  get() { return 1; },\n  put: 2,\n};\n"
    );
}

/// A declaration without a body has no braces of its own; braces in its signature are types.
#[test]
fn replace_body_refuses_declarations_whose_only_braces_are_in_the_signature() {
    let fx = Fx::new();
    fx.write(
        "src/a.ts",
        "interface I {\n  m(opts: { a: number }): void;\n}\ndeclare function df(o: { a: number }): void;\n",
    );
    fx.write(
        "src/lib.rs",
        "trait T {\n    fn f(&self) -> [u8; { 3 }];\n}\n",
    );
    for (path, symbol) in [
        ("src/a.ts", "I.m"),
        ("src/a.ts", "df"),
        ("src/lib.rs", "T::f"),
    ] {
        let err = fx
            .preview(replace_body(path, symbol, "{ b: string }"))
            .expect_err(symbol);
        assert_eq!(
            err.code,
            opencrayast_core::ErrorCode::InvalidArgs,
            "{symbol}: {err}"
        );
    }
    // The interface itself still has a body.
    let id = fx.plan(replace_body("src/a.ts", "I", "{\n  n(): void;\n}"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.ts"),
        "interface I {\n  n(): void;\n}\ndeclare function df(o: { a: number }): void;\n"
    );
}

/// The arrow function bound by a `const` is the definition, so its block body is replaced.
#[test]
fn replace_body_of_a_const_bound_arrow_function() {
    let fx = Fx::new();
    fx.write(
        "src/a.ts",
        "export const f = (x: number) => {\n  return x;\n};\n",
    );
    let id = fx.plan(replace_body("src/a.ts", "f", "{\n  return x * 2;\n}"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.ts"),
        "export const f = (x: number) => {\n  return x * 2;\n};\n"
    );
}

/// A Python decorated function: the decorator's lambda is not the body.
#[test]
fn replace_body_of_a_decorated_python_function() {
    let fx = Fx::new();
    fx.write("src/a.py", "@dec(lambda: 0)\ndef f():\n    return 1\n");
    let id = fx.plan(replace_body("src/a.py", "f", "return 2"));
    fx.apply(&id);
    assert_eq!(
        fx.read("src/a.py"),
        "@dec(lambda: 0)\ndef f():\n    return 2\n"
    );
}
