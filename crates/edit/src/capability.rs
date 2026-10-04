//! Type-level write capability (ADR-006, SECURITY-MODEL S-2 / T-17; WCAP-1).
//!
//! A [`WriteCap`] is proof that the operator enabled write mode. Apply, undo and
//! recover all require one; a read-only [`crate::ApplyContext`] cannot be upgraded
//! by flipping a public `bool`.
//!
//! # Legal mint path
//!
//! The only public constructor is [`WriteCap::mint`], and it requires a
//! [`opencrayast_core::config::WritePermission`] — a token that **only** the
//! configuration module can build, when `[policy] allow_write = true` survives
//! [`Settings::parse`](opencrayast_core::config::Settings::parse) /
//! [`load`](opencrayast_core::config::Settings::load). A `bool`, a public
//! [`Mode`](opencrayast_tools::Mode), or a flag is **not** a witness.
//!
//! Shells still need their own second gate (`--allow-write`); configuration opt-in
//! alone does not turn writing on (CONFIGURATION.md / CFG-06).

use opencrayast_core::config::WritePermission;

/// Proof that write mode was granted from a parsed operator configuration.
///
/// Public minting requires a [`WritePermission`]. Zero-arg mint and
/// `policy::enable_writes()` stay crate-internal for in-crate specs.
///
/// ```compile_fail
/// // No zero-arg public mint — need a WritePermission from Settings.
/// let _ = opencrayast_edit::WriteCap::mint();
/// ```
///
/// ```compile_fail
/// // Dependents cannot invent WritePermission (private field, no Default).
/// let _ = opencrayast_core::config::WritePermission { _private: () };
/// ```
///
/// ```compile_fail
/// // The former public forge path stays closed.
/// let _ = opencrayast_edit::policy::enable_writes();
/// ```
///
/// ```
/// // Legal path: parsed settings → permission → capability.
/// let settings = opencrayast_core::config::Settings::parse(
///     "[policy]\nallow_write = true\n",
/// )
/// .unwrap();
/// let perm = settings.write_permission().expect("allow_write minted a token");
/// let _cap = opencrayast_edit::WriteCap::mint(perm);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteCap {
    _private: (),
}

impl WriteCap {
    /// Mint a capability from a configuration-issued [`WritePermission`].
    ///
    /// This is the **only** public construction path. Shells obtain the permission
    /// from [`Settings::write_permission`](opencrayast_core::config::Settings::write_permission)
    /// after loading the user file; they must still apply `--allow-write` themselves.
    pub fn mint(_permission: &WritePermission) -> Self {
        Self { _private: () }
    }

    /// Shell helper: configuration opt-in **and** the process `--allow-write` flag.
    ///
    /// Returns `None` unless both gates pass (CONFIGURATION.md / CFG-06). Neither a
    /// bare `bool` nor [`Mode`](opencrayast_tools::Mode) is accepted here.
    pub fn from_operator(
        settings: &opencrayast_core::config::Settings,
        allow_write_flag: bool,
    ) -> Option<Self> {
        if !allow_write_flag {
            return None;
        }
        settings.write_permission().map(Self::mint)
    }

    /// Crate-internal mint for unit specs that do not go through a settings file.
    pub(crate) fn mint_unconditional() -> Self {
        Self { _private: () }
    }
}

/// Policy-layer entry points that may mint a [`WriteCap`] inside this crate.
///
/// External code must use [`WriteCap::mint`] with a [`WritePermission`]. This module
/// stays `pub` so the path is named in docs, but [`enable_writes`] is `pub(crate)`.
#[allow(dead_code)] // In-crate unit specs (`src/spec/*`).
pub mod policy {
    use super::WriteCap;

    /// Grant write capability for in-crate tests only (`pub(crate)`).
    ///
    /// Production shells must call [`WriteCap::mint`] with a
    /// [`WritePermission`](opencrayast_core::config::WritePermission).
    pub(crate) fn enable_writes() -> WriteCap {
        WriteCap::mint_unconditional()
    }
}
