//! Hit normalisation shared by the oracle and (later) our matcher.

/// One capture after normalisation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureSpan {
    pub name: String,
    pub start: usize,
    pub end: usize,
    /// `true` for `$$$NAME` (list); `false` for `$NAME`.
    pub list: bool,
}

/// One match: root span plus sorted captures.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NormalizedHit {
    pub start: usize,
    pub end: usize,
    pub captures: Vec<CaptureSpan>,
}

/// Sort captures by (name, start, end, list) and hits by (start, end, captures).
pub fn finalize_hits(mut hits: Vec<NormalizedHit>) -> Vec<NormalizedHit> {
    for h in &mut hits {
        h.captures.sort();
    }
    hits.sort();
    hits
}

/// Identity helper so call sites read symmetrically with a future `normalize_ours_hits`.
pub fn normalize_ast_grep_hits(hits: Vec<NormalizedHit>) -> Vec<NormalizedHit> {
    finalize_hits(hits)
}

/// Align empty-list capture anchors: both engines report a zero-width span, but
/// ast-grep often pins it at the match start while we pin it at the insertion
/// point among siblings. For PAT-05 we compare both as `(hit.start, hit.start)`.
pub fn canonicalize_hits(mut hits: Vec<NormalizedHit>) -> Vec<NormalizedHit> {
    for h in &mut hits {
        for c in &mut h.captures {
            if c.list && c.start == c.end {
                c.start = h.start;
                c.end = h.start;
            }
        }
        h.captures.sort();
    }
    hits.sort();
    hits
}
