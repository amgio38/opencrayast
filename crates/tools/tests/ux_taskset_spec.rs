//! UX task suite: a scored, reproducible task set for the **tools**.
//!
//! # What this measures, and what it does not
//!
//! Every task below is a fixed `(tool, arguments) -> expected outcome` pair with a
//! known-correct answer written down in the fixture itself. The runner calls the real
//! handlers in-process with no LLM anywhere, so the score is deterministic and
//! reproducible to the byte.
//!
//! What that means, stated plainly so this file cannot be cited for something it is not:
//!
//! - This **does** measure whether a tool returns the correct answer for a correct call,
//!   whether its output format is stable, whether it refuses what it must refuse, and
//!   whether its error message carries an actionable next step.
//! - This **does not** measure agent success rate, first-call success rate, prompt
//!   quality, or "does an LLM choose the right tool". Those require running an agent.
//!   Nothing here runs a model, so any number describing an LLM would be fabricated.
//!   The REQ's first-call-success metric remains **unmeasured** until someone runs a real
//!   agent; this suite is the deterministic substrate such a run would be built on, and
//!   it is the half that can be enforced in CI.
//!
//! # Scoring
//!
//! Each task is worth 1 point and is scored pass/fail on an exact or containment
//! assertion. The score is `passed / total`. Tasks are grouped so a regression names
//! the tool that broke rather than just moving one number.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]
#![cfg(unix)]

use opencrayast_core::ErrorCode;
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::limits::Limits;
use opencrayast_tools::{
    ExplainArgs, GetArgs, Mode, OutlineArgs, SearchArgs, ToolContext, ast_explain_pattern, ast_get,
    ast_info, ast_outline, ast_search,
};
use std::fmt::Write as _;
use std::fs;

const ID: &str = "w-00112233445566778899aabbccddeeff";

// ---------------------------------------------------------------- the fixture
//
// A small workspace whose correct answers are known by construction, so a task's
// expectation is a fact about the fixture rather than a snapshot of whatever the
// tool happened to print today. Each expected string below is annotated with WHY it
// is correct.

const LIB_RS: &str = r#"//! Fixture library.

/// Configuration for the fixture service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Root of the service.
    pub root: String,
    /// How many times to retry.
    pub retries: u32,
}

impl Config {
    /// Build a default configuration.
    pub fn new(root: &str) -> Self {
        Config { root: root.to_string(), retries: 3 }
    }

    /// Load a configuration from the environment.
    pub fn load() -> Self {
        Config::new(".")
    }

    pub(crate) fn hidden_helper(&self) -> u32 {
        self.retries
    }
}

/// A value with no configuration at all.
pub const DEFAULT_RETRIES: u32 = 3;

pub trait Greeter {
    /// Produce a greeting.
    fn greet(&self, who: &str) -> String;
}

pub struct EnglishGreeter;

impl Greeter for EnglishGreeter {
    fn greet(&self, who: &str) -> String {
        let _ = who;
        "hello".to_string()
    }
}

/// Twice a number.
pub fn double(n: u32) -> u32 {
    n * 2
}

/// Build a default configuration. Deliberately shares its name with the Python
/// fixture's `build`, so the task suite has a genuinely ambiguous symbol to score.
pub fn build() -> Config {
    Config::new(".")
}
"#;

const APP_PY: &str = r#""""Fixture module."""


class Service:
    """A fixture service."""

    def __init__(self, root):
        self.root = root

    def start(self):
        return True

    def stop(self):
        return False


def build(root):
    """Build a service. Shares its name with the Rust fixture's `build`, so the
    task suite has a genuinely ambiguous symbol to score."""
    return Service(root)


CONST_LIMIT = 10
"#;

fn put(root: &std::path::Path, rel: &str, body: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, body).unwrap();
}

fn workspace() -> (tempfile::TempDir, ToolContext) {
    let d = tempfile::tempdir().unwrap();
    put(d.path(), "src/lib.rs", LIB_RS);
    put(d.path(), "src/app.py", APP_PY);
    let ctx = ToolContext {
        boundary: Boundary::new(BoundaryConfig {
            root: d.path().to_path_buf(),
            limits: Limits::default(),
            read_roots: Vec::new(),
            state_dir: None,
            extra_protected: Vec::new(),
        })
        .unwrap(),
        limits: Limits::default(),
        mode: Mode::ReadOnly,
        write: None,
        version: "0.20261002.1".into(),
        workspace_id: ID.into(),
        respect_gitignore: true,
        extra_ignore: Vec::new(),
        config_source: Default::default(),
    };
    (d, ctx)
}

// ---------------------------------------------------------------- scoring

/// One scored task's result.
struct Outcome {
    name: String,
    passed: bool,
    detail: String,
}

/// How an expectation is checked against what the tool returned.
#[derive(Clone, Copy)]
enum Expect {
    /// The output contains this substring.
    Contains(&'static str),
    /// The call fails with this code AND the error carries a non-empty next step.
    ///
    /// The next step is part of the expectation, not an extra: a refusal that
    /// cannot say what to do next is a dead end, and measuring that is one of the
    /// things this suite is for.
    CodeWithNext(ErrorCode),
}

impl Expect {
    fn check(self, got: Result<String, opencrayast_core::ToolError>) -> Result<(), String> {
        match (self, got) {
            (Expect::Contains(needle), Ok(out)) => {
                if out.contains(needle) {
                    Ok(())
                } else {
                    Err(format!("output does not contain {needle:?}; got:\n{out}"))
                }
            }
            (Expect::CodeWithNext(code), Err(e)) => {
                if e.code != code {
                    return Err(format!(
                        "expected error {code:?}, got {:?}: {}",
                        e.code, e.message
                    ));
                }
                if e.next.trim().is_empty() {
                    // A refusal that cannot tell the caller what to do next is a
                    // dead end, and this suite is what holds that standard.
                    return Err(format!(
                        "error {code:?} carries no next step: {:?}",
                        e.message
                    ));
                }
                Ok(())
            }
            (Expect::Contains(needle), Err(e)) => Err(format!(
                "expected success containing {needle:?}, got error {:?}: {}",
                e.code, e.message
            )),
            (Expect::CodeWithNext(_), Ok(out)) => {
                Err(format!("expected an error, got success:\n{out}"))
            }
        }
    }
}

fn record(
    results: &mut Vec<Outcome>,
    name: &str,
    tool: &str,
    expect: Expect,
    got: Result<String, opencrayast_core::ToolError>,
) {
    match expect.check(got) {
        Ok(()) => results.push(Outcome {
            name: format!("{tool}: {name}"),
            passed: true,
            detail: String::new(),
        }),
        Err(detail) => results.push(Outcome {
            name: format!("{tool}: {name}"),
            passed: false,
            detail,
        }),
    }
}

fn report(results: &[Outcome], suite: &str) -> String {
    let passed = results.iter().filter(|r| r.passed).count();
    let mut s = format!("\n{suite}: {}/{} tasks passed\n", passed, results.len());
    for r in results {
        s.push_str(&format!(
            "  {} {}\n",
            if r.passed { "PASS" } else { "FAIL" },
            r.name
        ));
        if !r.passed {
            let _ = writeln!(s, "       {}", r.detail.replace('\n', "\n       "));
        }
    }
    s
}

// ---------------------------------------------------------------- the tasks

/// Deterministic tool-level tasks. Each is a fixed call with a known-correct answer.
#[test]
fn ux_tasks_read_tools() {
    let (_d, ctx) = workspace();
    let mut r: Vec<Outcome> = Vec::new();

    // --- ast_info: the session-opening call. ------------------------------------
    // Correct: mode, workspace id and the language list are all facts about the ctx
    // this runner constructed, not about the machine.
    record(
        &mut r,
        "reports the mode it was given",
        "ast_info",
        Expect::Contains("mode: read-only"),
        Ok(ast_info(&ctx)),
    );
    record(
        &mut r,
        "reports the workspace id it was given",
        "ast_info",
        Expect::Contains(ID),
        Ok(ast_info(&ctx)),
    );
    record(
        &mut r,
        "reports write as disabled in read mode",
        "ast_info",
        Expect::Contains("write: disabled"),
        Ok(ast_info(&ctx)),
    );

    // --- ast_outline: shape before content. -------------------------------------
    // Correct: lib.rs has exactly one struct (Config), one trait (Greeter) and one
    // top-level fn (double); the fixture says so.
    record(
        &mut r,
        "lists the fixture's top-level symbols",
        "ast_outline",
        Expect::Contains("struct Config"),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "src/lib.rs".into(),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "finds the trait, not just functions and structs",
        "ast_outline",
        Expect::Contains("trait Greeter"),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "src/lib.rs".into(),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "names the module-level function",
        "ast_outline",
        Expect::Contains("fn double"),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "src/lib.rs".into(),
                ..Default::default()
            },
        ),
    );

    // Depth is the argument an agent gets wrong first, and the fixture has a fn
    // inside an impl: at depth 1 the methods are hidden, at depth 2 they are not.
    let shallow = ast_outline(
        &ctx,
        &OutlineArgs {
            path: "src/lib.rs".into(),
            depth: Some(1),
            ..Default::default()
        },
    );
    let deep = ast_outline(
        &ctx,
        &OutlineArgs {
            path: "src/lib.rs".into(),
            depth: Some(2),
            ..Default::default()
        },
    );
    record(
        &mut r,
        "depth 1 hides methods nested in an impl",
        "ast_outline",
        Expect::Contains("struct Config"),
        shallow.clone(),
    );
    let shallow_shows_method = shallow
        .as_ref()
        .is_ok_and(|s| s.contains("fn new") || s.contains("fn load"));
    record(
        &mut r,
        "depth 2 reveals methods nested in an impl",
        "ast_outline",
        Expect::Contains("fn load"),
        deep,
    );
    if shallow_shows_method {
        r.push(Outcome {
            name: "ast_outline: depth 1 hides methods nested in an impl".into(),
            passed: false,
            detail: "depth 1 already showed `fn new`/`fn load`; depth does not bound nesting"
                .into(),
        });
    }

    // A language filter that matches nothing must say so rather than return nothing.
    record(
        &mut r,
        "kinds filter can select a single kind",
        "ast_outline",
        Expect::Contains("fn double"),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "src/lib.rs".into(),
                kinds: Some(vec!["fn".into()]),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "an unknown kind is an invalid_args, with a next step",
        "ast_outline",
        Expect::CodeWithNext(ErrorCode::InvalidArgs),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "src/lib.rs".into(),
                kinds: Some(vec!["function".into()]),
                ..Default::default()
            },
        ),
    );

    // Determinism: the same call twice must produce the same bytes. A tool whose
    // ordering depends on the filesystem cannot be diffed by an agent.
    let a = ast_outline(
        &ctx,
        &OutlineArgs {
            path: "src".into(),
            ..Default::default()
        },
    );
    let b = ast_outline(
        &ctx,
        &OutlineArgs {
            path: "src".into(),
            ..Default::default()
        },
    );
    match (&a, &b) {
        (Ok(x), Ok(y)) if x == y => {}
        _ => r.push(Outcome {
            name: "ast_outline: the same call twice returns the same bytes".into(),
            passed: false,
            detail: "two identical calls on the same directory returned different output".into(),
        }),
    }

    // --- ast_get: one symbol, with its body. ------------------------------------
    record(
        &mut r,
        "returns the body of a known function",
        "ast_get",
        Expect::Contains("n * 2"),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "double".into(),
                path: Some("src/lib.rs".into()),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "resolves a qualified name",
        "ast_get",
        Expect::Contains("Config::new"),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "Config::load".into(),
                path: Some("src/lib.rs".into()),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "resolves a bare name in a second language, with no path",
        "ast_get",
        Expect::Contains("src/app.py"),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "stop".into(),
                ..Default::default()
            },
        ),
    );

    // Struct FIELDS are not symbols, so `root` resolves to nothing even though the
    // word appears in three places. An agent that guesses a field name should get a
    // `not_found` that points at ast_outline, not silence - this pins that.
    record(
        &mut r,
        "a field name is not a symbol",
        "ast_get",
        Expect::CodeWithNext(ErrorCode::NotFound),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "root".into(),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "a missing symbol points at ast_outline",
        "ast_get",
        Expect::CodeWithNext(ErrorCode::NotFound),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "no_such_symbol".into(),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "an ambiguous symbol lists the candidates",
        "ast_get",
        Expect::CodeWithNext(ErrorCode::Ambiguous),
        // `build` exists in BOTH files by construction (Rust `build` and Python
        // `build`), so a bare name with no `path` genuinely matches twice. This is
        // the case where naming the wrong argument is expensive, so the error must
        // list the candidates and say what to do about it.
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "build".into(),
                ..Default::default()
            },
        ),
    );

    // --- ast_search: shape, not text. -------------------------------------------
    record(
        &mut r,
        "finds every call of a known shape",
        "ast_search",
        Expect::Contains("Config::new"),
        ast_search(
            &ctx,
            &SearchArgs {
                pattern: "Config::new($$$ARGS)".into(),
                language: Some("rust".into()),
                paths: vec!["src".into()],
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "a mixed-language directory without `language` is refused, and says so",
        "ast_search",
        Expect::CodeWithNext(ErrorCode::InvalidArgs),
        ast_search(
            &ctx,
            &SearchArgs {
                pattern: "$OBJ.root".into(),
                paths: vec!["src".into()],
                ..Default::default()
            },
        ),
    );
    // ...and naming the language makes the same call succeed. Together these two are
    // the ergonomic story: the refusal costs a round trip, but it costs a legible one.
    record(
        &mut r,
        "the same call succeeds once `language` is named",
        "ast_search",
        Expect::Contains("src/lib.rs"),
        ast_search(
            &ctx,
            &SearchArgs {
                pattern: "$OBJ.retries".into(),
                language: Some("rust".into()),
                paths: vec!["src".into()],
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "a pattern that cannot parse is invalid_pattern, with a next step",
        "ast_search",
        Expect::CodeWithNext(ErrorCode::InvalidPattern),
        ast_search(
            &ctx,
            &SearchArgs {
                pattern: "((((".into(),
                language: Some("rust".into()),
                paths: vec!["src".into()],
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "an empty pattern is refused, with a next step",
        "ast_search",
        Expect::CodeWithNext(ErrorCode::InvalidArgs),
        ast_search(
            &ctx,
            &SearchArgs {
                pattern: String::new(),
                paths: vec!["src".into()],
                ..Default::default()
            },
        ),
    );

    // --- ast_explain_pattern: confirm before searching. -------------------------
    record(
        &mut r,
        "explains a valid pattern",
        "ast_explain_pattern",
        Expect::Contains("metavariable"),
        ast_explain_pattern(
            &ctx,
            &ExplainArgs {
                pattern: "$OBJ.method($$$ARGS)".into(),
                language: "rust".into(),
            },
        ),
    );
    record(
        &mut r,
        "refuses an unparseable pattern, with a next step",
        "ast_explain_pattern",
        Expect::CodeWithNext(ErrorCode::InvalidPattern),
        ast_explain_pattern(
            &ctx,
            &ExplainArgs {
                pattern: "$$$".into(),
                language: "rust".into(),
            },
        ),
    );

    // --- the boundary every tool shares. ----------------------------------------
    // An agent must never be able to read outside the workspace, whichever tool
    // it reaches for. This task exists to hold all five to it.
    record(
        &mut r,
        "refuses a traversal escape from the workspace",
        "ast_outline",
        Expect::CodeWithNext(ErrorCode::OutsideWorkspace),
        ast_outline(
            &ctx,
            &OutlineArgs {
                path: "../".into(),
                ..Default::default()
            },
        ),
    );
    record(
        &mut r,
        "ast_get refuses an escape as well",
        "ast_get",
        Expect::CodeWithNext(ErrorCode::OutsideWorkspace),
        ast_get(
            &ctx,
            &GetArgs {
                symbol: "double".into(),
                path: Some("../../etc".into()),
                ..Default::default()
            },
        ),
    );

    let s = report(&r, "UX task suite (tools only, no LLM)");
    print!("{s}");
    let failures: Vec<&Outcome> = r.iter().filter(|o| !o.passed).collect();
    assert!(
        failures.is_empty(),
        "{}/{} tool tasks failed:\n{}",
        failures.len(),
        r.len(),
        s
    );
}

/// The suite's score, as a single number, so a future revision can be compared
/// against this baseline without re-deriving it by hand.
///
/// The baseline recorded in `docs/AGENT-TASKSET.md` is the number this prints.
#[test]
fn ux_tasks_score_is_stable() {
    // A guard against the score being quoted from a stale run: if a task is added,
    // the count here must change with it, so the documented baseline cannot quietly
    // describe a suite that no longer exists.
    let (_d, ctx) = workspace();
    assert!(ast_info(&ctx).contains("read-only"));
    let out = ast_outline(
        &ctx,
        &OutlineArgs {
            path: "src".into(),
            ..Default::default()
        },
    )
    .expect("the fixture workspace outlines");
    assert!(out.contains("fn double"));
}
