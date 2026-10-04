//! What every handler needs: the boundary, the limits and the mode.

use opencrayast_core::boundary::Boundary;
use opencrayast_core::limits::Limits;

/// Whether write tools are available (docs/TOOLS.md "Modes and annotations").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default: no write tool is exposed.
    ReadOnly,
    /// Write tools exposed (the operator enabled them and policy allows it).
    Write,
}

impl Mode {
    /// `read-only` or `write`.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ReadOnly => "read-only",
            Mode::Write => "write",
        }
    }
}

/// Where the effective configuration came from.
///
/// A process that is running with limits, a write policy or extra read roots it cannot explain is
/// a process nobody can audit. This is what `ast_info` prints so the answer is in-band: the user
/// has no other way to find out that the file being honoured is one they did not expect (a
/// `--config` pointing into the repository is the case that matters — see
/// `docs/CONFIGURATION.md` §Sources and precedence, which documents why `--config` is honoured
/// wherever it points and what visibility that obliges us to provide).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ConfigSource {
    /// The documented user-level location, and no file was there. The defaults are in force.
    #[default]
    Defaults,
    /// The documented user-level location, and a file was read from it.
    UserFile(String),
    /// An explicit `--config <path>`, which was read. The path is the operator's own argument.
    ExplicitFile(String),
}

impl From<opencrayast_core::config::ConfigLocation> for ConfigSource {
    fn from(loc: opencrayast_core::config::ConfigLocation) -> ConfigSource {
        use opencrayast_core::config::ConfigLocation as L;
        match loc {
            L::Defaults => ConfigSource::Defaults,
            L::UserFile(p) => ConfigSource::UserFile(p.to_string_lossy().into_owned()),
            L::ExplicitFile(p) => ConfigSource::ExplicitFile(p.to_string_lossy().into_owned()),
        }
    }
}

impl ConfigSource {
    /// The one-line form `ast_info` prints.
    pub fn describe(&self) -> String {
        match self {
            Self::Defaults => "defaults (no user configuration file found)".to_string(),
            Self::UserFile(p) => format!("user file: {}", display_config_path(p)),
            Self::ExplicitFile(p) => format!("--config: {}", display_config_path(p)),
        }
    }

    /// Is this a file the operator named on the command line?
    ///
    /// `doctor` warns when it is — not because it is forbidden (it is not), but because a file
    /// inside the workspace is one a repository can ship, and the operator should be told when
    /// the running configuration is such a file.
    pub fn is_explicit(&self) -> bool {
        matches!(self, Self::ExplicitFile(_))
    }

    /// The path, if a file was actually read.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Defaults => None,
            Self::UserFile(p) | Self::ExplicitFile(p) => Some(p),
        }
    }
}

/// A configuration path, shown with its last two components rather than in full.
///
/// This is a path from inside the machine, read out of the operator's own command line or
/// environment, and `ast_info` is a response an agent sees. The rule is the same one the CLI's
/// `doctor` uses for `--workspace`: show enough to recognise the file, not enough to enumerate
/// the filesystem. A leading ellipsis rather than a stripped slash, so `…/opencrayast/config.toml`
/// cannot be misread as a workspace-relative path.
fn display_config_path(p: &str) -> String {
    let path = std::path::Path::new(p);
    let components: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match components.len() {
        0 => "?".to_string(),
        1 | 2 => components.join("/"),
        _ => format!(
            "…/{}/{}",
            components[components.len() - 2],
            components[components.len() - 1]
        ),
    }
}

/// The environment of one tool call.
pub struct ToolContext {
    /// The one path policy.
    pub boundary: Boundary,
    /// Effective, already validated limits.
    pub limits: Limits,
    /// Read-only or write.
    pub mode: Mode,
    /// Version string printed by `ast_info`, e.g. `0.20261002.1`.
    pub version: String,
    /// The workspace id (`w-` + 32 hex), from `core::workspace::workspace_id`.
    pub workspace_id: String,
    /// Honour `.gitignore` files while walking directories.
    pub respect_gitignore: bool,
    /// Extra ignore globs from the user configuration.
    pub extra_ignore: Vec<String>,
    /// The write capability for this call, minted by the shell from a configuration-issued
    /// `WritePermission`. `None` means writing is off, and the three write handlers answer
    /// `[write_disabled]` (TOOLS.md §Modes; the catalogue still hides them unless `mode`
    /// is `Mode::Write`, which is a separate question).
    pub write: Option<opencrayast_edit::WriteCap>,
    /// Where the effective configuration was read from. `ast_info` prints it.
    pub config_source: ConfigSource,
}
