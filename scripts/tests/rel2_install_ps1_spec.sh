#!/usr/bin/env bash
# REL2-xx: the Windows installer, `install.ps1`.
#
# ## What is checked here, and what is NOT
#
# This machine has **no PowerShell**. So these cases split into two kinds, and pretending
# otherwise is the one thing this file must not do:
#
# - **Contract checks (run here, run everywhere).** The installer is parsed for the
#   properties that decide whether it is safe: the order of the gates, the whitelist, the
#   refusal to touch PREFIX before verification, the BOM handling, no elevation. These are
#   checkable without executing PowerShell, because they are properties of the text.
# - **Execution checks (need a Windows or PowerShell runner).** "A good dist installs", "a
#   bad SHA256SUMS refuses", "a truncated SHA256SUMS refuses" — the three cases the ticket
#   names — are written below as a `pwsh`-driven block that **skips loudly** when `pwsh` is
#   absent. A skip prints SKIP and is counted; it is not a pass.
#
# The skip is the honest part. A CI job that goes green because the thing under test was
# missing is the failure mode this project keeps finding, so the skip says so on stdout and
# `make test-rel2` prints whether any case was skipped.
#
# ## The contract, from install.sh
#
# install.sh enforces: (1) both install paths listed in SHA256SUMS, (2) every listed digest
# verifies, (3) both binaries exist, (4) copy — in that order, and nothing before (4) may
# touch PREFIX. `install.ps1` must enforce the same four in the same order, or a Windows user
# is held to a different standard than a Unix one.
#
# Usage: scripts/tests/rel2_install_ps1_spec.sh
set -uo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
ps1="$root/install.ps1"
sh="$root/install.sh"

pass=0
fail=0
skip=0
ok()   { printf 'ok: %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL: %s\n' "$1" >&2; fail=$((fail + 1)); }
skipped() { printf 'SKIP: %s (reason: %s)\n' "$1" "$2"; skip=$((skip + 1)); }

[ -f "$ps1" ] || { echo "FAIL: install.ps1 is missing" >&2; exit 1; }
ps=$(cat "$ps1")

# ---------------------------------------------------------------------------
# REL2-01: the installer exists and is a PowerShell script, not a renamed shell script.
# ---------------------------------------------------------------------------
if head -5 "$ps1" | grep -q 'Requires -Version'; then
	ok "REL2-01 install.ps1 declares a PowerShell version requirement"
else
	bad "REL2-01 install.ps1 must declare #Requires -Version"
fi

if grep -qE '^#!/.*(ba)?sh' "$ps1"; then
	bad "REL2-01 install.ps1 must not be a shell script with a .ps1 name"
else
	ok "REL2-01 install.ps1 is not a shell script"
fi

# ---------------------------------------------------------------------------
# REL2-02: the four gates appear, and in order.
#
# This is the whole contract in one assertion. The order is what makes "nothing is touched
# before verification" true; a script with the right checks in the wrong order installs
# nothing on a bad tree only by luck of filesystem ordering.
#
# The gates are looked for INSIDE Install-FromDist, not in the file as a whole: the installer
# has three paths (release asset, local dist tree, cargo build), and only the dist path is
# governed by the SHA256SUMS contract. A whole-file search finds the release path's Copy-Item
# first and then reports a spurious ordering failure — which is what happened when the
# release path was added.
# ---------------------------------------------------------------------------
python3 - "$ps1" <<'PY' && ok "REL2-02 the four gates appear in install order" || bad "REL2-02 the four gates are missing or out of order"
import re, sys

s = open(sys.argv[1], encoding="utf-8").read()

# Isolate the dist path. Everything this case asserts is about the SHA256SUMS contract, and
# that contract lives in exactly one function.
m = re.search(r'function\s+Install-FromDist\b(.*?)\n\}', s, re.S)
if not m:
    print("  missing: the Install-FromDist function", file=sys.stderr)
    sys.exit(1)
dist = m.group(1)

def pos(pattern, what):
    m = re.search(pattern, dist, re.S)
    if not m:
        print(f"  missing: {what}", file=sys.stderr)
        sys.exit(1)
    return m.start()

coverage = pos(r'not listed in SHA256SUMS', "coverage check (both paths listed)")
digest   = pos(r'checksum mismatch',       "digest verification")
exists   = pos(r'missing binary',           "binary existence")
copy     = pos(r'Copy-Item',               "the copy")

# Coverage must be a loop over the required set, not a single hardcoded path: install.sh
# loops, and a script that checks one literal would pass with the other binary unlisted.
if not re.search(r'foreach\s*\(\s*\$rel\s+in\s+\$required', dist):
    print("  coverage must iterate the required set, not name one path", file=sys.stderr)
    sys.exit(1)

for a, b, what in ((coverage, digest, "coverage before digest"),
                   (digest, exists, "digest before existence"),
                   (exists, copy, "existence before copy")):
    if not a < b:
        print(f"  out of order: {what}", file=sys.stderr)
        sys.exit(1)
PY

# ---------------------------------------------------------------------------
# REL2-03: nothing writes to PREFIX before the digest check.
#
# `New-Item`/`Copy-Item` under the prefix must not appear before the mismatch refusal. This
# is the property REL1-08 asserts for install.sh, and it is the one an installer most easily
# loses by "helpfully" creating the directory early.
# ---------------------------------------------------------------------------
mismatch_line=$(sed -n '/^function Install-FromDist/,/^}/p' "$ps1" | grep -n 'checksum mismatch' | head -1 | cut -d: -f1)
mkdir_line=$(sed -n '/^function Install-FromDist/,/^}/p' "$ps1" | grep -n 'New-Item -ItemType Directory' | head -1 | cut -d: -f1)
if [ -n "$mismatch_line" ] && [ -n "$mkdir_line" ] && [ "$mismatch_line" -lt "$mkdir_line" ]; then
	ok "REL2-03 the prefix is not created before the digest check"
else
	bad "REL2-03 install.ps1 creates \$Prefix/bin before verifying digests (mismatch line $mismatch_line, mkdir line $mkdir_line)"
fi

# ---------------------------------------------------------------------------
# REL2-04: the whitelist is exactly the two binaries — nothing else is ever copied.
#
# Counted inside Install-FromDist, because that is the path the SHA256SUMS contract governs.
# The release path copies the same two binaries by the same names, and counting the whole
# file would see four calls and call it a whitelist failure.
# ---------------------------------------------------------------------------
dist_body=$(sed -n '/^function Install-FromDist/,/^}/p' "$ps1")
n_copy=$(printf '%s\n' "$dist_body" | grep -c 'Copy-Item')
if [ "$n_copy" -eq 2 ] \
	&& printf '%s\n' "$dist_body" | grep -q "opencrayast.exe" \
	&& printf '%s\n' "$dist_body" | grep -q "opencrayast-mcp.exe"; then
	ok "REL2-04 exactly two binaries are copied, by name"
else
	bad "REL2-04 expected exactly 2 Copy-Item calls naming the two binaries, found $n_copy"
fi

# ---------------------------------------------------------------------------
# REL2-05: UTF-8 BOM in SHA256SUMS must not turn into a false refusal.
#
# A BOM before the first digest makes the first field "<BOM>deadbeef", which fails the
# digest check for a reason that has nothing to do with tampering. That is a false refusal —
# the same bug class as a false pass, and just as wrong.
# ---------------------------------------------------------------------------
if grep -q '0xFEFF' "$ps1"; then
	ok "REL2-05 a UTF-8 BOM in SHA256SUMS is stripped before parsing"
else
	bad "REL2-05 install.ps1 does not handle a UTF-8 BOM in SHA256SUMS"
fi

# ---------------------------------------------------------------------------
# REL2-06: the release path is tried first, and refusing it is no longer the contract.
#
# This case used to assert that `-FromRelease` exits 2 with a reason, which was right when the
# repository had no tagged release and the script had no URL to fetch. The release path is
# implemented now, so the contract is the opposite one: it is attempted, and an absent asset
# falls through to the dist tree and then to a build. A refusal would be the regression.
# ---------------------------------------------------------------------------
if grep -q 'Install-FromRelease' "$ps1" \
	&& grep -q 'Install-FromDist' "$ps1" \
	&& grep -q 'Install-FromSource' "$ps1" \
	&& ! grep -q 'download-from-release is not enabled yet' "$ps1"; then
	ok "REL2-06 the three install paths exist and no path invents an absent release"
else
	bad "REL2-06 install.ps1 must try release, then dist, then source"
fi

# ---------------------------------------------------------------------------
# REL2-07: no elevation.
#
# The Unix installer writes under a prefix the user owns. Asking for admin on Windows would
# make the common case — a per-user install — require a privilege it does not need.
# ---------------------------------------------------------------------------
if grep -qiE 'runas|Start-Process.*-Verb|#Requires -RunAsAdministrator' "$ps1"; then
	bad "REL2-07 install.ps1 must not request elevation"
else
	ok "REL2-07 install.ps1 does not request elevation"
fi

# ---------------------------------------------------------------------------
# REL2-08: the argument names match install.sh's, so a documented command works on both.
# ---------------------------------------------------------------------------
for arg in Prefix DistDir Target; do
	if ! grep -qE "\\\$$arg\b|\\\$$arg\b" "$ps1" && ! grep -q "$arg" "$ps1"; then
		bad "REL2-08 install.ps1 has no -$arg parameter"
		exit 1
	fi
done
ok "REL2-08 -Prefix / -DistDir / -Target are all present"

# ---------------------------------------------------------------------------
# REL2-09..11: the execution cases. Skipped loudly when there is no PowerShell.
# ---------------------------------------------------------------------------
run_exec_cases() {
	local pwsh_bin="$1"
	local label="$2"
	local tmp good bad_dir
	tmp=$(mktemp -d)
	good="$tmp/good"
	bad_dir="$tmp/bad"
	mkdir -p "$good/$label"
	# Stand-in binaries: the installer checks digests and names, not formats.
	printf 'MZ fake cli\n'  > "$good/$label/opencrayast.exe"
	printf 'MZ fake mcp\n'  > "$good/$label/opencrayast-mcp.exe"
	printf 'fake notices\n' > "$good/THIRD-PARTY-LICENSES.md"
	printf '1.0.0\n'        > "$good/VERSION"
	( cd "$good" && sha256sum "$label/opencrayast.exe" "$label/opencrayast-mcp.exe" > SHA256SUMS )

	# REL2-09: a good tree installs both.
	local dest="$tmp/ok"
	if "$pwsh_bin" -NoProfile -File "$ps1" -DistDir "$good" -Prefix "$dest" -Target "$label" >/dev/null 2>&1; then
		if [ -f "$dest/bin/opencrayast.exe" ] && [ -f "$dest/bin/opencrayast-mcp.exe" ]; then
			ok "REL2-09 $label: a good dist installs both binaries"
		else
			bad "REL2-09 $label: install.ps1 reported success but wrote nothing"
		fi
	else
		bad "REL2-09 $label: install.ps1 refused a good dist"
	fi

	# REL2-10: a corrupt digest refuses, and leaves the prefix without binaries.
	# sha256sum separator is not portable: GNU uses two spaces, some Windows builds
	# emit "hash *path" or a single space — flip the first hex digit without assuming
	# the separator (a prior `split("  ", 1)` crashed here and left the sums file good,
	# so install.ps1 correctly accepted it and the test blamed the installer).
	cp -a "$good" "$bad_dir"
	python3 - "$bad_dir/SHA256SUMS" <<'PY'
import pathlib, re, sys
p = pathlib.Path(sys.argv[1])
text = p.read_text()
def flip(m: re.Match[str]) -> str:
    h = m.group(1)
    return ("0" if h[0] != "0" else "1") + h[1:] + m.group(2)
new, n = re.subn(r"(?m)^([0-9a-fA-F]{64})(\s)", flip, text, count=1)
assert n == 1, f"no digest line to corrupt in {text!r}"
p.write_text(new)
PY
	dest2="$tmp/bad-prefix"
	if "$pwsh_bin" -NoProfile -File "$ps1" -DistDir "$bad_dir" -Prefix "$dest2" -Target "$label" >/dev/null 2>&1; then
		bad "REL2-10 $label: install.ps1 accepted a bad SHA256SUMS"
	elif [ -e "$dest2/bin/opencrayast.exe" ] || [ -e "$dest2/bin/opencrayast-mcp.exe" ]; then
		bad "REL2-10 $label: a refused install still wrote binaries"
	else
		ok "REL2-10 $label: a bad SHA256SUMS is refused and the prefix stays empty"
	fi

	# REL2-11: a truncated sums file must not let an uncovered binary through — even when
	# the remaining line still verifies.
	cp -a "$good" "$tmp/trunc"
	python3 - "$tmp/trunc" "$label" <<'PY'
import pathlib, sys
d = pathlib.Path(sys.argv[1]); t = sys.argv[2]
sums = d / "SHA256SUMS"
keep = [ln for ln in sums.read_text().splitlines(True) if ln.rstrip().endswith("/opencrayast-mcp.exe")]
assert len(keep) == 1, keep
sums.write_text("".join(keep))
cli = d / t / "opencrayast.exe"
cli.write_bytes(cli.read_bytes() + b"X")
PY
	dest3="$tmp/trunc-prefix"
	if "$pwsh_bin" -NoProfile -File "$ps1" -DistDir "$tmp/trunc" -Prefix "$dest3" -Target "$label" >/dev/null 2>&1; then
		bad "REL2-11 $label: a truncated SHA256SUMS still installed"
	elif [ -e "$dest3/bin/opencrayast.exe" ] || [ -e "$dest3/bin/opencrayast-mcp.exe" ]; then
		bad "REL2-11 $label: a truncated SHA256SUMS still wrote binaries"
	else
		ok "REL2-11 $label: a truncated SHA256SUMS is refused and nothing is written"
	fi

	rm -rf "$tmp"
}

if command -v pwsh >/dev/null 2>&1; then
	run_exec_cases pwsh x86_64-pc-windows-msvc
elif command -v powershell >/dev/null 2>&1; then
	run_exec_cases powershell x86_64-pc-windows-msvc
else
	skipped "REL2-09 a good dist installs both binaries" \
		"no pwsh/powershell on this machine; needs a Windows or PowerShell runner"
	skipped "REL2-10 a bad SHA256SUMS is refused" \
		"no pwsh/powershell on this machine; needs a Windows or PowerShell runner"
	skipped "REL2-11 a truncated SHA256SUMS is refused" \
		"no pwsh/powershell on this machine; needs a Windows or PowerShell runner"
fi

# ---------------------------------------------------------------------------
echo
printf 'rel2 self-test: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
if [ "$skip" -gt 0 ]; then
	echo "NOTE: the skipped cases are the three execution cases. They are NOT verified on this"
	echo "      machine. docs/RELEASE.md records this; do not read a green run as e2e proof."
fi
[ "$fail" -eq 0 ] || exit 1
exit 0