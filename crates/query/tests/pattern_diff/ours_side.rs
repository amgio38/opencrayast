//! Our matcher side for PAT-05 differential tests.

use opencrayast_lang::{ParseBudget, parse};
use opencrayast_query::pattern::{CaptureKind, Pattern, SearchBudget, search};
use std::time::Duration;

use super::DiffLang;
use super::normalize::{CaptureSpan, NormalizedHit, finalize_hits};

/// Run our `Pattern::compile` + `search`, normalised like the oracle.
pub fn search_ours(
    lang: DiffLang,
    pattern: &str,
    source: &str,
) -> Result<Vec<NormalizedHit>, String> {
    let compiled = Pattern::compile(lang.0, pattern).map_err(|e| e.message)?;
    let budget = ParseBudget {
        max_bytes: (source.len() as u64).saturating_add(64).max(4096),
        timeout: Duration::from_secs(2),
        max_depth: 256,
        max_nodes: 100_000,
    };
    let parsed = parse(lang.0, source, &budget).map_err(|e| e.message)?;
    let outcome = search(
        &parsed,
        source,
        &compiled,
        None,
        &SearchBudget {
            max_steps: 5_000_000,
            deadline: None,
            max_matches: 10_000,
        },
    )
    .map_err(|e| e.message)?;
    Ok(normalize_ours_hits(outcome.matches))
}

fn normalize_ours_hits(matches: Vec<opencrayast_query::pattern::Match>) -> Vec<NormalizedHit> {
    let hits = matches
        .into_iter()
        .map(|m| NormalizedHit {
            start: m.start_byte,
            end: m.end_byte,
            captures: m
                .captures
                .into_iter()
                .map(|c| CaptureSpan {
                    name: c.name,
                    start: c.start_byte,
                    end: c.end_byte,
                    list: matches!(c.kind, CaptureKind::List),
                })
                .collect(),
        })
        .collect();
    finalize_hits(hits)
}
