//! `ast_info` (docs/TOOLS.md).

use crate::context::{Mode, ToolContext};

/// What is running and what it will allow. Exactly six lines:
///
/// ```text
/// opencrayast 0.20261002.1 (mode: read-only)
/// workspace: . (id w-5c1e9a07...)
/// languages: rust (tier 1), typescript (tier 1), tsx (tier 1), javascript (tier 1), python (tier 1), go (tier 1)
/// limits: file 4 MiB · output 64 KiB · results 200 · plan 50 files / 500 edits
/// config: defaults (no user configuration file found)
/// write: disabled (needs BOTH --allow-write AND policy.allow_write = true in the server config; together they expose ast_edit_apply, ast_undo, ast_recover)
/// ```
///
/// The `config:` line was added when `--config` was ruled to be honoured **wherever it points**,
/// including inside the workspace (documented in `docs/CONFIGURATION.md` §Sources and precedence).
/// It sits before the `write:` line because the write state is the thing most worth checking, and
/// the two together answer "why is this process running like this". A process on a configuration
/// nobody can name is a process nobody can audit.
///
/// `languages` lists only languages whose grammar is built in (`Language::is_available`), in
/// `Language::all()` order. Byte sizes print as `N MiB` when a multiple of 1 MiB, else `N KiB`
/// when a multiple of 1 KiB, else `N B`. The separator is U+00B7 surrounded by spaces. With
/// `Mode::Write` the last line is `write: enabled (ast_edit_apply, ast_undo, ast_recover)`.
pub fn ast_info(ctx: &ToolContext) -> String {
    use opencrayast_lang::Language;

    let languages: Vec<String> = Language::all()
        .iter()
        .filter(|l| l.is_available())
        .map(|l| format!("{} (tier {})", l.id(), l.tier()))
        .collect();
    let write = match ctx.mode {
        Mode::ReadOnly => {
            "write: disabled (needs BOTH --allow-write AND policy.allow_write = true in the \
             server config; together they expose ast_edit_apply, ast_undo, ast_recover)"
        }
        Mode::Write => "write: enabled (ast_edit_apply, ast_undo, ast_recover)",
    };
    // The separator is U+00B7 with spaces around it, not a hyphen and not a pipe: this line is
    // read by people and by scripts, and the exact bytes are the contract.
    format!(
        "opencrayast {version} (mode: {mode})\n\
         workspace: . (id {id})\n\
         languages: {languages}\n\
         limits: file {file} · output {output} · results {results} · plan {plan_files} files / \
         {plan_edits} edits\n\
         config: {config}\n\
         {write}\n",
        version = ctx.version,
        mode = ctx.mode.as_str(),
        id = ctx.workspace_id,
        languages = languages.join(", "),
        file = bytes(ctx.limits.max_file_bytes),
        output = bytes(ctx.limits.max_output_bytes),
        results = ctx.limits.max_results,
        plan_files = ctx.limits.plan_max_files,
        plan_edits = ctx.limits.plan_max_edits,
        config = ctx.config_source.describe(),
    )
}

/// A byte count as `N MiB`, `N KiB` or `N B`.
///
/// The largest unit that divides the count EXACTLY, so a limit never prints as `3.5 MiB`: the
/// numbers are compared against configuration and hard maxima by people reading them, and a
/// rounded figure there would be a figure that is not the limit.
fn bytes(n: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if n.is_multiple_of(MIB) {
        format!("{} MiB", n / MIB)
    } else if n.is_multiple_of(KIB) {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
}
