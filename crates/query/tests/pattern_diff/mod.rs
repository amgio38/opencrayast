//! Differential-test helpers for PAT-05 (ast-grep-core oracle).
//!
//! Both sides run: our matcher (`ours_side`) and ast-grep-core (`ast_grep_side`).
//! Hits are normalised, then compared against the corpus / known-divergence list.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

mod ast_grep_side;
mod corpus;
mod divergences;
mod normalize;
mod ours_side;
mod random_js;

pub use ast_grep_side::{DiffLang, search_ast_grep};
pub use corpus::{Case, corpus_all, corpus_counts};
pub use divergences::{KNOWN_DIVERGENCES, Kind};
pub use normalize::{
    CaptureSpan, NormalizedHit, canonicalize_hits, finalize_hits, normalize_ast_grep_hits,
};
pub use ours_side::search_ours;
pub use random_js::{RANDOM_JS_PATTERNS, generate_random_js_sources};
