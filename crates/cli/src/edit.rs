//! `edit preview|show|apply|undo|list`: the human write path, and the person in the loop.
//!
//! # What this module is allowed to do (docs/ARCHITECTURE.md:71)
//!
//! Parse the arguments, ask the question, and print what [`opencrayast_tools`] returned. It
//! implements **no** edit, plan or recovery logic: every decision about what is written, whether
//! it may be written, and what a refusal is called belongs to `opencrayast-edit` (L3), and the
//! spelling of the answer belongs to the six tool handlers in L4. A second implementation here
//! would be a second thing to get wrong, and this project has already paid for that shape once —
//! a test double that escaped its output while the shipping sink did not.
//!
//! # What is added on top of the tools, and why that is not a reimplementation
//!
//! Four things the tools deliberately do not do, because a tool call has no terminal:
//!
//! 1. **Confirmation.** `ast_edit_apply` writes when it is called; the *person* decides whether it
//!    is called. Invariant 3: interactive means an explicit confirmation, non-interactive without
//!    `--yes` means a refusal with **zero writes**.
//! 2. **Colour.** The tool handlers return plain text. The human page paints it, from a palette
//!    the caller injects. An ESC inside a file name is escaped to the seven characters `\u{1b}`
//!    before it can be painted, so no byte of file content can become colour.
//! 3. **The risk summary.** `EDIT-MODEL.md` §Risk summary names figures the tool output does not
//!    carry; they are read from the plan the tool just stored, not recomputed.
//! 4. **`recover`.** The tools expose it and take no arguments; here it is a subcommand, because a
//!    person told there was a crash may need to converge the workspace by hand.
//!
//! # Paths
//!
//! Nothing here prints an absolute path from inside the machine, and no file's content is ever
//! quoted. File names come from the plan (workspace-relative, refused by L3 if they contain a
//! control character) and are additionally escaped through [`Out`](crate::out::Out).

use crate::confirm::{self, Confirmer, Decision, Op, Request};
use crate::exit::{EXIT_OK, EXIT_USER, exit_code_for_error};
use crate::out::Out;
use crate::palette::{Colour, Palette};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::workspace::workspace_id;
use opencrayast_edit::{JournalStore, PlanStore, SystemClock, WriteCap};
use opencrayast_tools::context::{Mode, ToolContext};
use opencrayast_tools::{
    ApplyArgs, EditTools, PlanShowArgs, PreviewArgs, UndoArgs, ast_edit_apply, ast_edit_preview,
    ast_plan_show, ast_recover, ast_undo,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a write waits for the apply lock before answering `[busy]`.
///
/// A person is at the keyboard and can simply retry, so a few seconds beats telling them another
/// apply is running the moment they press Enter.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The shortest prefix `edit show` accepts for reading (EDIT-MODEL §Plan format).
const MIN_READ_PREFIX: usize = 10;

/// The length of a full plan id: `p-` plus 26 base32 characters.
const FULL_ID_CHARS: usize = 28;

/// The `edit` subcommands. The flags live on [`EditCmd`](crate::EditCmd); this is what runs them.
///
/// `Preview` is boxed because it is the only variant carrying a whole request: the other five are
/// a handful of `String`s and `Option`s, so keeping them inline would make every `match` over this
/// enum pay for the largest variant — a `Plan`-building struct is 360 bytes, and it is only ever
/// held for the length of one command.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Produce a plan and show it. Never writes the workspace.
    Preview(Box<PreviewArgs>),
    /// Show one stored plan: a human-readable, optionally coloured diff.
    Show {
        /// The full id, or an unambiguous prefix of at least ten characters (EDIT-MODEL E-15).
        plan_id: String,
        /// Show only this file's diff.
        file: Option<String>,
        /// First hunk to show (0-based).
        offset: Option<usize>,
        /// Maximum hunks to show.
        limit: Option<usize>,
    },
    /// Apply a plan, after the person said yes.
    Apply {
        /// The **full** plan id. A prefix is refused here (E-15).
        plan_id: String,
        /// `--yes`: consent given in advance. Handed to the gate, never read here.
        yes: bool,
    },
    /// Undo an applied plan, after the person said yes.
    Undo {
        /// The **full** plan id.
        plan_id: String,
        /// `--yes`: consent given in advance. Handed to the gate, never read here.
        yes: bool,
    },
    /// Converge any half-applied plan after a crash.
    Recover {
        /// `--yes`: consent given in advance. Handed to the gate, never read here.
        yes: bool,
    },
    /// List this workspace's stored plans: the same set `plan list` shows.
    List {
        /// How many plans to show.
        limit: Option<usize>,
    },
}

/// Everything the six handlers need, owned for the length of one command.
pub struct Ctx {
    /// The read context: boundary, limits, mode, workspace id.
    pub tools: ToolContext,
    /// Where plans are stored.
    pub plans: PlanStore,
    /// Where journals are stored.
    pub journals: JournalStore,
    /// The state directory, for the apply lock.
    pub state_dir: PathBuf,
}

/// Why a write was refused before the tools layer was reached.
///
/// Separate from [`ToolError`] because **none of these has happened yet**: no store was opened,
/// no file was read and nothing was written. There is deliberately **no `NotConfirmed` variant
/// here**: "nobody said yes" is [`crate::confirm`]'s answer to give, not this module's, and having
/// it in both places is how the two would drift — one gate, one home
/// ([`crate::confirm::authorize`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// No write capability: `--write` missing, or `[policy] allow_write` not true.
    WriteDisabled,
}

impl Refusal {
    /// The literal `[code] what is true` line, then the next step, then the CLI-only advice.
    fn lines(self) -> [String; 3] {
        match self {
            Refusal::WriteDisabled => [
                format!(
                    "[{}] writing is off for this run",
                    ErrorCode::WriteDisabled.as_str()
                ),
                "Next: pass --write and set policy.allow_write = true in the operator \
                 configuration; `opencrayast doctor` reports whether both are in force."
                    .to_string(),
                "Nothing was written. `edit preview`, `edit show` and `edit list` read only, and \
                 need neither --write nor --yes."
                    .to_string(),
            ],
        }
    }
}

/// Build the context the edit handlers need, or say why it cannot be built.
///
/// One place, so `edit preview` and `edit apply` cannot disagree about which limits, which state
/// directory or which boundary they are using.
fn context(
    root: &Path,
    settings: &opencrayast_core::config::Settings,
    read_roots: &[std::path::PathBuf],
    write: Option<WriteCap>,
    state: &crate::StateDir<'_>,
    config_source: &opencrayast_tools::ConfigSource,
) -> Result<Ctx, ToolError> {
    let ws = workspace_id(root)?;
    // The one resolver, shared with the MCP shell and with the boundary's own `state_dir` below,
    // so the directory the boundary protects is by construction the directory the stores write to.
    // A machine where it cannot be determined has no state directory at all, which is a refusal
    // rather than a fall back to the workspace.
    let state_dir = state.resolve()?;
    let boundary = opencrayast_core::boundary::Boundary::new(
        settings.boundary_config_with_read_roots(root, read_roots)?,
    )?;
    let clock = std::sync::Arc::new(SystemClock);
    let plans = PlanStore::open(&state_dir, &ws, settings.limits.clone(), clock.clone())?;
    let journals = JournalStore::open(&state_dir, &ws, settings.limits.clone(), clock)?;
    Ok(Ctx {
        tools: ToolContext {
            boundary,
            limits: settings.limits.clone(),
            mode: if write.is_some() {
                Mode::Write
            } else {
                Mode::ReadOnly
            },
            version: env!("CARGO_PKG_VERSION").to_string(),
            workspace_id: ws,
            respect_gitignore: true,
            extra_ignore: Vec::new(),
            write,
            config_source: config_source.clone(),
        },
        plans,
        journals,
        state_dir,
    })
}

/// The borrowed [`EditTools`] for one call.
fn tools<'a>(ctx: &'a Ctx) -> EditTools<'a> {
    EditTools {
        tools: &ctx.tools,
        plans: &ctx.plans,
        journals: &ctx.journals,
        state_dir: &ctx.state_dir,
        lock_timeout: LOCK_TIMEOUT,
    }
}

/// Run one `edit` subcommand. Returns the process exit code.
///
/// `write` is the capability minted from the **parsed** settings by the caller, or `None`.
#[allow(clippy::too_many_arguments)] // dispatcher: all params are distinct concerns, grouped into `ctx` below
pub fn run(
    cmd: &Edit,
    root: &Path,
    settings: &opencrayast_core::config::Settings,
    read_roots: &[std::path::PathBuf],
    write: Option<WriteCap>,
    config_source: &opencrayast_tools::ConfigSource,
    env: &mut crate::Env<'_>,
    out: &mut Out,
) -> i32 {
    // The write capability is decided **before** anything else, so a refused write never opens a
    // store, never reads a file and — trivially — cannot have written anything.
    if needs_write(cmd) && write.is_none() {
        return refuse(Refusal::WriteDisabled, out);
    }

    let ctx = match context(root, settings, read_roots, write, &env.state, config_source) {
        Ok(ctx) => ctx,
        Err(e) => return report(&e, out),
    };

    match cmd {
        Edit::Preview(args) => preview(&ctx, args, env.palette, out),
        Edit::Show {
            plan_id,
            file,
            offset,
            limit,
        } => show(
            &ctx,
            plan_id,
            file.as_deref(),
            *offset,
            *limit,
            env.palette,
            out,
        ),
        Edit::List { limit } => list(&ctx, *limit, out),
        Edit::Apply { plan_id, yes } => {
            run_write(WriteOp::Apply, &ctx, plan_id, *yes, env.confirmer, out)
        }
        Edit::Undo { plan_id, yes } => {
            run_write(WriteOp::Undo, &ctx, plan_id, *yes, env.confirmer, out)
        }
        Edit::Recover { yes } => recover(&ctx, *yes, env.confirmer, out),
    }
}

/// Does this command write the workspace?
fn needs_write(cmd: &Edit) -> bool {
    matches!(
        cmd,
        Edit::Apply { .. } | Edit::Undo { .. } | Edit::Recover { .. }
    )
}

// ---- preview -----------------------------------------------------------------------------------

/// `edit preview`: the plan, the human diff, the risk summary and the expiry.
///
/// Writes into the plan store and nowhere else — the tools layer gives preview no write
/// capability at all (E-12), and `needs_write` is false for this command, so there is nothing for
/// a capability to do here even when `--write` was passed.
fn preview(ctx: &Ctx, args: &PreviewArgs, palette: Palette, out: &mut Out) -> i32 {
    let text = match ast_edit_preview(&tools(ctx), args) {
        Ok(text) => text,
        Err(e) => return report(&e, out),
    };
    paint(&text, palette, out);
    // The tool's render is the contract (`docs/TOOLS.md`); it is printed as returned and painted,
    // never re-laid-out. A second renderer is the drift the tools module documents at length.
    if let Some(id) = plan_id_of(&text)
        && let Err(e) = risk_summary(ctx, &id, out)
    {
        // The summary is extra. If the store cannot be read back, that is worth saying, but the
        // preview itself succeeded, so it is a diagnostic and not a failed command.
        report(&e, out);
    }
    EXIT_OK
}

/// `HH:MM UTC` from epoch seconds, spelled the way `docs/TOOLS.md` writes it.
///
/// Duplicated from the tools layer on purpose rather than exported from it: L4's copy is private,
/// and reaching across layers for two lines of formatting would be a worse coupling than two
/// identical small functions. `cli2_spec` pins both to the same output.
fn hhmm_utc(epoch_seconds: u64) -> String {
    const DAY_SECONDS: u64 = 86_400;
    let within = epoch_seconds % DAY_SECONDS;
    format!("{:02}:{:02}", within / 3600, (within % 3600) / 60)
}

/// The plan id out of the tool's own first line: `plan p-…  (expires …)`.
///
/// Read from the text the handler returned rather than recomputed from the request, so the figure
/// a person reads is the one L4 printed. With no matches there is no `plan ` line and no id.
fn plan_id_of(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("plan ")?;
    rest.split_whitespace().next().map(str::to_string)
}

/// The risk summary `EDIT-MODEL.md` §Risk summary asks for, from the stored plan's own figures.
///
/// The plan is read back through [`PlanStore::get_for_read`] — the same store the tool wrote
/// through, and the same reader `edit show` uses. Nothing here recomputes a limit or a hash: an
/// edit that had changed would fail to match `post_hash` at apply, long after this line is read.
fn risk_summary(ctx: &Ctx, plan_id: &str, out: &mut Out) -> Result<(), ToolError> {
    let (plan, meta) = ctx.plans.get_for_read(plan_id)?;
    let edits: Vec<opencrayast_edit::Edit> = plan
        .files
        .iter()
        .flat_map(|f| f.edits.iter().cloned())
        .collect();
    let added = opencrayast_edit::bytes_added(&edits);
    let removed = opencrayast_edit::bytes_removed(&edits);
    let edit_count: usize = plan.files.iter().map(|f| f.edits.len()).sum();
    let named = |keep: fn(&opencrayast_edit::PlanFile) -> bool| -> Vec<String> {
        plan.files
            .iter()
            .filter(|f| keep(f))
            .map(|f| f.path.clone())
            .collect()
    };
    let pre_errors = named(|f| f.pre_errors > 0);
    let post_errors = named(|f| f.post_errors > 0);
    let say = |files: &[String]| -> String {
        if files.is_empty() {
            "none".to_string()
        } else {
            files.join(", ")
        }
    };

    out.line("");
    out.line("Risk summary");
    // U+2212 MINUS SIGN, the same character the tools layer writes for `-{removed}`, so the two
    // halves of one page are not spelled differently.
    out.line(&format!(
        "  files: {n}   edits: {edit_count}   bytes: +{added} \u{2212}{removed}",
        n = plan.files.len()
    ));
    out.line(&format!(
        "  expires: {} UTC (created {})",
        hhmm_utc(meta.expires_at),
        hhmm_utc(meta.created_at)
    ));
    out.line(&format!(
        "  files with syntax errors before: {}",
        say(&pre_errors)
    ));
    out.line(&format!(
        "  files with syntax errors after: {}",
        say(&post_errors)
    ));
    Ok(())
}

// ---- show --------------------------------------------------------------------------------------

/// `edit show <plan-id>`: the stored plan, a diff a person can read, and whether it is coloured.
///
/// Reading accepts a **prefix** (E-15). Which prefixes are legal, and which are ambiguous, is
/// decided by the handler — this only adds the one thing a handler cannot do without reading the
/// store twice: name the candidates when a prefix matches more than one plan.
fn show(
    ctx: &Ctx,
    plan_id: &str,
    file: Option<&str>,
    offset: Option<usize>,
    limit: Option<usize>,
    palette: Palette,
    out: &mut Out,
) -> i32 {
    if let Some(candidates) = ambiguous(ctx, plan_id) {
        out.diag(&format!(
            "[{}] {} stored plan(s) match this prefix.",
            ErrorCode::Ambiguous.as_str(),
            candidates.len()
        ));
        for c in &candidates {
            out.diag(&format!("  {c}"));
        }
        out.diag(&format!(
            "Next: pass more of the id. A full plan id is {FULL_ID_CHARS} characters."
        ));
        return EXIT_USER;
    }
    let args = PlanShowArgs {
        plan_id: plan_id.to_string(),
        file: file.map(str::to_string),
        offset,
        limit,
    };
    match ast_plan_show(&tools(ctx), &args) {
        Ok(text) => {
            paint(&text, palette, out);
            EXIT_OK
        }
        Err(e) => report(&e, out),
    }
}

/// The stored plans a read-only prefix names, when it names **more than one**.
///
/// `None` for a full id, for anything below the documented minimum prefix, and for an
/// unambiguous prefix: in each of those cases the handler is the authority and its answer is the
/// right one. Reads the store's listing only, never a plan body.
fn ambiguous(ctx: &Ctx, plan_id: &str) -> Option<Vec<String>> {
    if opencrayast_core::hash::is_full_plan_id(plan_id) {
        return None;
    }
    if !plan_id.starts_with("p-") || plan_id.len() < MIN_READ_PREFIX {
        return None;
    }
    let (plans, _unreadable) = ctx.plans.list().ok()?;
    let hits: Vec<String> = plans
        .into_iter()
        .map(|p| p.id)
        .filter(|id| id.starts_with(plan_id))
        .collect();
    if hits.len() > 1 { Some(hits) } else { None }
}

/// `edit list`: the same set `plan list` shows.
///
/// Not a second listing. [`crate::plan::list`] is called, so the sentence printed for an empty
/// store, the `--limit` handling and the unreadable-entry warning are the same code. What differs
/// is only that `edit list` may be called in write mode, which changes nothing about reading.
fn list(ctx: &Ctx, limit: Option<usize>, out: &mut Out) -> i32 {
    crate::plan::list(
        &ctx.tools.workspace_id,
        &ctx.state_dir,
        &ctx.tools.limits,
        limit.unwrap_or(crate::plan::DEFAULT_LIMIT),
        out,
    )
}

// ---- apply -------------------------------------------------------------------------------------

/// One write, after its plan id has been checked and its files known.
///
/// `apply` and `undo` differ in one thing each — where the file list comes from, and which handler
/// runs — and in every other respect they must behave identically: same full-id rule, same prompt,
/// same refusal text, same escape code. Written as one shape rather than two functions so a change
/// to the confirmation cannot land on one and miss the other. The confirmation itself is **not**
/// here: it is [`crate::confirm::authorize`], shared with every other write, so this module has no
/// second copy of the question, the file list or the refusal.
#[derive(Debug, Clone, Copy)]
enum WriteOp {
    /// `edit apply <plan-id>`.
    Apply,
    /// `edit undo <plan-id>`.
    Undo,
}

impl WriteOp {
    /// The confirmation vocabulary for this operation, so the subject, the prompt and the verb in the
    /// output all come from one place.
    fn op(self) -> Op {
        match self {
            WriteOp::Apply => Op::Apply,
            WriteOp::Undo => Op::Undo,
        }
    }

    /// The files this operation would change, or the error that stops it.
    ///
    /// Resolved **before** the prompt: `apply` reads them from the plan store (the reviewed intent)
    /// and `undo` from the journal (what was actually written), so the file list on screen and the
    /// files the handler touches are the same list, read at the same moment.
    fn files(self, ctx: &Ctx, plan_id: &str) -> Result<Vec<String>, ToolError> {
        match self {
            WriteOp::Apply => {
                let (plan, _) = ctx.plans.get_for_read(plan_id)?;
                Ok(plan.files.iter().map(|f| f.path.clone()).collect())
            }
            WriteOp::Undo => {
                let manifest = ctx.journals.load(plan_id)?;
                Ok(manifest.files.iter().map(|f| f.path.clone()).collect())
            }
        }
    }

    /// Run the write.
    fn execute(self, ctx: &Ctx, plan_id: &str) -> Result<String, ToolError> {
        match self {
            WriteOp::Apply => ast_edit_apply(
                &tools(ctx),
                &ApplyArgs {
                    plan_id: plan_id.into(),
                },
            ),
            WriteOp::Undo => ast_undo(
                &tools(ctx),
                &UndoArgs {
                    plan_id: plan_id.into(),
                },
            ),
        }
    }
}

/// `edit apply <plan-id>` and `edit undo <plan-id>`.
///
/// The order is the point. The plan id is checked first — so nobody is asked to approve a change
/// that was never going to happen — then the exact list of files is resolved, then the gate decides
/// ([`crate::confirm::authorize`] prints that list and asks), and only on `Proceed` is the handler
/// called. A person who says no has already seen what they are refusing, and nothing has been
/// written.
fn run_write(
    op: WriteOp,
    ctx: &Ctx,
    plan_id: &str,
    yes: bool,
    confirmer: &mut dyn Confirmer,
    out: &mut Out,
) -> i32 {
    let full = match full_id(plan_id, op.op().subject()) {
        Ok(id) => id,
        Err(e) => return report(&e, out),
    };
    // Expiry is checked first, because the tools layer can only report it once it is asked to
    // write, and this command has not written anything yet. `expires_at` is the store's own
    // envelope field, read through the store's own reader, so this compares against the same clock
    // L3 uses rather than forming a second opinion about time. The read handle is dropped before
    // the write, so it never makes the plan "in use" and cannot hold it past its own expiry.
    if let Err(e) = expired(ctx, &full) {
        return report(&e, out);
    }
    let files = match op.files(ctx, &full) {
        Ok(files) => files,
        Err(e) => return report(&e, out),
    };
    // The one decision point: `authorize` prints what will change, asks, and says yes or no.
    // Nothing below runs on a refusal, so nothing below has written anything.
    let request = Request::of(op.op(), &full, &files, yes);
    if let Decision::Refused(r) = confirm::authorize(out, &request, confirmer) {
        return r.exit_code();
    }
    match op.execute(ctx, &full) {
        Ok(text) => {
            out.line("");
            // Line by line, not as one string: `Out` escapes a newline, so printing a handler's
            // multi-line answer whole would show the reader a single line of literal `\n`.
            for line in text.lines() {
                out.line(line);
            }
            EXIT_OK
        }
        Err(e) => report(&e, out),
    }
}

/// Whether the stored plan for `plan_id` has passed its expiry, as `[plan_expired]`.
///
/// Only asked about a plan that is **not in the listing**: [`opencrayast_edit::PlanStore::list`]
/// drops expired plans, so a plan missing from `edit list` is either expired or was never there. A
/// plan that is genuinely absent is not an error here — the resolution below reports that with the
/// store's own `[plan_not_found]` and its own next step, so one condition does not get two
/// different messages from the CLI and the tools.
fn expired(ctx: &Ctx, plan_id: &str) -> Result<(), ToolError> {
    if ctx
        .plans
        .list()
        .is_ok_and(|(live, _)| live.iter().any(|p| p.id == plan_id))
    {
        return Ok(());
    }
    let (_plan, meta) = ctx.plans.get_for_read(plan_id)?;
    if opencrayast_edit::Clock::now_secs(&SystemClock) < meta.expires_at {
        return Ok(());
    }
    Err(ToolError::new(
        ErrorCode::PlanExpired,
        "plan has expired",
        "Preview again; expired plans cannot be applied (EDT-06).",
    ))
}

/// `edit recover`: the one write with no id and no file list, because there is no single plan to
/// name (EDIT-MODEL §Plan format). Still confirmed, because it writes — through the same
/// [`crate::confirm::authorize`] as `apply` and `undo`, with an empty file list.
fn recover(ctx: &Ctx, yes: bool, confirmer: &mut dyn Confirmer, out: &mut Out) -> i32 {
    let request = Request::recover(yes);
    if let Decision::Refused(r) = confirm::authorize(out, &request, confirmer) {
        return r.exit_code();
    }
    match ast_recover(&tools(ctx)) {
        Ok(text) => {
            for line in text.lines() {
                out.line(line);
            }
            EXIT_OK
        }
        Err(e) => report(&e, out),
    }
}

/// Report a refusal this module decided on its own, before anything was attempted. Returns the exit
/// code.
///
/// Only the write-capability refusal lives here — "nothing confirmed it" is
/// [`crate::confirm`]'s to say, in one place, and this function has no `advice` parameter to
/// re-spell it. The lines it prints carry no path from inside the machine and no file's contents, so
/// a refusal cannot become a channel for either.
fn refuse(refusal: Refusal, out: &mut Out) -> i32 {
    let [what, next, tail] = refusal.lines();
    out.diag(&what);
    out.diag(&next);
    out.diag(&tail);
    exit_code_for_error(&ToolError::new(refusal.code(), "", ""))
}

impl Refusal {
    /// The error code this refusal exits with.
    ///
    /// `write_disabled` is the code `docs/TOOLS.md` names for a write tool reached while writing is
    /// off, and it is in the environment bucket, so it is exit 2: the machine cannot do this now and
    /// the same command may work once the configuration is fixed.
    fn code(self) -> ErrorCode {
        match self {
            Refusal::WriteDisabled => ErrorCode::WriteDisabled,
        }
    }
}

/// A full plan id, or the refusal to accept a prefix.
///
/// Writing takes the whole id (E-15): a 50-bit prefix is a convenience for reading, never an
/// authority for writing. The message spells the tools' own `invalid_args` rather than inventing a
/// code, because it is the same condition and a caller reading both surfaces should not have to
/// know which layer answered.
fn full_id(candidate: &str, command: &str) -> Result<String, ToolError> {
    let id = candidate.trim();
    if opencrayast_core::hash::is_full_plan_id(id) {
        return Ok(id.to_string());
    }
    Err(ToolError::new(
        ErrorCode::InvalidArgs,
        if id.chars().count() < FULL_ID_CHARS {
            format!("{command} needs the full plan id; this is a prefix.")
        } else {
            "This is not a plan id.".to_string()
        },
        format!(
            "Pass all {FULL_ID_CHARS} characters from `edit preview`. Abbreviations are accepted \
             by `edit show`, never by a command that writes."
        ),
    ))
}

/// Print an error the way the tools spell it: the literal code, what is true, then what to do.
///
/// The code is the layer's own, and the exit status is derived from it — never the other way
/// round, which is the defect CLI1-06's comment describes.
fn report(e: &ToolError, out: &mut Out) -> i32 {
    out.diag(&format!("[{}] {}", e.code.as_str(), e.message));
    if !e.next.is_empty() {
        out.diag(&format!("Next: {}", e.next));
    }
    exit_code_for_error(e)
}

// ---- colour ------------------------------------------------------------------------------------

/// Write `text` a line at a time, painting the diff lines.
///
/// Painting is by **role**, decided from the unified-diff marker, and never from the content of a
/// line beyond that one character. So a file whose contents begin with `+` is not painted by its
/// own text: the marker is only read at the start of a line the tool rendered as a diff, and the
/// text itself has already been escaped.
fn paint(text: &str, palette: Palette, out: &mut Out) {
    for line in text.lines() {
        out.coloured(line, Colour::for_diff_line(line), palette);
    }
}
