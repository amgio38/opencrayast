//! L1 lang: the language registry and budgeted parsing (docs/LANGUAGES.md, docs/ARCHITECTURE.md).
//!
//! This crate never touches the filesystem: it is handed bytes. It is the part that will run in
//! the isolated parse worker (ADR-004), so everything public here is plain data in, plain data out.

pub mod language;
pub mod parse;

pub use language::Language;
pub use parse::{ParseBudget, ParsedFile, parse};
