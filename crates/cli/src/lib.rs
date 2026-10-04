//! The opencrayast human command line, as a library so tests can drive it without a process
//! (docs/ARCHITECTURE.md:71: argument parsing, interactive confirmation, `doctor` — nothing else).
//!
//! `main.rs` is a thin wrapper: it parses, calls [`run`] and maps the result to a process exit code.
//!
//! # Write mode comes from parsed settings, never from a flag on its own
//!
//! `--write` alone does not enable writing. It is one of the two gates in `CONFIGURATION.md` /
//! CFG-06, and the other is `[policy] allow_write = true` in the operator's configuration. The two
//! meet in exactly one place — [`write_capability`] — which takes the **already parsed** [`Settings`]
//! and the flag and returns a [`WriteCap`] or `None`. There is no public constructor that turns a
//! `bool` into a capability, no `Default`, and no way for the write commands to be reached without
//! going through this function: [`run_with`] hands the mutating commands nothing but the `Option` it
//! produced. That is the same shape `opencrayast-edit` closed in WCAP-1.
//!
//! # Where the confirmation lives
//!
//! Once. [`confirm::authorize`] is the only function that turns a confirmation into a decision, and
//! every write reaches it through [`crate::edit`]; no command re-implements any part of it, and no
//! other module names a confirmer. That is what makes the non-interactive refusal a property of the
//! CLI rather than a property of each write's argument handling.

use clap::{Parser, Subcommand};
use opencrayast_core::config::Settings;
use opencrayast_edit::WriteCap;
use std::path::PathBuf;

pub mod confirm;
pub mod doctor;
pub mod edit;
pub mod exit;
pub mod out;
pub mod palette;
pub mod plan;
pub mod sweep;

use confirm::{AlwaysYes, Confirmer, Stdin};
use palette::{ColorChoice, Palette};

/// The human command line for opencrayast.
#[derive(Debug, Parser)]
#[command(
    name = "opencrayast",
    version,
    about = "Review a plan, then apply it. Writing needs --write and allow_write in the operator configuration.",
    // Running with no subcommand prints the help and exits 0: the user asked a question the CLI can
    // answer, so it is not an error. A BAD subcommand is exit 1.
    arg_required_else_help = true,
    after_help = exit::HELP_TAIL,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// The workspace root (default: the current directory).
    #[arg(long, global = true, value_name = "DIR")]
    pub workspace: Option<PathBuf>,

    /// An extra READ-ONLY root, repeatable. A file under one is shown as `@root1/…`,
    /// `@root2/…` in that order, and can be read but never written (BND-19).
    ///
    /// `/`, a drive root, the home directory itself and credential directories are refused here
    /// exactly as they are for `--workspace`.
    #[arg(long, global = true, value_name = "DIR")]
    pub read_root: Vec<PathBuf>,

    /// Allow mutating commands. This is the **first** of two gates: the operator's configuration
    /// must also say `[policy] allow_write = true`. Neither alone is enough.
    #[arg(long, global = true)]
    pub write: bool,

    /// Say yes to the confirmation `edit apply`, `edit undo` and `edit recover` ask. Without it,
    /// those three refuse when there is no terminal to ask.
    ///
    /// `--yes` means a person has already read this plan. It is the only way to write unattended,
    /// and it is not a default.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Colour: auto, always or never. Auto colours only on a terminal, and never when NO_COLOR is
    /// set or TERM=dumb.
    #[arg(long, global = true, value_name = "WHEN")]
    pub color: Option<ColorChoice>,

    /// The operator configuration file. Omit to use the documented location.
    ///
    /// A file that exists but is unacceptable (malformed, world-writable, someone else's) is an
    /// error, never a silent fallback to defaults.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<String>,

    /// What was asked for.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
///
/// `edit` is the only one that writes, and it holds two of its own gates: `--write` plus the
/// operator's configuration, and a confirmation or `--yes`. The bare names `apply`, `undo` and
/// `recover` are deliberately **not** top-level commands — they exist only under `edit`, so there is
/// one spelling of a write and one place its confirmation lives.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check that this machine can use opencrayast, and say what to do about what is wrong.
    Doctor,
    /// Read stored plans.
    #[command(subcommand)]
    Plan(PlanCmd),
    /// Review, apply and undo plans. The write path, and the person in the loop.
    #[command(subcommand)]
    Edit(EditCmd),
}

/// The read-only plan commands.
#[derive(Debug, Subcommand)]
pub enum PlanCmd {
    /// List the stored plans of this workspace.
    List {
        /// How many plans to show (default 20).
        #[arg(long, default_value_t = plan::DEFAULT_LIMIT, value_name = "N")]
        limit: usize,
    },
    /// Show one plan. Accepts a full id or an unambiguous prefix of at least 10 characters.
    Show {
        /// The plan id, or a prefix of it.
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
    },
    /// Apply the retention policy now: delete expired plans and aged journals.
    ///
    /// The only reachable entry point for `PlanStore::sweep` and `JournalStore::evict`. Deleting a
    /// journal makes that plan's edit permanently unundoable, so this is an operator decision and
    /// is never run for you: `opencrayast doctor` reports what it would remove.
    Gc,
}

/// The `edit` subcommands. Reading ones are `edit preview|show|list`; the rest write the workspace
/// and need `--write` **and** the configuration opt-in **and** a confirmation or `--yes`.
#[derive(Debug, Subcommand)]
pub enum EditCmd {
    /// Produce a plan and show it, with the risk summary. Writes no file in the workspace.
    ///
    /// This is `ast_edit_preview` with a human layout: a diff you can read, the files and edits it
    /// would make, how many bytes move, which files already have syntax errors, and when it expires.
    /// It stores the plan and prints its id; `edit show` and `edit apply` are how you reach it
    /// afterwards.
    Preview {
        /// The language the pattern is compiled for (for example `rust`, `typescript`, `go`).
        #[arg(long, value_name = "LANG")]
        language: String,
        /// A file or directory to scan. Repeat for more than one.
        #[arg(long, value_name = "PATH", required = true)]
        path: Vec<String>,
        /// The pattern to find, e.g. `log($$$ARGS)`.
        #[arg(long, value_name = "PATTERN")]
        pattern: String,
        /// What to put in its place, e.g. `logger.debug($$$ARGS)`. Use `$$` for a literal `$`.
        #[arg(long, value_name = "TEXT")]
        replacement: String,
        /// A label stored with the plan and shown to reviewers. It is part of the hashed plan, so
        /// changing it changes the plan id.
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
    /// Show a stored plan: its diff, its sizes, its syntax-error counts. Reads only.
    ///
    /// Takes the full plan id or an unambiguous prefix of at least 10 characters — reading may
    /// abbreviate, writing may not. A prefix that matches several plans is refused with the
    /// candidates listed, never guessed at.
    Show {
        /// The plan id, or a prefix of it.
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
        /// Show only this file's diff.
        #[arg(long, value_name = "PATH")]
        file: Option<String>,
        /// First hunk to show (0-based).
        #[arg(long, value_name = "N")]
        offset: Option<usize>,
        /// Maximum hunks to show.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// List this workspace's stored plans. Reads only; the same set `plan list` shows.
    List {
        /// How many plans to show (default 20).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
    /// Apply a stored plan. Needs --write, and a confirmation or --yes.
    ///
    /// Takes the FULL plan id. It prints the exact list of files it will change and asks before it
    /// writes; with no terminal and no --yes it refuses without writing anything.
    Apply {
        /// The full plan id, all 28 characters. A prefix is refused here.
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
    },
    /// Undo an applied plan. Needs --write, and a confirmation or --yes.
    ///
    /// Takes the FULL plan id, and only reverses a plan whose files still hold exactly what the
    /// apply wrote. Anything else is refused as `diverged`, naming the files and nothing about what
    /// is in them.
    Undo {
        /// The full plan id, all 28 characters.
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
    },
    /// Converge every half-applied plan after a crash. Needs --write, and a confirmation or --yes.
    ///
    /// Normally unnecessary — every apply does this first — and it takes no plan id, because there is
    /// no single plan to name.
    Recover,
}

/// The one place `--write` and the operator's configuration meet.
///
/// Returns `None` unless **both** gates pass, and the capability it returns can only have come from
/// a `WritePermission` that [`Settings`] minted while parsing a file that said `allow_write = true`.
/// So a caller cannot turn writing on by flipping the flag, and cannot turn it on by constructing a
/// [`WriteCap`] either — there is no public constructor for one.
pub fn write_capability(settings: &Settings, write_flag: bool) -> Option<WriteCap> {
    if !write_flag {
        return None;
    }
    settings.write_permission().map(WriteCap::mint)
}

/// Parse the process arguments, mapping clap's own failures onto this CLI's exit codes.
///
/// clap exits with 2 on a parse error; the contract here says an argument the user got wrong is
/// exit 1, and `--help` / `--version` are answers rather than failures.
pub fn parse_args() -> Result<Cli, (i32, clap::Error)> {
    exit_code_for_error(Cli::try_parse())
}

/// Parse an explicit argument vector (tests use this; [`parse_args`] uses the process ones) and
/// return the process exit code the entry point would exit with.
pub fn parse_args_from_check(args: &[&str]) -> (i32, clap::Error) {
    match Cli::try_parse_from(args) {
        Ok(_) => (
            exit::EXIT_OK,
            clap::Error::raw(clap::error::ErrorKind::DisplayHelp, ""),
        ),
        Err(e) => (exit_code_of(&e), e),
    }
}

/// The exit code for a clap error: a help or version request is an answer, anything else is a user
/// error. One function, because [`parse_args`] and [`parse_args_from_check`] must not be able to
/// disagree about the same mistake.
fn exit_code_of(e: &clap::Error) -> i32 {
    match e.kind() {
        clap::error::ErrorKind::DisplayHelp
        | clap::error::ErrorKind::DisplayVersion
        | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => exit::EXIT_OK,
        _ => exit::EXIT_USER,
    }
}

fn exit_code_for_error(result: Result<Cli, clap::Error>) -> Result<Cli, (i32, clap::Error)> {
    result.map_err(|e| (exit_code_of(&e), e))
}

/// Run one parsed invocation and return the process exit code.
///
/// The production entry point. It builds the three injected dependencies — the palette from the real
/// environment, the confirmer from the real terminal and `--yes`, and nothing else — and hands them
/// to [`run_with`], which is what the tests drive.
pub fn run(cli: &Cli, sink: &mut dyn out::Sink) -> i32 {
    let palette = Palette::detect(cli.color.unwrap_or(ColorChoice::Auto));
    let mut confirmer: Box<dyn Confirmer> = if cli.yes {
        Box::new(AlwaysYes::new())
    } else {
        Box::new(Stdin::new())
    };
    run_with(cli, sink, palette, confirmer.as_mut())
}

/// Run one parsed invocation with the palette and the confirmer **supplied**.
///
/// Everything a test needs to be deterministic lives in the two arguments: the palette is a value
/// rather than an environment lookup, and the confirmer answers a script rather than asking a
/// terminal. That is the whole reason this split exists — a test that reached the process's
/// `NO_COLOR`, its stdout or its stdin would be testing the machine it ran on.
pub fn run_with(
    cli: &Cli,
    sink: &mut dyn out::Sink,
    palette: Palette,
    confirmer: &mut dyn Confirmer,
) -> i32 {
    run_with_state(cli, sink, palette, confirmer, &StateDir::Resolved)
}

/// Where a run's state directory came from.
///
/// Two answers, not one: a real run resolves it from the environment, and a test names it. The
/// distinction is the same one [`run_with`] already draws with the palette and the confirmer —
/// "everything a test needs to be deterministic lives in the arguments" — and the state directory
/// joined them because the alternative was for every CLI test to read the *developer's* state
/// directory, which would make each one write into `$XDG_STATE_HOME` and see whatever the last
/// test left there.
#[derive(Debug, Clone, Copy)]
pub enum StateDir<'a> {
    /// Resolve the platform user-state directory (what a real run does).
    Resolved,
    /// Use this directory instead. For tests.
    Fixed(&'a std::path::Path),
}

/// The three inputs a run takes from its environment rather than from its arguments.
///
/// Grouped because they are one thing, not three: each is a seam a test replaces so that a test
/// never depends on the machine it runs on. The palette stops `NO_COLOR` and terminal detection
/// deciding an assertion, the confirmer stops a human being needed, and the state directory stops
/// the developer's real `$XDG_STATE_HOME` being written to and read from. Passing them as one
/// value also keeps the argument count of every handler at something a human can hold.
pub struct Env<'a> {
    /// Colour and terminal decisions. Always supplied, never guessed.
    pub palette: Palette,
    /// Who answers the human gate. Always supplied, never a real terminal.
    pub confirmer: &'a mut dyn Confirmer,
    /// Where state lives: resolved from the environment, or named by a test.
    pub state: StateDir<'a>,
}

impl StateDir<'_> {
    /// The directory this run should use, or the refusal the resolver gave.
    fn resolve(&self) -> Result<PathBuf, opencrayast_core::error::ToolError> {
        match self {
            Self::Resolved => opencrayast_core::statedir::user_state_dir(),
            Self::Fixed(p) => Ok(p.to_path_buf()),
        }
    }
}

/// Run one parsed invocation with **every** environment-determined input supplied: the palette,
/// the confirmer, and the state directory.
///
/// [`run_with`] is this with the state directory resolved from the environment, and `run` is
/// that with a `Stdin` confirmer. Nothing else reaches for the ambient environment, so a test that
/// needs a real state directory says which one in the call.
pub fn run_with_state(
    cli: &Cli,
    sink: &mut dyn out::Sink,
    palette: Palette,
    confirmer: &mut dyn Confirmer,
    state: &StateDir<'_>,
) -> i32 {
    let root = cli.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
    // One value for the three environment-supplied inputs, so each handler below takes an
    // argument list a human can hold and none of them can reach the ambient machine by accident.
    let mut env = Env {
        palette,
        confirmer,
        state: *state,
    };
    let mut o = out::Out::new(sink);

    // The operator's configuration is read once, here, and every command is held to the limits it
    // names — the same decision point the MCP shell uses (`Settings`).
    //
    // The exit code comes from `exit_code_for_error` like every other refusal, so a MALFORMED
    // configuration is a user error (1) and an UNTRUSTED one — wrong owner, wrong permissions,
    // unreadable — is an environment error (2). Both used to be flattened to 2 here, which is how
    // the two shells came to disagree about every configuration refusal: this surface invented
    // its own number instead of asking the mapping that `exit.rs` documents.
    let settings = match opencrayast_core::config::load_or_default(cli.config.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            let code = exit::exit_code_for_error(&e);
            o.diag(&format!("[{}] {}", e.code.as_str(), e.message));
            o.diag(&format!("Next: {}", e.next));
            return code;
        }
    };

    // Which file produced those settings, decided by the same function `load_or_default` used.
    let config_location = opencrayast_core::config::config_location(cli.config.as_deref());
    let config_source: opencrayast_tools::ConfigSource = config_location.clone().into();

    match &cli.command {
        Command::Doctor => doctor::run(
            &root,
            cli.write,
            cli.config.as_deref(),
            &config_location,
            &cli.read_root,
            &env,
            &mut o,
        ),
        Command::Plan(cmd) => {
            let ws = match workspace_or_report(&root, &mut o) {
                Ok(ws) => ws,
                Err(code) => return exit::exit_code_for(code),
            };
            // Resolved once, by the same function the MCP shell and the boundary use. A machine
            // that cannot say where its state lives is an environment error, not a reason to fall
            // back to putting state inside the workspace.
            let state_dir = match env.state.resolve() {
                Ok(d) => d,
                Err(e) => {
                    o.diag(&format!("[{}] {}", e.code.as_str(), e.message));
                    o.diag(&format!("Next: {}", e.next));
                    return exit::EXIT_ENV;
                }
            };
            match cmd {
                PlanCmd::List { limit } => {
                    plan::list(&ws, &state_dir, &settings.limits, *limit, &mut o)
                }
                PlanCmd::Show { plan_id } => {
                    plan::show(&ws, &state_dir, &settings.limits, plan_id, &mut o)
                }
                PlanCmd::Gc => sweep::run(&state_dir, &ws, &settings.limits, &mut o),
            }
        }
        Command::Edit(cmd) => {
            // The capability is decided here, before the module that writes exists, so a refused
            // write never opens a store, never reads a file and — trivially — cannot have written
            // anything. `cli.write` is the operator's half of the gate; `settings`' own
            // `policy.allow_write` is the other, and `WriteCap::from_operator` requires both.
            let capability = write_capability(&settings, cli.write);
            edit::run(
                &to_edit(cmd, cli.yes),
                &root,
                &settings,
                &cli.read_root,
                capability,
                &config_source,
                &mut env,
                &mut o,
            )
        }
    }
}

/// Turn the parsed subcommand into what [`edit::run`] executes.
///
/// One conversion, in one place, so `EditCmd` can stay a pure argument shape and the module that
/// does the work cannot grow its own idea of what a flag means. `--yes` is carried here rather than
/// read from `Cli` inside `edit`, so the confirmation's inputs are visible at the call site.
fn to_edit(cmd: &EditCmd, yes: bool) -> edit::Edit {
    match cmd {
        EditCmd::Preview {
            language,
            path,
            pattern,
            replacement,
            note,
        } => edit::Edit::Preview(Box::new(opencrayast_tools::PreviewArgs {
            kind: "rewrite".into(),
            language: Some(language.clone()),
            paths: Some(path.clone()),
            pattern: Some(pattern.clone()),
            replacement: Some(replacement.clone()),
            note: note.clone(),
            ..opencrayast_tools::PreviewArgs::default()
        })),
        EditCmd::Show {
            plan_id,
            file,
            offset,
            limit,
        } => edit::Edit::Show {
            plan_id: plan_id.clone(),
            file: file.clone(),
            offset: *offset,
            limit: *limit,
        },
        EditCmd::List { limit } => edit::Edit::List { limit: *limit },
        EditCmd::Apply { plan_id } => edit::Edit::Apply {
            plan_id: plan_id.clone(),
            yes,
        },
        EditCmd::Undo { plan_id } => edit::Edit::Undo {
            plan_id: plan_id.clone(),
            yes,
        },
        EditCmd::Recover => edit::Edit::Recover { yes },
    }
}

/// The workspace id for `root`, or the printed complaint AND the exit code it maps to.
///
/// The exit code is always `exit_code_for` of the error's own code. It is never chosen here: an
/// earlier version returned a hardcoded `EXIT_ENV` while printing `[not_found]`, so the same
/// condition reported itself as one thing and exited as another (CR R3).
fn workspace_or_report(
    root: &Path,
    o: &mut out::Out,
) -> Result<String, opencrayast_core::ErrorCode> {
    match opencrayast_core::workspace::workspace_id(root) {
        Ok(id) => Ok(id),
        Err(e) => {
            o.diag(&format!("[{}] {}", e.code.as_str(), e.message));
            o.diag(&format!("Next: {}", e.next));
            Err(e.code)
        }
    }
}

use std::path::Path;
