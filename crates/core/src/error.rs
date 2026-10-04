//! Stable error taxonomy. Codes are part of the public contract (docs/TOOLS.md).

use std::fmt;

/// Stable machine-readable error code. `as_str` values never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Missing, mistyped or out-of-range argument.
    InvalidArgs,
    /// Path resolves outside the boundary (also used when existence cannot be shown).
    OutsideWorkspace,
    /// Target is a protected path.
    ProtectedPath,
    /// No such file or symbol.
    NotFound,
    /// File over the size limit.
    FileTooLarge,
    /// File is not valid UTF-8.
    NotUtf8,
    /// A plan or store limit.
    LimitExceeded,
    /// Another apply holds the lock.
    Busy,
    /// Target cannot be replaced without losing properties (hard links, read-only, ...).
    UnsupportedTarget,
    /// The filesystem failed.
    IoError,
    /// The target was replaced, but the parent directory could not be synced, so the
    /// replacement may not survive a crash. The write DID happen.
    ReplacedNotDurable,
    /// No grammar is built in for this file.
    UnsupportedLanguage,
    /// A parse or match budget ran out (size of tree, depth, steps).
    BudgetExceeded,
    /// A wall-clock limit ran out.
    Timeout,
    /// More than one symbol matches the request.
    Ambiguous,
    /// A pattern does not parse.
    InvalidPattern,
    /// A file changed after the plan was previewed.
    StalePlan,
    /// The plan's time to live ran out.
    PlanExpired,
    /// No stored plan has this id.
    PlanNotFound,
    /// A stored plan does not match its id or is malformed.
    PlanCorrupt,
    /// The plan belongs to another workspace.
    WrongWorkspace,
    /// The plan was already applied and its journal still exists.
    AlreadyApplied,
    /// The plan's journal no longer exists, so its originals cannot be restored. The plan itself is
    /// still in the store: this is retention, not an unknown id (TOOLS.md "Errors" table,
    /// `journal_missing` = "Retention expired | Undo no longer possible").
    JournalMissing,
    /// A gate (syntax, size, path, encoding, stability) refused the new content.
    GateFailed,
    /// A file changed after apply, so undo or recovery refuses to overwrite it.
    Diverged,
    /// Writing is not enabled for this server.
    WriteDisabled,
    /// A rollback could not finish; run recovery.
    RollbackIncomplete,
    /// The rewrite would drop comments and the request did not allow it.
    CommentLoss,
    /// An edit set violates E-1: out of range, overlapping or mid-character.
    InvalidEdit,
    /// The user configuration file exists but cannot be trusted: it is owned by another user,
    /// or its permissions let somebody else change it, or it is not a regular file.
    ///
    /// A separate code from [`Self::InvalidArgs`] because the operator's remedy is different
    /// and so is the classification: the file is not wrong, the machine is, and `chmod` or `chown`
    /// fixes it without touching the configuration. It maps to the environment exit code, where
    /// `invalid_args` maps to the user one.
    ConfigUntrusted,
    /// A defect; never expected.
    Internal,
}

/// Which of the two documented exit-code classes an [`ErrorCode`] belongs to.
///
/// Deliberately a **class**, not a number. The two shells print different digits for the two
/// classes and must not print different *decisions*: `opencrayast` uses 1 and 2, `opencrayast-mcp`
/// uses 2 and 3, and only the mapping to digits is local to each binary. The classification itself
/// is [`ErrorCode::exit_class`], in this crate, so it cannot drift between them.
///
/// Each class is also a promise about whether retrying unchanged can help, which is what a wrapper
/// script actually branches on:
///
/// - [`User`](ExitClass::User) — the input was wrong. The same command fails the same way forever.
/// - [`Environment`](ExitClass::Environment) — this machine, this moment. The same command may well
///   work later, or elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitClass {
    /// The request is wrong: bad arguments, an unresolvable id, a configuration file that does not
    /// parse. Retrying unchanged will not help.
    User,
    /// The machine cannot do it now: a permission, a missing state directory, a held lock, a limit,
    /// or a configuration file that cannot be trusted.
    Environment,
}

impl ExitClass {
    /// The name used for this class in documentation and in `--help`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user error",
            Self::Environment => "environment",
        }
    }
}

impl fmt::Display for ExitClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ErrorCode {
    /// Which of the two documented exit-code classes this error belongs to.
    ///
    /// **The single source of truth for the user-error / environment-error split**, because two
    /// shells have to agree on it and they cannot share a crate that both may depend on: `opencrayast`
    /// (the CLI) and `opencrayast-mcp` are separate binaries, and the layering table does not let
    /// either depend on the other. Each shell maps this to its own digits and documents them —
    /// the CLI in `--help`, the MCP server in `docs/CONFIGURATION.md` — but the decision itself is
    /// made once, here.
    ///
    /// The distinction is the same one `docs/TOOLS.md` and `exit.rs` describe: a request that is
    /// wrong (`invalid_args`, an unresolvable id) will fail the same way every time, so it is a
    /// **user** error; something about this machine or this moment (a permission, a lock, a limit,
    /// a configuration file nobody can trust) is an **environment** error.
    pub fn exit_class(self) -> ExitClass {
        match self {
            // The request itself is wrong, or the thing it names is not there.
            Self::InvalidArgs
            | Self::InvalidPattern
            | Self::InvalidEdit
            | Self::PlanNotFound
            | Self::PlanExpired
            | Self::PlanCorrupt
            | Self::WrongWorkspace
            | Self::AlreadyApplied
            | Self::StalePlan
            | Self::GateFailed
            | Self::Diverged
            | Self::CommentLoss
            | Self::JournalMissing
            | Self::RollbackIncomplete
            | Self::NotFound
            | Self::Ambiguous
            | Self::UnsupportedLanguage => ExitClass::User,

            // Something about this machine or this moment.
            Self::IoError
            | Self::OutsideWorkspace
            | Self::ProtectedPath
            | Self::WriteDisabled
            | Self::Busy
            | Self::LimitExceeded
            | Self::UnsupportedTarget
            | Self::ReplacedNotDurable
            | Self::FileTooLarge
            | Self::NotUtf8
            | Self::BudgetExceeded
            | Self::Timeout
            | Self::ConfigUntrusted => ExitClass::Environment,

            // A defect. Reported as an environment problem because the user cannot act on it by
            // changing their command; the message says to report it.
            Self::Internal => ExitClass::Environment,
        }
    }

    /// The stable wire name, e.g. `outside_workspace`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgs => "invalid_args",
            Self::OutsideWorkspace => "outside_workspace",
            Self::ProtectedPath => "protected_path",
            Self::NotFound => "not_found",
            Self::FileTooLarge => "file_too_large",
            Self::NotUtf8 => "not_utf8",
            Self::LimitExceeded => "limit_exceeded",
            Self::Busy => "busy",
            Self::UnsupportedTarget => "unsupported_target",
            Self::IoError => "io_error",
            Self::ReplacedNotDurable => "replaced_not_durable",
            Self::UnsupportedLanguage => "unsupported_language",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Timeout => "timeout",
            Self::Ambiguous => "ambiguous",
            Self::InvalidPattern => "invalid_pattern",
            Self::StalePlan => "stale_plan",
            Self::PlanExpired => "plan_expired",
            Self::PlanNotFound => "plan_not_found",
            Self::PlanCorrupt => "plan_corrupt",
            Self::WrongWorkspace => "wrong_workspace",
            Self::AlreadyApplied => "already_applied",
            Self::JournalMissing => "journal_missing",
            Self::GateFailed => "gate_failed",
            Self::Diverged => "diverged",
            Self::WriteDisabled => "write_disabled",
            Self::RollbackIncomplete => "rollback_incomplete",
            Self::CommentLoss => "comment_loss",
            Self::InvalidEdit => "invalid_edit",
            Self::ConfigUntrusted => "config_untrusted",
            Self::Internal => "internal",
        }
    }
}

/// An error that is safe to show to an agent: a code, what is true, and what to do next.
/// Never contains source text or absolute paths outside the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    /// Stable code.
    pub code: ErrorCode,
    /// What is true.
    pub message: String,
    /// What to do next.
    pub next: String,
}

impl ToolError {
    /// Build an error.
    pub fn new(code: ErrorCode, message: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            next: next.into(),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} Next: {}",
            self.code.as_str(),
            self.message,
            self.next
        )
    }
}

/// Every code this crate can emit, in declaration order.
///
/// Exists so the documentation can be checked against the code rather than against a copy of it.
/// Before this existed, `docs/TOOLS.md` §Error code reference and `ErrorCode` were two hand-kept
/// lists that could drift: adding a variant did not force a doc row, and deleting one left a
/// documented code no call could ever produce. `UX1-06` in `crates/tools/tests/
/// tool_descriptions_spec.rs` now asserts the two sets are EQUAL in both directions, so a new
/// variant without a documented row fails the test suite rather than a code review.
///
/// A `const` slice rather than a function: the contents never change at runtime, and a test can
/// then compare against a literal list without allocating.
pub const ALL_ERROR_CODES: &[ErrorCode] = &[
    ErrorCode::InvalidArgs,
    ErrorCode::OutsideWorkspace,
    ErrorCode::ProtectedPath,
    ErrorCode::NotFound,
    ErrorCode::FileTooLarge,
    ErrorCode::NotUtf8,
    ErrorCode::LimitExceeded,
    ErrorCode::Busy,
    ErrorCode::UnsupportedTarget,
    ErrorCode::IoError,
    ErrorCode::ReplacedNotDurable,
    ErrorCode::UnsupportedLanguage,
    ErrorCode::BudgetExceeded,
    ErrorCode::Timeout,
    ErrorCode::Ambiguous,
    ErrorCode::InvalidPattern,
    ErrorCode::StalePlan,
    ErrorCode::PlanExpired,
    ErrorCode::PlanNotFound,
    ErrorCode::PlanCorrupt,
    ErrorCode::WrongWorkspace,
    ErrorCode::AlreadyApplied,
    ErrorCode::JournalMissing,
    ErrorCode::GateFailed,
    ErrorCode::Diverged,
    ErrorCode::WriteDisabled,
    ErrorCode::RollbackIncomplete,
    ErrorCode::CommentLoss,
    ErrorCode::InvalidEdit,
    ErrorCode::ConfigUntrusted,
    ErrorCode::Internal,
];

/// The count [`ALL_ERROR_CODES`] must always have.
///
/// A new variant that is not added to the slice is invisible to `UX1-06` in the "crate emits it"
/// direction, which would turn the bidirectional check into a one-sided one. This count is compared
/// against a literal in that test, so forgetting the slice fails there too.
pub const ERROR_CODE_COUNT: usize = 31;

const _: () = {
    // Compile-time: the slice and the count cannot disagree. (The duplicate-name check needs
    // `PartialEq` in const, which is not stable yet, so it lives in the test below.)
    assert!(ALL_ERROR_CODES.len() == ERROR_CODE_COUNT);
};

#[cfg(test)]
mod all_error_codes_tests {
    use super::*;

    /// The slice has no duplicates.
    ///
    /// A duplicate would make `UX1-06`'s bidirectional comparison silently pass: the documented
    /// set is built from unique names, so a repeated variant would hide a missing one.
    #[test]
    fn no_duplicate_codes() {
        let mut seen = std::collections::BTreeSet::new();
        for c in ALL_ERROR_CODES {
            assert!(seen.insert(c.as_str()), "duplicate: {}", c.as_str());
        }
        assert_eq!(seen.len(), ALL_ERROR_CODES.len());
    }

    /// The count matches, so a variant added to the enum but not to the slice cannot pass
    /// unnoticed. `UX1-06` compares against a literal count for the same reason.
    #[test]
    fn the_count_is_the_number_of_variants() {
        assert_eq!(ALL_ERROR_CODES.len(), ERROR_CODE_COUNT);
    }
}

impl std::error::Error for ToolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_has_code_message_and_next() {
        let e = ToolError::new(
            ErrorCode::OutsideWorkspace,
            "Path is outside.",
            "Use a path inside.",
        );
        assert_eq!(
            e.to_string(),
            "[outside_workspace] Path is outside. Next: Use a path inside."
        );
    }
}

#[cfg(test)]
mod replaced_not_durable_tests {
    use super::*;

    /// B: the new code has to say two different things at once — the write happened, and it
    /// may not survive a crash — so that a caller cannot read it as "nothing was written".
    #[test]
    fn wire_name_is_stable() {
        assert_eq!(
            ErrorCode::ReplacedNotDurable.as_str(),
            "replaced_not_durable"
        );
    }

    #[test]
    fn display_names_the_code_and_the_consequence() {
        let e = ToolError::new(
            ErrorCode::ReplacedNotDurable,
            "file was replaced but the directory sync failed; the change may not survive a crash",
            "Re-read the file to confirm its contents, then decide whether to retry.",
        );
        let s = e.to_string();
        assert!(s.starts_with("[replaced_not_durable]"), "{s}");
        assert!(s.contains("may not survive a crash"), "{s}");
        assert!(s.contains("Re-read the file"), "{s}");
    }

    /// Adding a variant must not change the wire name of any existing code.
    #[test]
    fn existing_wire_names_are_unchanged() {
        assert_eq!(ErrorCode::InvalidArgs.as_str(), "invalid_args");
        assert_eq!(ErrorCode::IoError.as_str(), "io_error");
        assert_eq!(ErrorCode::Internal.as_str(), "internal");
        assert_eq!(ErrorCode::Busy.as_str(), "busy");
    }
}
