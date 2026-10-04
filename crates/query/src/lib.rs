//! L2 query: outline and symbol lookup (and, in later milestones, patterns and rewrites), all as
//! pure functions of source text (docs/ARCHITECTURE.md; docs/LANGUAGES.md "Outline queries").
//!
//! Nothing here reads files: the caller hands over the source and the parsed tree.

pub mod outline;

pub use outline::{
    OutlineOptions, Symbol, SymbolKind, SymbolText, collect_all_counting_discards, find_symbols,
    outline, symbol_text,
};
pub mod pattern;
