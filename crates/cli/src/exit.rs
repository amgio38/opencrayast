//! Exit codes and the mapping from a [`ToolError`] to one of them.
//!
//! The mapping is a contract, not an implementation detail: a script wrapping `opencrayast` decides
//! whether to retry, and the only signal it has is the exit code. It is written into `--help` (see
//! [`EXIT_CODE_HELP`]) and pinned by CLI1-04, so the two cannot drift.

use opencrayast_core::error::{ErrorCode, ExitClass, ToolError};

/// Success.
///
/// The NUMBERS are the contract, not an implementation detail: a script branches on them, so a
/// test that only asserted "these three constants differ" would pass even if 1 and 2 were swapped
/// (CR R2 — that mutation was green against the first version of this suite). [`EXIT_USER`] and
/// [`EXIT_ENV`] are therefore pinned to their digits below and in `cli1_spec`.
pub const EXIT_OK: i32 = 0;
/// The user asked for something that cannot be: a bad argument, an id that does not resolve, a
/// plan that is gone. Retrying unchanged will not help. **Exit status 1.**
pub const EXIT_USER: i32 = 1;
/// The machine cannot do it now: permissions, a missing state directory, a busy lock, a
/// limit. The same command may work later, or elsewhere. **Exit status 2.**
pub const EXIT_ENV: i32 = 2;

/// The exit-code column of `--help`, kept next to the mapping it documents.
pub const EXIT_CODE_HELP: &str = "\
Exit codes:
  0  success
  1  user error      bad arguments, or the id/plan asked for does not resolve
                    (invalid_args, invalid_pattern, invalid_edit, plan_not_found,
                     plan_expired, plan_corrupt, wrong_workspace, already_applied,
                     stale_plan, gate_failed, diverged, comment_loss, journal_missing,
                     rollback_incomplete,
                     not_found, ambiguous, unsupported_language)
  2  environment     the machine cannot do it now: permissions, a missing or
                    unusable state directory, a held lock, a limit
                    (io_error, outside_workspace, protected_path, write_disabled,
                     busy, limit_exceeded, unsupported_target, replaced_not_durable,
                     file_too_large, not_utf8, budget_exceeded, timeout,
                     config_untrusted, internal)";

/// What the write commands require, in the same place as the exit codes.
///
/// Written as the truth rather than as a wish: `confirm::REFUSAL_NEXT` and the two messages in
/// `confirm::refuse` name the same command, so a person who follows the next step in `--help` lands
/// on the command that works. `cli2_confirm_spec` asserts the overlap, so this text cannot drift
/// from the messages into advising a command that does not exist.
pub const CONFIRM_HELP: &str = "\
Confirmation (edit apply, undo, recover):
  A plan is only changed when a person has agreed to it. At a terminal, each of those
  commands lists the files it would change and then asks; answering anything but y/n
  leaves the workspace untouched. Where there is no terminal to ask — a pipe, cron,
  CI — there is nobody to answer, so the command refuses with [invalid_args] and exit
  1 unless --yes was given. Nothing is written in either refusal.

  --yes means a person has already read this plan. It is the only way to write
  unattended, and it is not a default: without it a non-interactive write does not
  happen.";

/// How a write command is reached at all: two gates, and neither is enough alone.
///
/// `--write` is the **first** of two; the operator's configuration must also say
/// `policy.allow_write = true`. `opencrayast doctor` reports whether both are in force.
pub const WRITE_HELP: &str = "\
Write mode needs two gates, and neither is enough on its own:
  --write                  the flag on this command line, and
  policy.allow_write = true    in the operator configuration (CONFIGURATION.md, CFG-06).
Without both, `edit apply`, `edit undo` and `edit recover` refuse with
[write_disabled] and exit 2, having opened no store and written nothing.
`opencrayast doctor` reports whether both are in force.";

/// When SGR bytes are written, and by whom.
///
/// `docs/TOOLS.md` §Output sanitising states the rule the code implements: colour comes only from
/// the tool, never from file content. `Palette::from_env` decides; `Out` applies it after escaping.
pub const COLOR_HELP: &str = "\
Colour:
  --color auto (default) colours only on a terminal, and never when NO_COLOR is set
  (any value, including empty) or TERM=dumb. --color always and --color never
  override that. Colour is applied by the CLI after escaping, so no byte of a file's
  contents can become a terminal sequence.";

/// [`HELP_TAIL`] repeats these three as literals, because clap's `after_help` only accepts one
/// compile-time literal. This function is the single reader of that repetition, and `cli2_spec`
/// calls it, so the copies cannot drift without a test going red.
pub fn help_section(name: &str) -> &'static str {
    match name {
        "confirm" => CONFIRM_HELP,
        "write" => WRITE_HELP,
        "color" => COLOR_HELP,
        _ => "",
    }
}

/// Everything `--help` prints after the options: the exit codes, then the confirmation, write-mode
/// and colour rules.
///
/// One string rather than several, because clap's `after_help` only accepts a literal and the
/// sections must appear together anyway — a reader deciding whether to pipe this command needs the
/// exit code a refusal produces, what makes it refuse, and what it takes to be allowed to write.
pub const HELP_TAIL: &str = concat!(
    "\
Exit codes:
  0  success
  1  user error      bad arguments, or the id/plan asked for does not resolve
                    (invalid_args, invalid_pattern, invalid_edit, plan_not_found,
                     plan_expired, plan_corrupt, wrong_workspace, already_applied,
                     stale_plan, gate_failed, diverged, comment_loss, journal_missing,
                     rollback_incomplete,
                     not_found, ambiguous, unsupported_language)
  2  environment     the machine cannot do it now: permissions, a missing or
                    unusable state directory, a held lock, a limit
                    (io_error, outside_workspace, protected_path, write_disabled,
                     busy, limit_exceeded, unsupported_target, replaced_not_durable,
                     file_too_large, not_utf8, budget_exceeded, timeout,
                     config_untrusted, internal)

",
    "\
Confirmation (edit apply, undo, recover):
  A plan is only changed when a person has agreed to it. At a terminal, each of those
  commands lists the files it would change and then asks; answering anything but y/n
  leaves the workspace untouched. Where there is no terminal to ask — a pipe, cron,
  CI — there is nobody to answer, so the command refuses with [invalid_args] and exit
  1 unless --yes was given. Nothing is written in either refusal.

  --yes means a person has already read this plan. It is the only way to write
  unattended, and it is not a default: without it a non-interactive write does not
  happen.

",
    "\
Write mode needs two gates, and neither is enough on its own:
  --write                  the flag on this command line, and
  policy.allow_write = true    in the operator configuration (CONFIGURATION.md, CFG-06).
Without both, `edit apply`, `edit undo` and `edit recover` refuse with
[write_disabled] and exit 2, having opened no store and written nothing.
`opencrayast doctor` reports whether both are in force.

",
    "\
Colour:
  --color auto (default) colours only on a terminal, and never when NO_COLOR is set
  (any value, including empty) or TERM=dumb. --color always and --color never
  override that. Colour is applied by the CLI after escaping, so no byte of a file's
  contents can become a terminal sequence."
);

/// The exit code for an error code.
///
/// Every variant is classified by [`ErrorCode::exit_class`] in `opencrayast-core`, and this
/// function is only the mapping from that class to this shell's digits. Delegating is the point:
/// `opencrayast-mcp` is a separate binary that the layering rules keep from depending on the CLI,
/// so if the classification lived here it would have to be written down twice — and the two
/// copies are exactly how the shells came to disagree about every configuration refusal.
pub fn exit_code_for(code: ErrorCode) -> i32 {
    match code.exit_class() {
        ExitClass::User => EXIT_USER,
        ExitClass::Environment => EXIT_ENV,
    }
}

/// The exit code for an error.
pub fn exit_code_for_error(e: &ToolError) -> i32 {
    exit_code_for(e.code)
}
