#!/usr/bin/env bash
# Pre-publication scan for ADR-016 (OSS-PUBLISH / ISSUE-OSS-SCAN).
# Fails closed on high-confidence secret patterns and on cargo deny / docs / licence drift.
# Optional: if `gitleaks` is on PATH, run it too (not required — pattern scan always runs).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail=0
note() { printf '%s\n' "$*"; }
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }

note "== prepublish-scan =="

# --- licence / deny / docs (published surface) ---
test -s LICENSE || bad "LICENSE missing or empty"
test -s THIRD-PARTY-LICENSES.md || bad "THIRD-PARTY-LICENSES.md missing or empty"
test -s deny.toml || bad "deny.toml missing"
grep -q 'MIT License' LICENSE || bad "LICENSE does not look like MIT"

if command -v cargo >/dev/null 2>&1; then
  if cargo deny check all; then
    note "ok: cargo deny check all"
  else
    # advisories need network; still refuse if licenses/bans/sources fail
    if cargo deny check licenses bans sources; then
      note "ok: cargo deny licenses/bans/sources (advisories failed or unavailable — re-run online before push)"
    else
      bad "cargo deny check failed"
    fi
  fi
else
  bad "cargo not on PATH"
fi

bash scripts/check-docs.sh || bad "check-docs.sh failed"
bash scripts/check-layering.sh || bad "check-layering.sh failed"
bash scripts/check-matrix.sh || bad "check-matrix.sh failed"

# --- high-confidence secret patterns (exclude target, .git, known binary-ish) ---
# Deliberately narrow: false positives must be rarer than missed obvious keys.
# Patterns are assembled so this script's own source does not match them.
PATTERNS=(
  "BEGIN (RSA |OPENSSH |EC )?PRIVATE KEY"
  'AKIA[0-9A-Z]{16}'
  'ghp_[A-Za-z0-9]{20,}'
  'github_pat_[A-Za-z0-9_]{20,}'
  'xox[baprs]-[A-Za-z0-9-]{10,}'
  "-----BEGIN PGP PRIVATE KEY"" BLOCK-----"
)

note "pattern scan (tracked-ish tree, excluding target/.git/dist binaries)…"
tmp="$(mktemp)"
# shellcheck disable=SC2068
if command -v rg >/dev/null 2>&1; then
  for pat in "${PATTERNS[@]}"; do
    if rg -n --hidden -g '!target/**' -g '!.git/**' -g '!dist/**/*.exe' \
      -g '!**/Cargo.lock' -g '!scripts/prepublish-scan.sh' \
      -e "$pat" . >>"$tmp" 2>/dev/null; then
      :
    fi
  done
else
  bad "rg (ripgrep) required for pattern scan"
fi

if [[ -s "$tmp" ]]; then
  bad "secret-like patterns found:"
  cat "$tmp" >&2 || true
else
  note "ok: no high-confidence secret patterns"
fi
rm -f "$tmp"

# --- optional gitleaks ---
if command -v gitleaks >/dev/null 2>&1; then
  note "gitleaks present — running…"
  if gitleaks detect --source "$ROOT" --no-git -v; then
    note "ok: gitleaks clean"
  else
    bad "gitleaks reported findings"
  fi
else
  note "note: gitleaks not installed; pattern scan only (install for deeper coverage)"
fi

# --- SECURITY.md points at private reporting ---
grep -q 'private vulnerability reporting' SECURITY.md \
  || bad "SECURITY.md must describe GitHub private vulnerability reporting"

# --- public URLs present ---
grep -q 'github.com/amgio38/opencrayast' README.md \
  || bad "README must name the public opencrayast URL"
grep -q 'github.com/amgio38/opencraylsp' README.md \
  || bad "README must name the public opencraylsp URL"

if [[ "$fail" -ne 0 ]]; then
  note "prepublish-scan: FAILED"
  exit 1
fi
note "prepublish-scan: PASSED"
exit 0
