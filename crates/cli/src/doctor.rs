//! `doctor`: check that this machine can use opencrayast, and say what to do about what is wrong.
//!
//! Every check prints `ok` / `warn` / `fail` plus a next step, and a `fail` is reported, never
//! panicked on: the whole point of `doctor` is to survive a broken workspace (CLI1-01). Nothing here
//! decides anything — it asks the same L0–L4 functions the tools ask, and reports their answers.
//!
//! # What `doctor` touches
//!
//! It creates the state directory, because that is one of the things it is checking can be created,
//! and an empty state directory is not a change to anyone's code. It also creates and immediately
//! removes ONE empty probe file in the workspace root (`.opencrayast-write-probe`) to find out
//! whether the root is really writable — a permission that says yes and a filesystem that says no
//! are both possible, and only a real write settles it. That probe is removed on the success path
//! AND on the failure path, but a process killed between the two would leave it behind; a later run
//! removes any stale one it finds. Nothing else in the workspace is written, ever.

use crate::out::Out;
use opencrayast_core::error::ToolError;
use opencrayast_core::limits::Limits;
use opencrayast_core::workspace::workspace_id;
use std::path::{Path, PathBuf};

/// How one check went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Works.
    Ok,
    /// Works, but something is worth knowing.
    Warn,
    /// Does not work; the line says what to do.
    Fail,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Warn => "warn",
            Verdict::Fail => "fail",
        }
    }
}

/// One printed check line.
struct Line {
    name: &'static str,
    verdict: Verdict,
    detail: String,
}

/// Run every check against `root`, printing to `out`.
///
/// Returns the process exit code: 0 when nothing failed, 2 when anything did. A `warn` alone is
/// still success — a warning is information, not a refusal.
pub fn run(
    root: &Path,
    write_enabled: bool,
    config: Option<&str>,
    config_location: &opencrayast_core::config::ConfigLocation,
    read_roots: &[PathBuf],
    env: &crate::Env<'_>,
    out: &mut Out,
) -> i32 {
    // The operator's configuration is read here, once, and everything below is held to it. A file
    // that exists and is unacceptable is reported and stops `doctor` — falling back to defaults
    // would tell the operator their settings were in force when they were not.
    let settings = match opencrayast_core::config::load_or_default(config) {
        Ok(s) => s,
        Err(e) => {
            out.line(&format!("fail  configuration  {} — {}", e.message, e.next));
            return crate::exit::EXIT_ENV;
        }
    };
    let limits = settings.limits.clone();
    let mut lines: Vec<Line> = Vec::new();

    // 0. which file is in force. First, because every other line below is "in force" *because of*
    // this file, and because a configuration the operator cannot name is one they cannot audit.
    lines.push(config_line(root, config_location));

    // 1. the workspace root exists and is a directory we can name
    let ws = match workspace_id(root) {
        Ok(id) => {
            lines.push(Line {
                name: "workspace",
                verdict: Verdict::Ok,
                detail: format!("{} ({id})", display_path(root)),
            });
            Some(id)
        }
        Err(e) => {
            lines.push(Line {
                name: "workspace",
                verdict: Verdict::Fail,
                detail: format!("{} — {}", e.message, e.next),
            });
            None
        }
    };

    // 2. the root can actually be written to; without this no plan can be stored
    lines.push(writable_check(root));

    // 2b. the operator's extra READ-ONLY roots, one line each. Each goes through the same
    // `check_root` the workspace root does, so a forbidden root is reported here rather than
    // quietly dropped from the boundary. A root the agent can read but the operator cannot
    // audit is the failure this prevents — which is why the lines are printed even when every
    // one of them is fine, and why the `@root<N>` label is shown next to the path.
    for (i, r) in read_roots.iter().enumerate() {
        lines.push(read_root_check(r, i + 1));
    }

    // 3. the state directory can be created and used. This is only attempted once the workspace
    //    itself checked out: `ensure_state_dir` creates what is missing, so running it for a root
    //    that does not exist would leave a stray directory behind for a workspace nobody has.
    //
    //    Resolution and use are separate and reported separately, because they fail for
    //    different reasons: a machine with no `XDG_STATE_HOME` and no `HOME` cannot say *where*
    //    state would go (and there is no fallback — never the workspace), while a resolvable
    //    directory can still be one this user may not adopt.
    let state_dir = match env.state.resolve() {
        Ok(dir) => dir,
        Err(e) => {
            lines.push(Line {
                name: "state dir",
                verdict: Verdict::Fail,
                detail: format!("{} — {}", e.message, e.next),
            });
            lines.push(Line {
                name: "plan store",
                verdict: Verdict::Warn,
                detail: "not checked: the state directory location cannot be determined".into(),
            });
            return finish(lines, out);
        }
    };
    let state = match &ws {
        Some(_) => state_dir_check(&state_dir),
        None => Line {
            // Not a failure of the state directory: it was never tried, because there is no
            // workspace to hold it. `warn`, so the rule that a `fail` always carries a next step
            // stays true.
            name: "state dir",
            verdict: Verdict::Warn,
            detail: "not checked: the workspace root itself is not usable".into(),
        },
    };
    let state_usable = state.verdict != Verdict::Fail;
    lines.push(state);

    // 4. the plan store and the journal store can be opened and listed. This is the check that
    //    catches a state directory that exists but is not ours, or is on a read-only mount.
    if let (Some(_), true) = (&ws, state_usable) {
        let ws_id = ws.clone().unwrap_or_default();
        lines.push(store_check(&state_dir, &ws_id, &limits));
    } else {
        lines.push(Line {
            name: "plan store",
            verdict: Verdict::Warn,
            detail: "not checked: the workspace or state directory is not usable".into(),
        });
    }

    // 5. which languages this build actually has grammars for
    lines.push(languages_check(&settings, config_location));

    // 6. write mode — both the user-file opt-in (`WritePermission`) and `--write`.
    // A flag alone is not enough (CONFIGURATION.md / CFG-06 / WCAP-1).
    let write_cap = opencrayast_edit::WriteCap::from_operator(&settings, write_enabled);
    lines.push(Line {
        name: "write mode",
        verdict: if write_cap.is_some() {
            Verdict::Ok
        } else {
            Verdict::Warn
        },
        detail: if write_cap.is_some() {
            "enabled: mutating commands are allowed".into()
        } else if write_enabled {
            "disabled: --write was passed but policy.allow_write is not true in the user file"
                .into()
        } else {
            "disabled: only reading commands are allowed (pass --write and set policy.allow_write = true)"
                .into()
        },
    });

    // 7. what a maintenance sweep would reclaim right now. Reported, never performed: `doctor` is
    //    a diagnostic and the operator is the one who decides whether to run the verb. Before
    //    the sweep entry point existed this line could not exist at all, because "what would be
    //    removed" was unanswerable — nothing ever asked.
    let reclaimable: Result<crate::sweep::Reclaimable, Option<ToolError>> =
        match (state_usable, &ws) {
            (true, Some(ws)) => crate::sweep::reclaimable(&state_dir, ws, &limits).map_err(Some),
            // Nothing to count against: either the store cannot be opened, or there is no
            // workspace whose plans these would be. A `warn` with the reason, so the rule that a
            // `fail` always carries a next step stays true.
            _ => Err(None),
        };
    lines.push(match reclaimable {
        Ok(r) if r.is_empty() => Line {
            name: "reclaimable",
            verdict: Verdict::Ok,
            detail: "nothing: no plan or journal is past its retention".into(),
        },
        Ok(r) => Line {
            name: "reclaimable",
            verdict: Verdict::Warn,
            detail: format!(
                "{} plan(s) and {} journal(s) past retention, still on disk — run \
                 `opencrayast plan gc` to remove them",
                r.plans, r.journals
            ),
        },
        Err(e) => Line {
            name: "reclaimable",
            verdict: Verdict::Warn,
            detail: match e {
                Some(e) => format!("not checked: {} — {}", e.message, e.next),
                None => "not checked: the workspace or state directory is not usable".into(),
            },
        },
    });

    finish(lines, out)
}

/// Print the collected lines and the exit code they imply.
///
/// Split out because an unresolvable state directory ends [`run`] early: the remaining checks
/// (languages, write mode) answer questions about the *build* and the *operator*, not about
/// state, so they stay meaningful — and an operator reading this output should see them rather
/// than a truncated report.
fn finish(lines: Vec<Line>, out: &mut Out) -> i32 {
    let mut failed = false;
    for l in &lines {
        if l.verdict == Verdict::Fail {
            failed = true;
        }
        out.line(&format!(
            "{:<6} {:<12} {}",
            l.verdict.as_str(),
            l.name,
            l.detail
        ));
    }
    if failed {
        out.line("");
        out.line("Some checks failed. Fix the ones marked `fail` above, then run `opencrayast doctor` again.");
        crate::exit::EXIT_ENV
    } else {
        out.line("");
        out.line("All checks passed.");
        crate::exit::EXIT_OK
    }
}

/// The empty file `doctor` creates and removes to test whether the workspace root is writable.
/// Named so that it is recognisable as ours and never mistaken for a user's file.
pub const PROBE_NAME: &str = ".opencrayast-write-probe";

/// Where the tool's state lives on this machine: the platform user-state base, resolved by
/// [`opencrayast_core::statedir::user_state_dir`] — the one resolver the MCP shell and
/// `Settings::boundary_config` also use, so all three agree by construction.
///
/// **It does not append `ws-<id>`.** The stores and the apply lock do that themselves, and a
/// function that did it here would make them produce `ws-w-…/ws-w-…/`.
///
/// This replaced a `default_state_dir(root)` that joined `.opencrayast` onto the workspace root.
/// There is no longer a state directory *per workspace*, only one per user with a subdirectory
/// per workspace inside it.
pub fn user_state_dir() -> Result<std::path::PathBuf, ToolError> {
    opencrayast_core::statedir::user_state_dir()
}

/// Which configuration file is in force, and a warning when it is inside the workspace.
///
/// `--config <path>` is honoured wherever it points, including inside `--workspace`. That is the
/// operator's call and is not refused — but a repository can *ship* such a file, and its bytes are
/// indistinguishable from the operator's own once the permission and owner checks pass. So this
/// is a `warn`, not a `fail`: the operator is told, in the one place they already look, and
/// nothing stops them.
///
/// The three answers are the three real ones: no file (defaults), the documented user-level file,
/// or an explicit `--config`. Each names the path, because "a configuration was loaded" is not
/// an answer anyone can act on.
fn config_line(root: &Path, location: &opencrayast_core::config::ConfigLocation) -> Line {
    use opencrayast_core::config::ConfigLocation as L;
    match location {
        L::Defaults => Line {
            name: "config",
            verdict: Verdict::Ok,
            detail: "defaults (no user configuration file found)".into(),
        },
        L::UserFile(p) => Line {
            name: "config",
            verdict: Verdict::Ok,
            detail: display_path(p).to_string(),
        },
        L::ExplicitFile(p) => {
            let inside = p.starts_with(root) || root.starts_with(p);
            if inside {
                Line {
                    name: "config",
                    verdict: Verdict::Warn,
                    detail: format!(
                        "{} is INSIDE the workspace — this file came from the repository side, \
                         not from your user configuration; it was honoured after passing the \
                         0600/owner checks",
                        display_path(p)
                    ),
                }
            } else {
                Line {
                    name: "config",
                    verdict: Verdict::Ok,
                    detail: format!("--config {}", display_path(p)),
                }
            }
        }
    }
}

/// Can we create and write inside `root`?
fn writable_check(root: &Path) -> Line {
    let probe = root.join(PROBE_NAME);
    // A probe left by a killed run is removed first, so the check reports the CURRENT state rather
    // than tripping over its own residue.
    let _ = std::fs::remove_file(&probe);
    let written = std::fs::write(&probe, b"").is_ok();
    // Removed on both outcomes. A failure here means the probe could not be created or could not be
    // deleted, which is itself a reason to report the root as not usable.
    let removed = std::fs::remove_file(&probe).is_ok();
    if written && removed {
        Line {
            name: "workspace writable",
            verdict: Verdict::Ok,
            detail: "yes".into(),
        }
    } else {
        Line {
            name: "workspace writable",
            verdict: Verdict::Fail,
            detail: "no — cannot create a file in the workspace; check the directory permissions"
                .into(),
        }
    }
}

/// One extra READ-ONLY root: is it acceptable, and what label will its files carry?
///
/// The verdict comes from [`opencrayast_core::boundary::validate_read_roots`] — the same
/// `check_root` the boundary will run when it actually builds — so this line cannot disagree with
/// what the tools later do. The label is printed because it is what an agent will see in a result
/// (`@root2/lib.rs`), and an operator reading a diff needs to know which grant produced it.
fn read_root_check(root: &Path, index: usize) -> Line {
    match opencrayast_core::boundary::validate_read_roots(std::slice::from_ref(&root.to_path_buf()))
        .into_iter()
        .next()
    {
        Some(Ok(canonical)) => Line {
            name: "read root",
            verdict: Verdict::Ok,
            detail: format!("@root{index}  {}  (read-only)", display_path(&canonical)),
        },
        Some(Err(e)) => Line {
            name: "read root",
            verdict: Verdict::Fail,
            detail: format!("@root{index}  {} — {}", display_path(root), e.message),
        },
        None => Line {
            name: "read root",
            verdict: Verdict::Fail,
            detail: format!("@root{index}  {} — not checked", display_path(root)),
        },
    }
}

/// Can the state directory be created, and is it a real directory we own?
fn state_dir_check(state_dir: &Path) -> Line {
    match opencrayast_core::statedir::ensure_state_dir(state_dir) {
        Ok(_) => Line {
            name: "state dir",
            verdict: Verdict::Ok,
            detail: format!("{} (usable)", display_path(state_dir)),
        },
        Err(e) => Line {
            name: "state dir",
            verdict: Verdict::Fail,
            // The path is shown relative to what the user typed; never an absolute path from
            // inside the machine.
            detail: format!("{} — {}", e.message, e.next),
        },
    }
}

/// Can the plan store and the journal store be opened and listed?
fn store_check(state_dir: &Path, ws: &str, limits: &Limits) -> Line {
    use opencrayast_edit::{JournalStore, PlanStore, SystemClock};

    let clock = std::sync::Arc::new(SystemClock);
    match PlanStore::open(state_dir, ws, limits.clone(), clock.clone()) {
        Ok(store) => match store.list() {
            Ok(_) => {
                // The journal store lives beside the plan store; open it too, because a workspace
                // where plans work but journals do not is a real and confusing failure.
                match JournalStore::open(state_dir, ws, limits.clone(), clock) {
                    Ok(_) => Line {
                        name: "plan store",
                        verdict: Verdict::Ok,
                        detail: "readable and writable".into(),
                    },
                    Err(e) => Line {
                        name: "journal store",
                        verdict: Verdict::Fail,
                        detail: format!("{} — {}", e.message, e.next),
                    },
                }
            }
            Err(e) => Line {
                name: "plan store",
                verdict: Verdict::Fail,
                detail: format!("{} — {}", e.message, e.next),
            },
        },
        Err(e) => Line {
            name: "plan store",
            verdict: Verdict::Fail,
            detail: format!("{} — {}", e.message, e.next),
        },
    }
}

/// Which languages this build has grammars for.
///
/// Asked from the tools layer rather than from `opencrayast-lang` directly: the CLI may depend on
/// `tools`, `core` and `edit` only (ARCHITECTURE.md:71), and `ast_info` is already the one place
/// that knows which grammars are compiled in. Asking it means the CLI cannot drift from what the
/// tools will actually accept.
fn languages_check(
    settings: &opencrayast_core::config::Settings,
    config_location: &opencrayast_core::config::ConfigLocation,
) -> Line {
    use opencrayast_core::boundary::Boundary;
    use opencrayast_tools::context::{Mode, ToolContext};

    // One decision point for "which limits", via Settings, rather than a hand-built
    // BoundaryConfig that could drift from the `limits` the tools below are given.
    let Ok(cfg) = settings.boundary_config(PathBuf::from(".")) else {
        return Line {
            name: "languages",
            verdict: Verdict::Warn,
            detail: "not checked: the state directory location cannot be determined".into(),
        };
    };
    let Ok(boundary) = Boundary::new(cfg) else {
        return Line {
            name: "languages",
            verdict: Verdict::Warn,
            detail: "not checked: the tools layer could not be initialised".into(),
        };
    };
    let info = opencrayast_tools::ast_info(&ToolContext {
        config_source: config_location.clone().into(),
        boundary,
        limits: settings.limits.clone(),
        mode: Mode::ReadOnly,
        write: None,
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_id: String::new(),
        respect_gitignore: true,
        extra_ignore: Vec::new(),
    });

    // Six lines now (the `config:` line added when `--config` could point inside the workspace);
    // the third is the language list. Parsing it here keeps the CLI from holding its own copy of
    // the answer, and the line index is what would break first if the order ever changed.
    let Some(line) = info.lines().find(|l| l.starts_with("languages: ")) else {
        return Line {
            name: "languages",
            verdict: Verdict::Warn,
            detail: "not reported by the tools layer".into(),
        };
    };
    let list = line.trim_start_matches("languages: ").trim();
    if list.is_empty() {
        return Line {
            name: "languages",
            verdict: Verdict::Fail,
            detail: "no grammars in this build — nothing can be parsed; rebuild with the language features"
                .into(),
        };
    }
    // Every language the tools layer offers is one a plan may reference; a grammar missing is a
    // warning rather than a failure, because a build without Go is still useful.
    let count = list.split(',').filter(|s| !s.trim().is_empty()).count();
    Line {
        name: "languages",
        verdict: Verdict::Ok,
        detail: format!("{count} available: {}", crate::out::escape_line(list)),
    }
}

/// A path as the user should see it: the part they would recognise, never an absolute path from
/// inside the machine (BRIEF rule 4).
fn display_path(p: &Path) -> String {
    // Show the last two components with a leading ellipsis rather than a stripped leading slash:
    // stripping `/` turns `/tmp/probe` into `tmp/probe`, which reads like a workspace-relative path
    // and is not one. The rule this exists for is that a path from inside the machine is not printed
    // in full, and the caller's own `--workspace` argument is theirs, not ours to abbreviate into
    // something that looks like a relative path.
    let components: Vec<String> = p
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match components.len() {
        0 => ".".to_string(),
        1 | 2 => components.join("/"),
        n => format!("…/{}", components[n - 2..].join("/")),
    }
}
