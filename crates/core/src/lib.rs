//! L0 core: boundary, limits, errors, hashing, text, atomic filesystem, locks.
//!
//! Normative spec: `docs/ARCHITECTURE.md`, `docs/SECURITY-MODEL.md`, `docs/EDIT-MODEL.md`.
//! This crate depends on no other workspace crate and never parses source code.

pub mod boundary;
pub mod config;
pub mod error;
pub mod fsio;
pub mod hash;
pub mod limits;
pub mod protected;
pub mod render;
pub mod statedir;
pub mod text;
pub mod walk;
pub mod workspace;

pub use error::{ALL_ERROR_CODES, ERROR_CODE_COUNT, ErrorCode, ToolError};

// The pre-rename seam is crate-private, so the test that proves the production path uses it must be
// an in-crate unit test (CR F2). No dependent can inject a callback into the write path.
#[cfg(all(test, unix))]
#[path = "spec/fsio_seam_spec.rs"]
mod fsio_seam_spec;

// SEC-FIX 2: the write path's parent-directory proof is crate-private too, and the only honest
// way to test it is to reach the write from inside the crate. See the module docs for why these
// live in `src/` and not in `tests/`.
#[cfg(all(test, unix))]
#[path = "spec/secfix2_handle_relative_spec.rs"]
mod secfix2_handle_relative_spec;
