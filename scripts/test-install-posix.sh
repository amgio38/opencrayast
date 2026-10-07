#!/usr/bin/env bash
# Self-test: the one-line installer in the README works under plain POSIX `sh`.
#
# Why this exists: README says `curl -fsSL .../install.sh | sh`. A pipe never
# reads the script's `#!` line, so the script runs under whatever `sh` is. On
# Debian and Ubuntu that is dash, which has no `set -o pipefail`; the installer
# started with `set -euo pipefail` and died on line 44 for exactly the people the
# one-liner is for, while every CI job stayed green because nothing ever ran the
# script under dash.
#
# Every case runs the REAL install.sh, never a copy of its logic:
#
#   1. the README one-liner's exact shape, over HTTP from a local server:
#        curl -fsSL http://127.0.0.1:PORT/install.sh | sh -s -- --help
#      under `sh`, `dash` and `bash`                      -> must print usage
#   2. the same script run from a file under dash/bash    -> must print usage
#   3. `dash -n` syntax check                             -> must pass
#   4. every raw.githubusercontent.com/.../main/<path> in README.md and
#      install.sh names a file that exists in this repo   -> must all exist
#   5. NEGATIVE CONTROL: a copy of install.sh with the old `set -euo pipefail`
#      restored must FAIL under dash — otherwise the cases above would pass even
#      with the bug present, and this test would prove nothing.
#
# Usage: bash scripts/test-install-posix.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail=0
pass() { printf '  ok: %s\n' "$1"; }
bad() { printf '  FAIL: %s\n' "$1" >&2; fail=1; }

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
	[ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
	rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

has() { command -v "$1" >/dev/null 2>&1; }

# Shells to exercise: `sh` always (that is what the README hands the script to);
# dash and bash when present, so a machine whose `sh` is bash still checks dash.
SHELLS="sh"
has dash && SHELLS="$SHELLS dash"
has bash && SHELLS="$SHELLS bash"

echo "1/5 curl | <shell> -s -- --help (README one-liner shape, over HTTP)"
if has curl && has python3; then
	mkdir "$WORK/www"
	cp install.sh "$WORK/www/install.sh"
	PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
	(cd "$WORK/www" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) &
	SERVER_PID=$!
	for _ in $(seq 1 50); do
		curl -fsS "http://127.0.0.1:$PORT/install.sh" -o /dev/null 2>/dev/null && break
		sleep 0.1
	done
	for sh_ in $SHELLS; do
		out="$(curl -fsSL "http://127.0.0.1:$PORT/install.sh" | "$sh_" -s -- --help 2>&1)" || true
		case "$out" in
			usage:*) pass "curl | $sh_" ;;
			*) bad "curl | $sh_ did not print usage: $(printf '%s' "$out" | head -2)" ;;
		esac
	done
else
	echo "  skip: curl or python3 not available"
fi

echo "2/5 <shell> install.sh --help (from a file)"
for sh_ in $SHELLS; do
	out="$("$sh_" install.sh --help 2>&1)" || true
	case "$out" in
		usage:*) pass "$sh_ install.sh" ;;
		*) bad "$sh_ install.sh did not print usage: $(printf '%s' "$out" | head -2)" ;;
	esac
done

echo "3/5 dash -n"
if has dash; then
	if dash -n install.sh; then pass "dash -n install.sh"; else bad "dash -n install.sh"; fi
else
	echo "  skip: dash not available"
fi

echo "4/5 one-line install paths exist in the repo"
paths="$(grep -hEo 'raw\.githubusercontent\.com/amgio38/opencrayast/main/[A-Za-z0-9_./-]+' README.md install.sh |
	sed 's|^raw\.githubusercontent\.com/amgio38/opencrayast/main/||' | sort -u)"
if [ -z "$paths" ]; then
	bad "found no raw.githubusercontent.com install URL in README.md / install.sh"
fi
for p in $paths; do
	if [ -f "$p" ]; then pass "$p"; else bad "$p is named in README.md or install.sh but is not in the repo"; fi
done

echo "5/5 negative control: the old 'set -euo pipefail' must fail under dash"
if has dash && dash -c 'set -o pipefail' 2>/dev/null; then
	echo "  skip: this dash accepts pipefail, so the bug cannot be reproduced here"
elif has dash; then
	sed 's/^set -eu$/set -euo pipefail/; /^(set -o pipefail) 2>\/dev\/null/d' install.sh >"$WORK/old-install.sh"
	if grep -q '^set -euo pipefail$' "$WORK/old-install.sh"; then
		out="$(dash "$WORK/old-install.sh" --help 2>&1)" || true
		case "$out" in
			usage:*) bad "the old script ran under dash: the test cannot detect the bug" ;;
			*) pass "old script fails under dash ($(printf '%s' "$out" | head -1))" ;;
		esac
	else
		bad "could not rebuild the old header from install.sh: update this test"
	fi
else
	echo "  skip: dash not available"
fi

if [ "$fail" -ne 0 ]; then
	echo "test-install-posix: FAILED" >&2
	exit 1
fi
echo "test-install-posix: ok"
