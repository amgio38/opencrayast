#!/usr/bin/env bash
# ADR-016 clean export: copy the working tree into a fresh git history for
# github.com/amgio38/opencrayast. Does NOT push. Excludes private/operator paths.
#
# Usage:
#   bash scripts/clean-export.sh [DEST]
# Default DEST: /tmp/opencrayast-export-<date>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:-/tmp/opencrayast-export-$(date +%Y%m%d)}"
AUTHOR_NAME="${EXPORT_AUTHOR_NAME:-amgio38}"
AUTHOR_EMAIL="${EXPORT_AUTHOR_EMAIL:-amgio38@users.noreply.github.com}"

note() { printf '%s\n' "$*"; }

if [[ -e "$DEST" ]]; then
  printf 'DEST already exists: %s\n' "$DEST" >&2
  printf 'Remove it or pass a new path.\n' >&2
  exit 2
fi

note "== clean-export → $DEST =="
mkdir -p "$DEST"

# rsync if available; else tar pipe. Exclude build artefacts and private notes.
# test.md is a local pre-publication working note, not a published document.
EXCLUDE=(
  --exclude '.git'
  --exclude 'target'
  --exclude 'internal'
  --exclude 'dist'
  --exclude '.cray'
  --exclude '*.swp'
  --exclude '.DS_Store'
  --exclude 'test.md'
)

if command -v rsync >/dev/null 2>&1; then
  rsync -a "${EXCLUDE[@]}" "$ROOT/" "$DEST/"
else
  # portable-ish fallback
  (cd "$ROOT" && tar cf - \
    --exclude='.git' --exclude='target' --exclude='internal' --exclude='dist' \
    --exclude='.cray' --exclude='test.md' .) | (cd "$DEST" && tar xf -)
fi

# Guard: internal/ must not ship
if [[ -e "$DEST/internal" ]]; then
  printf 'export still contains internal/ — refuse\n' >&2
  exit 1
fi

# Guard: the local working note must not ship either. It is easy to forget an entry in
# EXCLUDE and this is the file that gets forgotten, so the outcome is checked, not assumed.
if [[ -e "$DEST/test.md" ]]; then
  printf 'export still contains test.md — refuse\n' >&2
  exit 1
fi

# Fresh history
git -C "$DEST" init -b main
git -C "$DEST" config user.name "$AUTHOR_NAME"
git -C "$DEST" config user.email "$AUTHOR_EMAIL"
# local only — do not inherit the working clone's remotes
git -C "$DEST" add -A
git -C "$DEST" commit -m "$(cat <<EOF
Initial public import of opencrayast.

Clean export from the private working tree (ADR-016). History starts here.
EOF
)"

note "initial commit: $(git -C "$DEST" rev-parse --short HEAD)"
note "author: $AUTHOR_NAME <$AUTHOR_EMAIL>"

# Verify published surface inside the export
note "running checks inside export…"
(
  cd "$DEST"
  bash scripts/check-docs.sh
  bash scripts/check-layering.sh
  # matrix + deny need network/tooling similar to source tree
  bash scripts/check-matrix.sh
  if command -v cargo >/dev/null 2>&1; then
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
    export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
    # A private target/ so the export does not grow a multi-gigabyte target/ of its own:
    # DEST is about to be pushed, and a rebuilt target/ inside it is neither wanted nor
    # tracked (.gitignore covers /target, so git would ignore it — but a hand-run
    # `rsync -a` to the remote, or a `git add -A` by someone in a hurry, would ship it).
    # The scratch dir lives in the user state directory, never in /tmp: the export must be
    # reproducible on a machine whose /tmp is wiped between the export and the push.
    SCRATCH_TARGET="${XDG_STATE_HOME:-$HOME/.local/state}/opencrayast/export-target"
    mkdir -p "$SCRATCH_TARGET"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$SCRATCH_TARGET}"
    cargo test --workspace --locked --no-run
    cargo test -p opencrayast-tools --test tool_descriptions_spec --locked
    cargo test -p opencrayast-mcp --locked --lib
  fi
)

# Prove the export really carries no target/ before declaring it ready.
if [[ -e "$DEST/target" ]]; then
  printf 'export grew a target/ — the verification build did not honour CARGO_TARGET_DIR\n' >&2
  exit 1
fi

note "clean-export: READY at $DEST"
note "next (human): create github.com/amgio38/opencrayast, push this tree, enable private vulnerability reporting, run Actions."
exit 0
