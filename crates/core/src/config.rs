//! The user-level configuration file: the one place a `[limits]` value becomes a
//! [`Limits`](crate::limits::Limits) the rest of the program uses.
//!
//! # Why this module exists
//!
//! `BoundaryConfig` grew a `limits` field and both path checks read it through
//! `Boundary::limits()`. Then nothing filled it in: there was no parser anywhere in
//! `crates/*/src`, so every caller reached for `Default::default()` and the operator's
//! settings went nowhere. `CONFIGURATION.md` promised the setting bound; there was no
//! code path for it to bind through. **The container existed and nobody poured into it.**
//!
//! So this is the pour. [`Settings::parse`] turns the text of a configuration file into
//! [`Settings`], and [`Settings::boundary_config`] is the single place that decides which
//! limits a `Boundary` is held to. One function, so a shell cannot pick its own.
//!
//! # Scope, stated honestly
//!
//! This is a deliberately small reader for the subset the project documents, not a TOML
//! implementation: `[section]` headers, `key = <integer>` lines, `#` comments, and blank
//! lines. It does **not** support nested tables, arrays, inline tables, floats, strings,
//! booleans or datetimes, and a line it cannot parse is a refusal rather than a guess —
//! the same rule as everywhere else in this codebase. Adding a real TOML dependency is a
//! decision for the maintainers, not something to slip in here.
//!
//! Security properties, from SECURITY-MODEL T-18 and T-20:
//!
//! - The file is **never** read from the workspace. There is no repo-local configuration
//!   and no search upward: the caller passes the path, and [`load`] refuses a file that
//!   is group- or world-accessible or not owned by the user (CFG-04, CFG-05).
//! - Every refusal carries **no path** and quotes **no value** from the file, so an error
//!   cannot become a channel for file contents. A key or section name *is* interpolated, so
//!   those two names are escaped with [`crate::render::escape_inline`] on the way in: a
//!   configuration file that names itself `max_\u{1b}[31mRED\u{1b}[0mults` produces a message
//!   with seven visible characters, never a terminal sequence. The *value* of a setting is
//!   still never echoed.
//! - An unknown key is refused rather than ignored (CFG-04): a typo in a safety limit
//!   must not silently leave the default in place.
//! - A key repeated inside one section is refused for the same reason, and with the same
//!   force: real TOML rejects a duplicate key, and the last one silently winning is exactly
//!   the "a typo leaves the wrong limit in place" failure this module exists to prevent. A
//!   file that reads as `allow_write = false` to a reader, to `grep` and in scrollback must
//!   not turn writing **on** because the same key was written twice. The refusal names the
//!   section, the key and **both** line numbers, because with two occurrences the first one
//!   is exactly what a person needs to find.

use crate::error::{ErrorCode, ToolError};
use crate::limits::Limits;
use std::path::PathBuf;

/// Proof that the **user configuration file** opted into writing
/// (`[policy] allow_write = true`).
///
/// This is the only witness [`opencrayast_edit::WriteCap`] may be minted from.
/// Fields are private and there is **no** public constructor, **no** [`Default`],
/// and no way for a dependent to build one except by going through
/// [`Settings::parse`] / [`Settings::load`]. Forging it means re-implementing the
/// configuration reader — not flipping a `bool` or a public `Mode`.
///
/// ```compile_fail
/// // No Default — a dependent must not mint a blank permission.
/// let _ = opencrayast_core::config::WritePermission::default();
/// ```
///
/// ```compile_fail
/// // Private field: struct literal is unreachable outside this module.
/// let _ = opencrayast_core::config::WritePermission { _private: () };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritePermission {
    _private: (),
}

/// The parsed configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Values from `[limits]`.
    pub limits: Limits,
    /// Values from `[policy]` (reporting / `ast_info` / doctor).
    ///
    /// `policy.allow_write` alone is **not** a write capability. Minting requires
    /// [`Self::write_permission`].
    pub policy: Policy,
    /// Set only when the file (or parsed text) had `allow_write = true`.
    write_permission: Option<WritePermission>,
}

impl Default for Settings {
    /// Built-in defaults: writing is off (`write_permission` is `None`).
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            policy: Policy::default(),
            write_permission: None,
        }
    }
}

/// The `[policy]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// `policy.allow_write`. `false` by default: write mode needs this AND `--allow-write`.
    #[allow(clippy::struct_field_names)] // the field is named after the key it is read from
    pub allow_write: bool,
}

impl Settings {
    /// The write-opt-in token from the parsed file, if any.
    ///
    /// `None` when writing was not enabled in configuration. Shells that also require
    /// `--allow-write` must check that flag separately (CONFIGURATION.md / CFG-06).
    pub fn write_permission(&self) -> Option<&WritePermission> {
        self.write_permission.as_ref()
    }

    /// The single place that decides which limits a `Boundary` is held to.
    ///
    /// Every shell goes through here. A caller that builds a `BoundaryConfig` by hand
    /// instead is not wrong, but it is then responsible for the limits it chose, which
    /// is the whole thing this function exists to make visible.
    ///
    /// It also sets `state_dir`, which is what makes the BND-15 guard reachable at all.
    /// `BoundaryConfig::new` leaves it `None` (a caller that has not decided where state
    /// lives has nothing to protect), and every production binary used to go through here
    /// — so in every production binary the write policy's "never a write target inside the
    /// state directory" check compared against `None` and never fired. `Boundary::is_write_target`
    /// gates on it at `crates/core/src/boundary.rs`; a guard nothing sets is not a guard.
    ///
    /// The value comes from [`crate::statedir::user_state_dir`], the one resolver both shells
    /// use, so the directory the boundary protects is by construction the directory the stores
    /// write to. **A resolution failure is not a fallback**: there is no "carry on without the
    /// state directory" branch, because that branch is exactly the unguarded state this
    /// function is fixed to prevent. It is returned as `io_error`, which is what the shells
    /// already report as an environment problem and exit 2 on.
    pub fn boundary_config(
        &self,
        root: impl Into<std::path::PathBuf>,
    ) -> Result<crate::boundary::BoundaryConfig, ToolError> {
        self.boundary_config_with_read_roots(root, &[])
    }

    /// [`Self::boundary_config`] plus the operator's extra READ-ONLY roots (`--read-root`).
    ///
    /// The roots are **not** read from the configuration file: they arrive on the command line,
    /// which is the surface a client (`--workspace`, `--read-root` together in one launch line)
    /// controls. That is deliberate, and it is the same decision `--workspace` already makes — a
    /// repository cannot widen its own read reach by dropping a file in itself.
    ///
    /// The empty case is [`Self::boundary_config`], not a separate code path: a shell with no
    /// `--read-root` must get the identical boundary, so there is one implementation and the
    /// no-roots call is a call with an empty slice.
    pub fn boundary_config_with_read_roots(
        &self,
        root: impl Into<std::path::PathBuf>,
        read_roots: &[std::path::PathBuf],
    ) -> Result<crate::boundary::BoundaryConfig, ToolError> {
        let mut cfg = crate::boundary::BoundaryConfig::new(root, self.limits.clone());
        cfg.read_roots = read_roots.to_vec();
        cfg.state_dir = Some(crate::statedir::user_state_dir()?);
        Ok(cfg)
    }

    /// Parse configuration text.
    ///
    /// Unknown sections and unknown keys are refused. Values are checked by
    /// [`Limits::validate`], so a zero fails here rather than at the point of use.
    ///
    /// An **above-maximum resource ceiling** (bytes, output, results, plan/journal sizes,
    /// time budgets) also fails here, naming the field: those ceilings are not switchable
    /// off, and a configuration that tries to exceed one does not load. **`path_max_depth`
    /// is the exception** — it is operator-tunable, so an above-maximum request parses and
    /// is clamped at enforcement time; the effective depth ceiling is
    /// [`Limits::clamped_path_max_depth`](crate::limits::Limits::clamped_path_max_depth).
    ///
    /// When `policy.allow_write = true`, a [`WritePermission`] is minted into this
    /// [`Settings`]; otherwise [`Self::write_permission`] is `None`.
    pub fn parse(src: &str) -> Result<Self, ToolError> {
        let mut limits = Limits::default();
        let mut policy = Policy::default();
        let mut section: Option<&str> = None;
        // Every (section, key) this file has already set, with the line it was set on.
        //
        // Keyed on BOTH, so a name that legitimately appears in two different sections
        // (`allow_write` in `[policy]` and some future `[limits]` key) is not a duplicate,
        // and a `[limits]` key repeated after a `[policy]` section is. The value is the
        // FIRST line number, which is what the refusal names alongside the second: the pair
        // is the whole diagnosis, and either half alone leaves the reader guessing.
        let mut seen: Vec<(&str, &str, usize)> = Vec::new();

        for (n, raw) in src.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let lineno = n + 1;

            if let Some(rest) = line.strip_prefix('[') {
                let name = rest
                    .strip_suffix(']')
                    .ok_or_else(|| invalid("A table header is not closed with `]`."))?;
                if name.is_empty() || name.contains('[') || name.contains(']') {
                    return Err(invalid("A table header must be a plain name in brackets."));
                }
                if name != "limits" && name != "policy" {
                    return Err(ToolError::new(
                        ErrorCode::InvalidArgs,
                        format!("Unknown section `{}` on line {lineno}.", quoted(name)),
                        "Known sections are `limits` and `policy`.",
                    ));
                }
                section = Some(name);
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                return Err(ToolError::new(
                    ErrorCode::InvalidArgs,
                    format!("Line {lineno} is not `key = value`."),
                    "Every entry is a key, an `=`, and an integer value.",
                ));
            };
            let key = key.trim();
            let value = value.trim();

            // A duplicate is refused BEFORE the key is recognised and before the value is
            // parsed, so the second occurrence cannot silently overwrite the first and the
            // refusal does not depend on which value happens to be valid. A key before any
            // section header has no section to be a duplicate *in*, so it falls through to
            // its own existing refusal below.
            if let Some(s) = section {
                if let Some((_, _, first)) = seen.iter().find(|(sec, k, _)| *sec == s && *k == key)
                {
                    return Err(duplicate_key(s, key, *first, lineno));
                }
                seen.push((s, key, lineno));
            }

            match section {
                Some("limits") => set_limit(&mut limits, key, value, lineno)?,
                Some("policy") => match key {
                    "allow_write" => {
                        policy.allow_write = parse_bool(value, lineno)?;
                    }
                    _ => return Err(unknown_key("policy", key, lineno)),
                },
                Some(other) => return Err(unknown_key(other, key, lineno)),
                None => {
                    return Err(ToolError::new(
                        ErrorCode::InvalidArgs,
                        format!("Line {lineno} has a key before any section header."),
                        "Put the entry under a `[section]`, or add the header.",
                    ));
                }
            }
        }

        // Reject a zero, and refuse an above-hard-maximum RESOURCE ceiling in the same place,
        // naming the field. `path_max_depth` — the one tunable knob — is deliberately not
        // refused here; it is clamped at its enforcement points via
        // `Limits::clamped_path_max_depth`, so the operator's request parses and the guard
        // stays bound.
        limits.validate()?;
        let write_permission = if policy.allow_write {
            Some(WritePermission { _private: () })
        } else {
            None
        };
        Ok(Self {
            limits,
            policy,
            write_permission,
        })
    }

    /// Read and parse a configuration file.
    ///
    /// Refuses a file that is not a regular file, that another user owns, or that grants
    /// any access beyond its owner: a settings file an attacker can edit is a way to turn
    /// write mode on (T-18, CFG-05). No error quotes the path or the file's contents.
    pub fn load(path: &std::path::Path) -> Result<Self, ToolError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let md = std::fs::symlink_metadata(path)
                .map_err(|_| untrusted("The configuration file cannot be read."))?;
            if md.file_type().is_symlink() || !md.is_file() {
                return Err(untrusted(
                    "The configuration file is not a regular file (it is a link or a device).",
                ));
            }
            if md.uid() != rustix::process::geteuid().as_raw() {
                return Err(untrusted(
                    "The configuration file is owned by another user.",
                ));
            }
            // Owner-execute is refused as well as group/other access. It grants nothing
            // (the file is not run), so this is not a security hole — but a mode of 0700
            // is almost always a `chmod -R` that caught more than was meant, and accepting
            // it silently meant the file nobody intended to make executable was accepted.
            // The mask now describes exactly what is accepted: owner read/write, nothing
            // else. 0600 and 0400 are what an operator is told to use; 0700 is not.
            if md.mode() & 0o177 != 0 {
                return Err(untrusted(
                    "The configuration file is executable or grants access to the group or to \
                     other users.",
                ));
            }
        }
        // Windows counterpart of the checks above: refuse a link or a non-file, and then read. The
        // ownership and mode checks have no stable equivalent — see the note on `DirIdentity` in
        // `crates/edit/src/fsutil.rs` — so a Windows config file is trusted on the strength of
        // where it lives (`%LOCALAPPDATA%\opencrayast\config.toml`, in the user's own profile)
        // rather than on what its metadata says. This is weaker than unix and is stated, not
        // implied, because the whole purpose of these checks is to decide whether a file may
        // turn write mode on.
        #[cfg(windows)]
        {
            let md = std::fs::symlink_metadata(path)
                .map_err(|_| untrusted("The configuration file cannot be read."))?;
            if md.file_type().is_symlink() || !md.is_file() {
                return Err(untrusted(
                    "The configuration file is not a regular file (it is a link or a device).",
                ));
            }
            let text = std::fs::read_to_string(path)
                .map_err(|_| untrusted("The configuration file cannot be read."))?;
            // Expression form: clippy::needless_return fires on Windows (this arm is
            // compiled only there; Linux/macOS CI never saw it).
            Self::parse(&text)
        }
        #[cfg(unix)]
        let text = std::fs::read_to_string(path)
            .map_err(|_| untrusted("The configuration file cannot be read."))?;
        #[cfg(unix)]
        Self::parse(&text)
    }
}

/// Set one `[limits]` field by name. The name set is exactly the `Limits` fields, so a
/// rename in `limits.rs` has to be reflected here rather than silently ignored.
fn set_limit(limits: &mut Limits, key: &str, value: &str, lineno: usize) -> Result<(), ToolError> {
    macro_rules! num {
        ($field:ident) => {{
            let v = parse_u64(value, lineno)?;
            limits.$field = v;
        }};
    }
    match key {
        "max_file_bytes" => num!(max_file_bytes),
        "max_output_bytes" => num!(max_output_bytes),
        "max_results" => num!(max_results),
        "max_scan_files" => num!(max_scan_files),
        "parse_timeout_ms" => num!(parse_timeout_ms),
        "parse_max_depth" => num!(parse_max_depth),
        "parse_max_nodes" => num!(parse_max_nodes),
        "call_timeout_ms" => num!(call_timeout_ms),
        "plan_ttl_minutes" => num!(plan_ttl_minutes),
        "plan_max_files" => num!(plan_max_files),
        "plan_max_edits" => num!(plan_max_edits),
        "plan_max_changed_bytes" => num!(plan_max_changed_bytes),
        "plan_max_store_mib" => num!(plan_max_store_mib),
        "plan_max_plans" => num!(plan_max_plans),
        "plan_max_plans_per_process" => num!(plan_max_plans_per_process),
        "journal_max_plan_mib" => num!(journal_max_plan_mib),
        "journal_retention_days" => num!(journal_retention_days),
        "journal_max_total_mib" => num!(journal_max_total_mib),
        "note_max_bytes" => num!(note_max_bytes),
        "path_max_bytes" => num!(path_max_bytes),
        "path_max_depth" => num!(path_max_depth),
        _ => return Err(unknown_key("limits", key, lineno)),
    }
    Ok(())
}

/// Everything after an unquoted `#` is a comment.
fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse a `u64`. Underscores are allowed so `4_194_304` reads as bytes. A value that is
/// not a plain non-negative integer is refused rather than coerced, and the message never
/// echoes the offending text.
fn parse_u64(value: &str, lineno: usize) -> Result<u64, ToolError> {
    let cleaned: String = value.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() || !cleaned.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("Line {lineno} is not a non-negative whole number."),
            "Limits are whole numbers of units (bytes, counts, milliseconds).",
        ));
    }
    cleaned.parse::<u64>().map_err(|_| {
        ToolError::new(
            ErrorCode::InvalidArgs,
            format!("Line {lineno} is larger than this build can represent."),
            "Use a smaller value.",
        )
    })
}

fn parse_bool(value: &str, lineno: usize) -> Result<bool, ToolError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ToolError::new(
            ErrorCode::InvalidArgs,
            format!("Line {lineno} is not `true` or `false`."),
            "Write the value as `true` or `false`.",
        )),
    }
}

/// Refusal for a key that appears twice in the same section.
///
/// The names are escaped here, at the one place file text enters a message, for the reason
/// given in [`Settings::parse`]: a message is text a terminal will render, so anything
/// interpolated into it has to be inert first.
fn duplicate_key(section: &str, key: &str, first: usize, second: usize) -> ToolError {
    let (section, key) = (quoted(section), quoted(key));
    ToolError::new(
        ErrorCode::InvalidArgs,
        format!(
            "Duplicate key `{key}` in section `{section}` on line {second}; it was already set \
             on line {first}."
        ),
        "A key may appear once per section. Remove one of the two, or rename one, so the setting \
         the file means is the one that is used.",
    )
}

/// A section or key name, made inert for display.
///
/// The only place in this module where file text is interpolated into a message, so it is
/// the only place that has to neutralise it. `escape_inline` turns ESC into the seven visible
/// characters `\u{1b}`, and every control, bidi and invisible character into a `\u{..}`
/// escape, so nothing here can become a terminal sequence.
fn quoted(name: &str) -> String {
    crate::render::escape_inline(name).0
}

fn unknown_key(section: &str, key: &str, lineno: usize) -> ToolError {
    let (section, key) = (quoted(section), quoted(key));
    ToolError::new(
        ErrorCode::InvalidArgs,
        format!("Unknown key `{key}` in section `{section}` on line {lineno}."),
        "An unknown key is refused rather than ignored, so a typo cannot leave a safety \
         limit at its default.",
    )
}

fn invalid(what: &str) -> ToolError {
    ToolError::new(
        ErrorCode::InvalidArgs,
        what.to_string(),
        "Check the configuration file's syntax.",
    )
}

/// The refusal for a configuration file whose permissions or ownership make it unsafe to read.
///
/// This is an ENVIRONMENT refusal, not a syntax one, and it carries
/// [`ErrorCode::ConfigUntrusted`] rather than [`ErrorCode::InvalidArgs`] so a shell can map it
/// to its own exit code: the file is fine, the machine it sits on is not configured the way
/// this program requires, and the operator can fix it with `chmod` without changing a word of
/// the configuration. A malformed file is the opposite — nothing about it is transient, so it
/// is a user error. Both used to be `invalid_args`, which is why the CLI could not tell them
/// apart and the two shells disagreed on every one of them.
///
/// Used by both platforms' file-trust checks: on unix for ownership and mode bits, on Windows
/// for the link/regular-file test, which is the part Windows can make.
fn untrusted(what: &str) -> ToolError {
    ToolError::new(
        ErrorCode::ConfigUntrusted,
        what.to_string(),
        "Keep the configuration file readable and writable by you alone (mode 0600) and owned \
         by you; see docs/CONFIGURATION.md.",
    )
}

/// Which file the effective configuration came from, and whether one was read.
///
/// This exists so a shell can tell the user *which* file is in force instead of letting them
/// find out from behaviour. `--config <path>` is honoured wherever it points — including inside
/// the workspace, which the operator may do deliberately — so the honest answer has to name the
/// path and say which of the two routes chose it, not merely "a configuration was loaded".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLocation {
    /// The documented user-level location, and no file was there: the defaults are in force.
    Defaults,
    /// A file was read from the documented user-level location.
    UserFile(PathBuf),
    /// A file was read from an explicit `--config <path>`.
    ExplicitFile(PathBuf),
}

impl ConfigLocation {
    /// The path that was read, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Defaults => None,
            Self::UserFile(p) | Self::ExplicitFile(p) => Some(p),
        }
    }

    /// Whether the file in force was named on the command line rather than found at the
    /// documented location.
    pub fn is_explicit(&self) -> bool {
        matches!(self, Self::ExplicitFile(_))
    }
}

/// Where the configuration in force came from, for this set of arguments.
///
/// The three answers, decided once, here, so `load_or_default` and a shell's diagnostic cannot
/// tell different stories: `doctor` and `ast_info` both render what this returns rather than
/// re-deriving it. A path that does not exist is [`ConfigLocation::Defaults`] — the same
/// "missing is not an error" rule [`load_or_default`] applies.
pub fn config_location(override_path: Option<&str>) -> ConfigLocation {
    let Some(path) = user_config_path(override_path) else {
        return ConfigLocation::Defaults;
    };
    if !path.exists() {
        return ConfigLocation::Defaults;
    }
    if override_path.is_some() {
        ConfigLocation::ExplicitFile(path)
    } else {
        ConfigLocation::UserFile(path)
    }
}

/// Where the user-level configuration file lives, and whether one was found.
///
/// The location is the one `docs/CONFIGURATION.md` documents:
/// `$XDG_CONFIG_HOME/opencrayast/config.toml` (defaulting to `~/.config/…`) on unix, and
/// `%APPDATA%\opencrayast\config.toml` on Windows. `--config <path>` overrides it, which is
/// what makes the behaviour testable with a real binary and a real file rather than a
/// hand-built route.
///
/// **A missing file is not an error.** A user who has never written a configuration file has
/// not misconfigured anything, and refusing to start would make the default path unusable.
/// A file that EXISTS and is wrong IS an error, and `Settings::load` is what reports it —
/// see [`Settings::load`] for why that distinction is the whole security property.
pub fn user_config_path(override_path: Option<&str>) -> Option<PathBuf> {
    if let Some(p) = override_path {
        return Some(PathBuf::from(p));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("opencrayast").join("config.toml"));
    }
    #[cfg(windows)]
    {
        if let Some(dir) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(dir).join("opencrayast").join("config.toml"));
        }
        // The `not(windows)` block below is absent here, so this block ends the function on
        // Windows and `None` is its value. The inner `return` above is an early return and stays.
        None
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME").filter(|v| !v.is_empty())?;
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("opencrayast")
                .join("config.toml"),
        )
    }
}

/// Load the user file, or fall back to the defaults when there is no file.
///
/// This is the function a shell calls. It is deliberately the ONLY place that decides
/// between "no file, use defaults" and "a file exists and it is not acceptable":
///
/// - no file at all → `Ok(Settings::default())`, exit 0;
/// - a file that exists but cannot be trusted or cannot be parsed → `Err`, and the shell
///   must exit non-zero. Silently falling back to defaults there would be the exact bug
///   this ticket exists to remove: an operator who wrote `path_max_depth = 4` and got a
///   world-writable or truncated file would silently be running on the default 64, with
///   nothing in the output to say so.
pub fn load_or_default(override_path: Option<&str>) -> Result<Settings, ToolError> {
    let Some(path) = user_config_path(override_path) else {
        return Ok(Settings::default());
    };
    if !path.exists() {
        return Ok(Settings::default());
    }
    Settings::load(&path)
}
