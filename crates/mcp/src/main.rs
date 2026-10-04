//! `opencrayast-mcp` — stdio JSON-RPC / MCP transport.
//!
//! stdout is the protocol wire. Every log line goes to stderr. See
//! `docs/ARCHITECTURE.md` (request lifecycle).
//!
//! # Exit codes
//!
//! This shell has its own, smaller table than the CLI's, and it is documented in
//! `docs/CONFIGURATION.md` §Exit codes. The *classification* is shared — it comes from
//! [`opencrayast_core::error::ErrorCode::exit_class`], so "is this the user's fault or the
//! machine's" is decided in one place and both shells decide it the same way — but the digits
//! differ, because this binary's exit status is also the status of a server a client launched:
//!
//! | Code | Class | Status | Means |
//! |---|---|---|---|
//! | — | success | 0 | The server ran, or `--help` printed usage |
//! | usage | user | 2 | Bad or missing arguments; `--help` reached on a non-terminal client |
//! | — | user | 3 | A request the server understood and refused — including a **malformed** configuration file |
//! | — | environment | 4 | The machine cannot do it: a **configuration file that cannot be trusted**, a bad state directory, an unreadable workspace |
//! | — | unexpected | 1 | The transport failed unexpectedly; the message says what |
//!
//! 2, 3 and 4 follow the convention that a distinct non-zero status per failure class is easier
//! for a supervisor to act on than a single 1, and they are chosen to leave `1` for "this process
//! fell over", which is the one case a wrapper cannot interpret from anything but the log. Before
//! this was documented every refusal was `1`, including a configuration file that could not be
//! trusted — so the CLI's 2 and this shell's 1 disagreed about every one of them, with nothing on
//! either side to be consistent with.

use std::env;
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

use opencrayast_core::boundary::Boundary;
use opencrayast_core::config::load_or_default;
use opencrayast_core::error::{ErrorCode, ExitClass, ToolError};
use opencrayast_core::render::escape_inline;
use opencrayast_core::statedir::user_state_dir;

use opencrayast_core::workspace::workspace_id;
use opencrayast_mcp::{ServerConfig, serve};
use opencrayast_tools::{Mode, ToolContext, WriteCap};

/// The process exited with a [`RunError`].
const EXIT_UNEXPECTED: u8 = 1;
/// Bad or missing arguments (`parse_args`).
const EXIT_USAGE: u8 = 2;
/// A [`ToolError`] that is the user's to fix.
const EXIT_USER: u8 = 3;
/// A [`ToolError`] about this machine — permissions, a bad path, a configuration file.
const EXIT_ENVIRONMENT: u8 = 4;

/// This shell's exit status for an [`ErrorCode`].
///
/// The classification comes from the shared [`ErrorCode::exit_class`]; only the digits are this
/// binary's own, for the reasons in the module docs.
fn exit_code_for(code: ErrorCode) -> u8 {
    match code.exit_class() {
        ExitClass::User => EXIT_USER,
        ExitClass::Environment => EXIT_ENVIRONMENT,
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(RunError::Help) => ExitCode::SUCCESS,
        Err(RunError::Usage(e)) => {
            // Failures before / during serve must never touch stdout.
            eprintln!("opencrayast-mcp: {}", escape_inline(&e).0);
            ExitCode::from(EXIT_USAGE)
        }
        Err(RunError::Refused(e)) => {
            eprintln!("opencrayast-mcp: {}", report(&e));
            ExitCode::from(exit_code_for(e.code))
        }
        Err(RunError::Transport(e)) => {
            eprintln!("opencrayast-mcp: {}", escape_inline(&e).0);
            ExitCode::from(EXIT_UNEXPECTED)
        }
    }
}

/// A [`ToolError`] as one sanitised line.
///
/// A refusal quotes text this process read out of the operator's configuration file — a key, a
/// section name — and stderr is a terminal. `Op::diag` in the CLI escapes such text before it is
/// written; this shell has no `Out`, and its messages used to reach stderr raw, so a key spelled
/// `max_\u{1b}[31mRED\u{1b}[0mults` put a real SGR sequence on the operator's screen. Escaping
/// here covers both halves of the message and the `Next:` line in one place.
///
/// `escape_inline` output is pure ASCII with no control characters, so the result cannot itself
/// carry a sequence, and re-escaping it would be a no-op.
fn report(e: &ToolError) -> String {
    let (message, next) = (escape_inline(&e.message).0, escape_inline(&e.next).0);
    format!("[{}] {message} Next: {next}", e.code.as_str())
}

enum RunError {
    /// `--help` was asked for; it printed usage on stderr and this is a success.
    Help,
    /// The command line itself was wrong.
    Usage(String),
    /// A [`ToolError`], carrying both the message and the class to exit with.
    Refused(ToolError),
    /// The transport failed, or something unforeseen. Not a classified refusal.
    Transport(String),
}

impl RunError {
    /// A classified refusal from anywhere on the startup path.
    fn refused(e: ToolError) -> RunError {
        RunError::Refused(e)
    }
}

fn run() -> Result<(), RunError> {
    let args = parse_args(env::args().skip(1))?;
    let root = args.workspace;
    // THE configuration load, on the shipping path. This used to be `Settings::default()`,
    // which meant the operator's file was never read by anything that ships: `Settings::load`
    // existed, was tested, and had zero callers outside tests (CFG 1, CR round 1).
    //
    // A missing file is fine and yields the defaults. A file that EXISTS and is malformed,
    // world-writable, or owned by someone else is NOT fine: the server refuses to start
    // rather than silently running on limits the operator did not ask for. Falling back here
    // would be the same defect wearing a different hat — an operator who wrote
    // `path_max_depth = 4` and then hit a permissions error would get 64, silently.
    let settings = load_or_default(args.config.as_deref()).map_err(RunError::refused)?;
    // Which file produced those settings, decided by the same function `load_or_default` used.
    // `--config` is honoured wherever it points — including inside the workspace — so the only
    // way a user finds out which file is in force is being told.
    let config_location = opencrayast_core::config::config_location(args.config.as_deref());
    // `--read-root DIR` (repeatable) widens what may be READ, never what may be written. Each root
    // goes through the same `check_root` the workspace root does (boundary.rs), so `/`, a drive
    // root, the home directory itself and credential directories are refused here exactly as they
    // are for `--workspace` — the refusal is not a second, weaker list.
    let boundary = Boundary::new(
        settings
            .boundary_config_with_read_roots(&root, &args.read_roots)
            .map_err(RunError::refused)?,
    )
    .map_err(RunError::refused)?;
    let id = workspace_id(&root).map_err(RunError::refused)?;
    // CFG-06 / T-32: write mode needs the flag AND `policy.allow_write = true`.
    //
    // Both halves come from the one file `load_or_default` above read: a **user-level** config
    // (`--config <path>`, else `$XDG_CONFIG_HOME`/`$HOME/.config/opencrayast/config.toml`). This
    // binary reads **no** project-level client config at all — there is no `.mcp.json` lookup
    // anywhere in it — so nothing inside the repository under `--workspace` can turn writing on,
    // and a checked-in file cannot influence the policy. The consequence worth stating plainly:
    // `--allow-write` alone has always done nothing, and `--allow-write` plus a repository file
    // does nothing either.

    let write_requested = args.allow_write && settings.policy.allow_write;
    let mode = if write_requested {
        Mode::Write
    } else {
        Mode::ReadOnly
    };
    // WCAP-1: the capability is minted here — from the parsed configuration, and nowhere else.
    // `Mode::Write` is a public enum any dependent can write, so it is not a capability; this is.
    let write = if write_requested {
        settings.write_permission().map(WriteCap::mint)
    } else {
        None
    };
    let ctx = ToolContext {
        boundary,
        limits: settings.limits,
        mode,
        write,
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_id: id,
        respect_gitignore: true,
        extra_ignore: Vec::new(),
        config_source: config_location.into(),
    };
    let cfg = ServerConfig {
        ctx,
        version: env!("CARGO_PKG_VERSION").to_string(),
        // One determined value, shared with the CLI and with the boundary's `state_dir`: the
        // platform user-state base (`$XDG_STATE_HOME/opencrayast`, `~/.local/state/opencrayast`,
        // `%LOCALAPPDATA%\opencrayast`). Resolved here, not per call, so every tool in this
        // process sees the same store — and so the store is the very directory the write policy
        // refuses as a target. It is not created until a plan tool is actually called: a
        // read-only server that only ever runs `ast_outline` leaves no state directory behind.
        //
        // `workspace_id` is what keeps two workspaces apart; this value is shared and the `ws-<id>`
        // segment is appended by the stores, so it carries no workspace of its own.
        state_dir: user_state_dir().map_err(RunError::refused)?,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let mut output = stdout.lock();
    serve(&cfg, &mut input, &mut output).map_err(|e| RunError::Transport(e.to_string()))
}

struct Args {
    workspace: PathBuf,
    allow_write: bool,
    /// `--config <path>`; `None` means "the documented location".
    config: Option<String>,
    /// `--read-root DIR`, repeatable. Extra READ-ONLY roots (never writable, BND-19).
    read_roots: Vec<PathBuf>,
}

fn parse_args<I>(args: I) -> Result<Args, RunError>
where
    I: IntoIterator<Item = String>,
{
    let mut workspace: Option<PathBuf> = None;
    let mut allow_write = false;
    let mut config: Option<String> = None;
    // Repeated, in the order given: the order IS the `@root<N>` label order (boundary.rs
    // `root_at`), so it must be preserved rather than sorted or deduplicated.
    let mut read_roots: Vec<PathBuf> = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--workspace" => {
                let value = iter
                    .next()
                    .ok_or_else(|| RunError::Usage("missing value for --workspace".into()))?;
                workspace = Some(PathBuf::from(value));
            }
            "--allow-write" => allow_write = true,
            "--read-root" => {
                let value = iter
                    .next()
                    .ok_or_else(|| RunError::Usage("missing value for --read-root".into()))?;
                read_roots.push(PathBuf::from(value));
            }
            "--config" => {
                let value = iter
                    .next()
                    .ok_or_else(|| RunError::Usage("missing value for --config".into()))?;
                config = Some(value);
            }
            "--help" | "-h" => {
                // Help text on stderr so a curious client does not poison stdout.
                eprintln!(
                    "Usage: opencrayast-mcp --workspace <dir> [--allow-write] [--read-root DIR]...\n\
                     Speaks MCP over stdio. Logs go to stderr.\n\
                     --read-root is repeatable and adds a READ-ONLY root (@root1, @root2, ...);\n\
                     it never grants write."
                );
                return Err(RunError::Help);
            }
            other => {
                return Err(RunError::Usage(format!("unknown argument: {other}")));
            }
        }
    }
    let workspace = workspace.ok_or_else(|| {
        RunError::Usage("missing --workspace <dir> (the only root this process may read)".into())
    })?;
    Ok(Args {
        workspace,
        allow_write,
        config,
        read_roots,
    })
}
