//! Content hashes and plan identifiers (ADR-008, ADR-011; EDIT-MODEL "Plan format").

use std::fmt;

use data_encoding::BASE32_NOPAD;
use sha2::{Digest, Sha256};

/// The only accepted textual prefix of a content hash.
const HASH_PREFIX: &str = "sha256:";
/// Hexadecimal digits of a SHA-256 digest.
const HEX_LEN: usize = 64;
/// Length of the base32 body of a plan id (26 chars = 130 bits >= 128).
const PLAN_BODY_LEN: usize = 26;
/// Shortest plan-id prefix that read-only tools may accept.
const MIN_READONLY_PREFIX_LEN: usize = 10;

/// SHA-256 of some bytes. Displays as `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Hash `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Parse the `sha256:<hex>` form; `None` if malformed (wrong prefix, length, case or digits).
    ///
    /// Only the exact canonical form produced by [`fmt::Display`] is accepted: the
    /// `sha256:` prefix, 64 hexadecimal digits, all lowercase. Anything else is a
    /// normal `None`, never a panic, so that untrusted input can be fed in directly.
    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix(HASH_PREFIX)?;
        if hex.len() != HEX_LEN {
            return None;
        }
        let bytes = hex.as_bytes();
        if !bytes.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
            // Rejects uppercase and any non-hex digit.
            return None;
        }
        let mut out = [0u8; 32];
        for (i, pair) in bytes.chunks_exact(2).enumerate() {
            let hi = hex_val(pair[0])?;
            let lo = hex_val(pair[1])?;
            out[i] = (hi << 4) | lo;
        }
        Some(Self(out))
    }
}

/// Value of one lowercase hexadecimal digit.
fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{HASH_PREFIX}")?;
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Plan id: `p-` + first 26 chars of unpadded lowercase RFC 4648 base32 (`a-z2-7`)
/// of the first 128 bits of SHA-256 of `canonical_plan_bytes`.
///
/// The input must already be the canonical serialisation of the plan (sorted keys,
/// no insignificant whitespace, UTF-8): the id is a function of those bytes and of
/// nothing else, so run-varying data such as the creation time must not be in them
/// (EDIT-MODEL "Plan format").
pub fn plan_id(canonical_plan_bytes: &[u8]) -> String {
    let digest = Sha256::digest(canonical_plan_bytes);
    // 16 bytes = the leading 128 bits of the digest.
    let truncated = &digest[..16];
    let encoded = BASE32_NOPAD.encode(truncated).to_lowercase();
    let mut out = String::with_capacity(2 + PLAN_BODY_LEN);
    out.push_str("p-");
    out.push_str(&encoded[..PLAN_BODY_LEN]);
    out
}

/// True only for a complete, well-formed plan id (`p-` + exactly 26 chars of `a-z2-7`).
/// Used by every write operation (EDIT-MODEL E-15): abbreviations must return false.
pub fn is_full_plan_id(s: &str) -> bool {
    let Some(body) = s.strip_prefix("p-") else {
        return false;
    };
    if body.len() != PLAN_BODY_LEN {
        return false;
    }
    body.bytes().all(|c| matches!(c, b'a'..=b'z' | b'2'..=b'7'))
}

/// Accept an abbreviation (>= 10 chars including the `p-`) of a plan id, for READ-ONLY
/// tools. Returns the unique full id among `known`, or `None` if zero or several match.
///
/// This is a lookup convenience, never an authority: callers that write (apply, undo,
/// recover) must require [`is_full_plan_id`] instead (EDIT-MODEL E-15).
pub fn resolve_plan_prefix<'a>(prefix: &str, known: &'a [String]) -> Option<&'a str> {
    if !prefix.starts_with("p-") || prefix.len() < MIN_READONLY_PREFIX_LEN {
        return None;
    }
    let mut found: Option<&'a str> = None;
    for id in known {
        if !id.starts_with(prefix) {
            continue;
        }
        if found.is_some() {
            // Ambiguous: refuse rather than guess.
            return None;
        }
        found = Some(id.as_str());
    }
    found
}
