//! Built-in protected write targets (docs/CONFIGURATION.md "Built-in protected paths").

use std::path::Path;

/// Directory names that mark VCS metadata at any depth.
const VCS_DIRS: [&str; 4] = [".git", ".hg", ".svn", ".bzr"];

/// True if a WORKSPACE-RELATIVE, already-canonicalised path is a protected write target:
/// VCS metadata (`.git/**`, `.hg/**`, `.svn/**`, `.bzr/**` at any depth), secret-like names
/// (`.env`, `.env.*`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.kdbx`, `id_rsa*`, `id_ed25519*`,
/// `.netrc`, `.npmrc`, `credentials*`, `*.keystore`), plus `extra` globs
/// (`*`, `**`, `?` supported). Matching is ASCII/Unicode case-folded. Built-ins cannot be removed.
///
/// The caller is responsible for having canonicalised `rel` first (no `..`, no absolute
/// prefix, `/` separators); this function only does the case-folded name comparison, so it
/// can never be talked into protecting nothing by a raw `.git/../.git/config` (SECURITY-MODEL
/// T-05: the boundary resolves the path, this layer only refuses writes).
pub fn is_protected(rel: &Path, extra: &[String]) -> bool {
    let folded = rel.to_string_lossy().to_lowercase();
    // An empty path is not a write target, so it is not protected either.
    if folded.is_empty() {
        return false;
    }
    let components: Vec<&str> = folded.split('/').filter(|c| !c.is_empty()).collect();
    let Some(last) = components.last() else {
        return false;
    };

    // Built-ins first: they apply no matter what `extra` says, so a broken extra pattern
    // can never widen access.
    if components.iter().any(|c| VCS_DIRS.contains(c)) {
        return true;
    }
    if matches_secret_name(last) {
        return true;
    }
    matches_extra(&components, extra)
}

/// The secret-name table, applied to the final component.
///
/// Each entry is either an exact name or a `*` pattern; patterns are matched by the same
/// matcher the extras use, so the built-in list and the configured list cannot drift apart
/// in their meaning.
const SECRET_PATTERNS: [&str; 14] = [
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.kdbx",
    "id_rsa*",
    "id_ed25519*",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "credentials*",
    "*.keystore",
];

/// True if the final path component is a built-in secret-like name.
fn matches_secret_name(last: &str) -> bool {
    SECRET_PATTERNS
        .iter()
        .any(|p| glob_match(p.as_bytes(), last.as_bytes()))
}

/// Apply the configured `[protect] extra` globs.
///
/// A pattern without `/` is compared against each single component, the way `.gitignore`
/// behaves, so `*.tfstate` protects a file at any depth. A pattern containing `/` is
/// compared against the whole path from the workspace root, so `secrets/**` protects that
/// subtree and nothing else.
fn matches_extra(components: &[&str], extra: &[String]) -> bool {
    let path = components.join("/");
    for raw in extra {
        let pattern = raw.to_lowercase();
        if !is_usable_pattern(&pattern) {
            // Unusable extra: it matches nothing. The built-in list already applied.
            continue;
        }
        if pattern.contains('/') {
            if glob_path_match(pattern.as_bytes(), path.as_bytes()) {
                return true;
            }
        } else if components
            .iter()
            .any(|c| glob_match(pattern.as_bytes(), c.as_bytes()))
        {
            return true;
        }
    }
    false
}

/// A pattern is usable when it is non-empty, NUL-free and has at least one non-separator
/// character. Anything else cannot describe a target, so it is skipped rather than
/// treated as a match-all.
fn is_usable_pattern(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.contains('\0') && !pattern.trim_matches('/').is_empty()
}

/// Match a full `/`-separated path against a `/`-separated pattern.
///
/// `**` matches zero or more whole components; `*` and `?` stay inside one component.
fn glob_path_match(pattern: &[u8], path: &[u8]) -> bool {
    let pat_segs: Vec<&[u8]> = pattern.split(|b| *b == b'/').collect();
    let path_segs: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    // (i, j) = "pattern segment i against path segment j" is reachable.
    let mut reach = vec![vec![false; path_segs.len() + 1]; pat_segs.len() + 1];
    reach[0][0] = true;
    for i in 0..pat_segs.len() {
        for j in 0..=path_segs.len() {
            if !reach[i][j] {
                continue;
            }
            if pat_segs[i] == b"**" {
                // Zero components, or consume one more and stay on this `**`.
                reach[i + 1][j] = true;
                if j < path_segs.len() {
                    reach[i][j + 1] = true;
                }
            } else if j < path_segs.len() && glob_match(pat_segs[i], path_segs[j]) {
                reach[i + 1][j + 1] = true;
            }
        }
    }
    reach[pat_segs.len()][path_segs.len()]
}

/// Match one component against one pattern segment: `?` is any single character, `*` is any
/// run of characters within the component, everything else is a literal.
///
/// Uses a fill-over-the-grid match rather than backtracking, so a pattern such as `****`
/// or a long run of stars costs `O(n*m)` and never blows up exponentially. No character
/// class is supported: `[` is a literal here, which is why it cannot be a malformed input.
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut dp = vec![false; text.len() + 1];
    dp[0] = true;
    for p in pattern {
        match p {
            b'*' => {
                // A star matches the empty string, then extends over every prefix.
                for j in 1..=text.len() {
                    dp[j] |= dp[j - 1];
                }
            }
            b'?' => {
                // `?` consumes exactly one character, whatever it is.
                for j in (1..=text.len()).rev() {
                    dp[j] = dp[j - 1];
                }
                dp[0] = false;
            }
            _ => {
                for j in (1..=text.len()).rev() {
                    dp[j] = dp[j - 1] && *p == text[j - 1];
                }
                dp[0] = false;
            }
        }
    }
    dp[text.len()]
}
