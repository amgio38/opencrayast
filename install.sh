#!/usr/bin/env bash
# Install opencrayast and opencrayast-mcp.
#
#   # one-line install (clones and builds, because there is no release yet):
#   curl -fsSL https://raw.githubusercontent.com/amgio38/opencrayast/main/scripts/install.sh | sh
#
#   # from a checkout, or with options:
#   sh scripts/install.sh --prefix "$HOME/.local"
#   sh scripts/install.sh --dist dist --prefix "$HOME/.local"
#
# Pass options through the pipe with `sh -s --`:
#
#   curl -fsSL https://raw.githubusercontent.com/amgio38/opencrayast/main/scripts/install.sh | sh -s -- --prefix /opt/ast
#
# Two paths, tried in this order:
#
#   1. a prebuilt GitHub release asset, verified against its published SHA-256;
#   2. a local dist/ artefact tree, every installed file verified against its
#      SHA256SUMS before anything is copied;
#   3. otherwise a cargo build, from this checkout or a shallow clone.
#
# Today the project has no tagged release, so path 1 finds nothing and the
# installer falls through to the build. When the first tag is pushed this same
# script prefers the prebuilt binary and no edit is needed: path 1 is tried first
# and simply succeeds.
#
# The script is wrapped in main() and only runs on its last line, so a download
# that is cut off part-way parses as an incomplete function and does nothing
# instead of executing a truncated script. That matters for `curl | sh`.
# Nothing here reads standard input (it is the script itself when piped), and
# child programs are given /dev/null.
#
# Both binaries are installed as a pair: they are staged and checked first, and
# only then moved into place. No path uses sudo, and none edits any client's
# configuration.
#
# The licence files go to $PREFIX/share/doc/opencrayast. A binary in $BINDIR is a
# redistribution, and the licences of what is bundled in it have to travel with
# it, so this installer puts them there rather than leaving them in the
# repository where a person who only has the binary cannot read them.
#
# Environment: PREFIX, DIST_DIR, TARGET, OPENCRAYAST_REPO, OPENCRAYAST_VERSION,
# OPENCRAYAST_INSECURE.
set -euo pipefail

DOC_SUBDIR='share/doc/opencrayast'

usage() {
	cat <<'EOF'
usage: install.sh [options]

  --prefix DIR    install binaries into DIR/bin (default: $PREFIX or ~/.local)
  --dist DIR      artefact tree to install from (default: ./dist)
  --target TRIPLE  subdirectory under dist/ (default: x86_64-unknown-linux-musl)
  --version TAG   release tag to fetch (default: the newest release)
  --from-source   skip release assets and the artefact tree; build with cargo
  --insecure      install a release asset with no published checksum
  --repo NAME     repository to fetch from (default: amgio38/opencrayast)
  -h, --help      show this help

Resolution order: a prebuilt release asset, then a local dist/ tree, then a
cargo build from this checkout or a shallow clone. Pass options through a pipe
with `sh -s --`.

A release asset is only installed after its published SHA-256 matches. If no
checksum is published, or no SHA-256 tool is installed, the installer stops
rather than installing an unverified binary -- `sha256sum` is not present on a
stock macOS, so failing open would skip verification for most of the platforms
this script is used from. Pass --insecure when reproducing a build yourself and
accepting the risk.

Every file installed from a dist/ tree must be covered by its SHA256SUMS, and
its digest must match. A truncated sums file stops the install rather than
letting an unchecked binary through.
EOF
}

# Defaults that do not depend on the arguments. Anything reading $HOME waits for
# main(), where an unset HOME can be reported in words.
PREFIX="${PREFIX:-}"
DIST_DIR="${DIST_DIR:-./dist}"
TARGET="${TARGET:-}"
VERSION="${OPENCRAYAST_VERSION:-}"
FROM_SOURCE=0
INSECURE="${OPENCRAYAST_INSECURE:-0}"
REPO="${OPENCRAYAST_REPO:-amgio38/opencrayast}"
OS_NAME=
PLATFORM_LABEL=
tmp=
stage=

die() {
	echo "install.sh: $1" >&2
	exit 1
}

have_downloader() {
	command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1
}

fetch() {
	if command -v curl >/dev/null 2>&1; then
		curl -fsSL --proto '=https' --tlsv1.2 --connect-timeout 10 --max-time 300 --retry 2 "$1" -o "$2"
	elif command -v wget >/dev/null 2>&1; then
		wget -q --https-only --connect-timeout=10 --timeout=300 --tries=3 -O "$2" "$1"
	else
		return 1
	fi
}

# The SHA-256 tool this system has, or empty when it has none.
#
# `sha256sum` is GNU coreutils; macOS ships `shasum` instead, so a check that
# only looked for the first would skip verification on exactly the platform that
# needs it most.
sha256_tool() {
	if command -v sha256sum >/dev/null 2>&1; then echo sha256sum
	elif command -v shasum >/dev/null 2>&1; then echo shasum
	elif command -v openssl >/dev/null 2>&1; then echo openssl
	else echo ""
	fi
}

digest_of() {
	case "$(sha256_tool)" in
		sha256sum) sha256sum "$1" | awk '{print tolower($1)}' ;;
		shasum) shasum -a 256 "$1" | awk '{print tolower($1)}' ;;
		openssl) openssl dgst -sha256 "$1" | awk '{print tolower($NF)}' ;;
		*) die "no SHA-256 tool found (sha256sum, shasum or openssl); nothing installed" ;;
	esac
}

# Decide whether this platform has a prebuilt asset. Sets OS_NAME, PLATFORM_LABEL
# and TARGET (empty when there is no prebuilt binary for this machine).
detect_platform() {
	OS_NAME=$(uname -s 2>/dev/null || echo unknown)
	arch=$(uname -m 2>/dev/null || echo unknown)
	PLATFORM_LABEL="$OS_NAME $arch"
	if [ -z "$TARGET" ]; then
		case "$OS_NAME" in
			Linux)
				case "$arch" in
					x86_64|amd64) TARGET=x86_64-unknown-linux-musl ;;
					aarch64|arm64) TARGET=aarch64-unknown-linux-musl ;;
				esac ;;
			Darwin) ;;
			MINGW*|MSYS*|CYGWIN*|Windows_NT)
				# The reading tools build here; only the write path is refused. So
				# this is a note, not a refusal: a source build is the right answer.
				echo "install.sh: note: native Windows has no prebuilt asset and the edit tools are unported; a source build gives you the reading tools." >&2 ;;
			*)
				echo "install.sh: note: $PLATFORM_LABEL is not a tested platform; trying a source build" >&2 ;;
		esac
	fi
}

# ---------------------------------------------------------------------------
# Artefact path: install from a local dist/ tree after verifying SHA256SUMS.
# ---------------------------------------------------------------------------

# True when the named path is listed in the sums file.
sums_lists() {
	awk -v want="$1" '
		NF >= 2 {
			# The digest is column 1 and the path is the rest of the line, so a
			# filename containing a space still matches.
			sub(/^[0-9a-fA-F]+[ \t]+[*]?/, "")
			if ($0 == want) { found = 1 }
		}
		END { exit(found ? 0 : 1) }
	' "$SUMS"
}

# The recorded digest for a path, lowercased.
sums_digest() {
	awk -v want="$1" '
		NF >= 2 {
			d = $1
			sub(/^[0-9a-fA-F]+[ \t]+[*]?/, "")
			if ($0 == want) { print tolower(d); found = 1; exit }
		}
		END { exit(found ? 0 : 1) }
	' "$SUMS"
}

digest_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | awk '{print tolower($1)}'
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | awk '{print tolower($1)}'
	elif command -v openssl >/dev/null 2>&1; then
		openssl dgst -sha256 "$1" | awk '{print tolower($NF)}'
	else
		die "no SHA-256 tool found (sha256sum, shasum or openssl); nothing installed"
	fi
}

install_from_dist() {
	SUMS="$DIST_DIR/SHA256SUMS"
	[ -f "$SUMS" ] || die "missing $SUMS"

	REL_CLI="${TARGET}/opencrayast"
	REL_MCP="${TARGET}/opencrayast-mcp"

	# Every installed file must be covered by SHA256SUMS — not only the files that
	# happen to be listed. A truncated sums file must not let an unchecked binary
	# through.
	for rel in "$REL_CLI" "$REL_MCP"; do
		sums_lists "$rel" || die "$rel is not listed in $SUMS; refusing to install an unchecked binary"
		f="$DIST_DIR/$rel"
		[ -f "$f" ] || die "missing binary: $f"
		want=$(sums_digest "$rel") || die "cannot read the digest for $rel"
		got=$(digest_of "$f")
		[ "$want" = "$got" ] || die "checksum mismatch for $rel (expected $want, got $got); nothing installed"
	done

	# Stage first, copy after every digest has been checked: a failure leaves the
	# previous installation untouched rather than half-replaced.
	stage="$tmp/stage"
	mkdir -p "$stage"
	install -m 0755 "$DIST_DIR/$REL_CLI" "$stage/opencrayast"
	install -m 0755 "$DIST_DIR/$REL_MCP" "$stage/opencrayast-mcp"

	mkdir -p "$PREFIX/bin"
	install -m 0755 "$stage/opencrayast" "$PREFIX/bin/opencrayast"
	install -m 0755 "$stage/opencrayast-mcp" "$PREFIX/bin/opencrayast-mcp"
	echo "install.sh: installed opencrayast and opencrayast-mcp into $PREFIX/bin"

	install_doc_files "$DIST_DIR"
}

# ---------------------------------------------------------------------------
# Release path: a prebuilt asset from the newest (or a named) GitHub release.
# Returns non-zero when there is nothing to install, so the caller falls through
# to the dist/ tree or the build rather than failing.
# ---------------------------------------------------------------------------

verify_checksum() {
	base=$1
	asset=$2
	want=
	if fetch "$base/$asset.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
		want=$(awk 'NR==1 {print tolower($1)}' "$tmp/$asset.sha256")
	fi
	if [ -z "$want" ] || ! printf '%s' "$want" | grep -Eq '^[0-9a-f]{64}$'; then
		if [ "$INSECURE" != "1" ]; then
			echo "install.sh: no published SHA-256 for $asset; refusing to install an unverified binary." >&2
			echo "install.sh: pass --insecure (or OPENCRAYAST_INSECURE=1) to accept it anyway." >&2
			return 1
		fi
		echo "install.sh: --insecure: installing $asset with no checksum" >&2
		return 0
	fi
	if [ -z "$(sha256_tool)" ]; then
		echo "install.sh: no SHA-256 tool (sha256sum, shasum or openssl); refusing to install an unverified binary." >&2
		return 1
	fi
	got=$(digest_of "$tmp/$asset")
	[ "$want" = "$got" ] || {
		echo "install.sh: checksum mismatch for $asset (expected $want, got $got); nothing installed" >&2
		return 1
	}
	echo "install.sh: checksum verified for $asset"
}

install_from_release() {
	[ "$FROM_SOURCE" -eq 0 ] || return 1
	if [ -z "$TARGET" ]; then
		echo "install.sh: no prebuilt asset for $PLATFORM_LABEL; building from source" >&2
		return 1
	fi
	have_downloader || {
		echo "install.sh: neither curl nor wget is installed; build from a checkout with --from-source" >&2
		return 1
	}

	if [ -n "$VERSION" ]; then
		base="https://github.com/$REPO/releases/download/$VERSION"
	else
		base="https://github.com/$REPO/releases/latest/download"
	fi
	asset="opencrayast-$TARGET.tar.gz"

	echo "install.sh: fetching $base/$asset" >&2
	fetch "$base/$asset" "$tmp/$asset" 2>/dev/null || {
		echo "install.sh: no release asset there; building from source" >&2
		return 1
	}
	verify_checksum "$base" "$asset" || return 1

	# The archive carries the two programs and, under one directory, the licence
	# files that have to travel with a redistributed binary. The two programs are an
	# exact allowlist: anything that is neither one of them nor under $DOC_SUBDIR is
	# refused, and a member with '..' in its path cannot write outside $tmp.
	tar -tzf "$tmp/$asset" | sed 's|^\./||' | LC_ALL=C sort > "$tmp/members"
	programs=
	doc_members=
	while IFS= read -r m; do
		[ -n "$m" ] || continue
		case "$m" in
		opencrayast|opencrayast-mcp) programs="$programs $m" ;;
		"$DOC_SUBDIR"/*)
			case "/$m/" in
			*/../*) die "refusing a member with '..' in $asset: $m" ;;
			esac
			case "$m" in
			*/) ;;
			*) doc_members="$doc_members $m" ;;
			esac ;;
		*) die "unexpected member in $asset: $m" ;;
		esac
	done < "$tmp/members"
	# Sorted above, so this is an order-independent exact allowlist: the two programs
	# and nothing else at the top level.
	[ "$programs" = " opencrayast opencrayast-mcp" ] || die "unexpected programs in $asset: $programs"
	# shellcheck disable=SC2086
	tar --no-same-owner --no-same-permissions -xzf "$tmp/$asset" -C "$tmp" \
		opencrayast opencrayast-mcp $doc_members || die "cannot unpack $asset"
	[ -f "$tmp/opencrayast" ] && [ -f "$tmp/opencrayast-mcp" ] ||
		die "release asset did not contain opencrayast and opencrayast-mcp"

	stage="$tmp/stage"
	mkdir -p "$stage"
	install -m 0755 "$tmp/opencrayast" "$stage/opencrayast"
	install -m 0755 "$tmp/opencrayast-mcp" "$stage/opencrayast-mcp"

	mkdir -p "$PREFIX/bin"
	install -m 0755 "$stage/opencrayast" "$PREFIX/bin/opencrayast"
	install -m 0755 "$stage/opencrayast-mcp" "$PREFIX/bin/opencrayast-mcp"
	echo "install.sh: installed opencrayast and opencrayast-mcp into $PREFIX/bin"

	install_doc_files "$tmp/$DOC_SUBDIR"
}

# ---------------------------------------------------------------------------
# Build path: what `curl | sh` uses while there is no tagged release.
# ---------------------------------------------------------------------------

install_from_source() {
	command -v cargo >/dev/null 2>&1 ||
		die "cargo is not installed. Install a Rust toolchain (1.95.0, see rust-toolchain.toml) and retry."

	repo=$PWD
	if [ ! -f "$repo/Cargo.toml" ] || [ ! -d "$repo/crates/cli" ] || [ ! -d "$repo/crates/mcp" ]; then
		command -v git >/dev/null 2>&1 || die "not in a checkout and git is not installed"
		repo="$tmp/src"
		echo "install.sh: cloning https://github.com/$REPO" >&2
		# --depth 1 and no --branch: a shallow clone of the default branch. The URL
		# follows the flag, so a REPO beginning with a dash cannot be read as one.
		git clone --depth 1 "https://github.com/$REPO" "$repo" </dev/null >&2 ||
			die "cannot clone https://github.com/$REPO"
	fi

	echo "install.sh: building with cargo (this can take a few minutes)" >&2
	build_root="$tmp/cargo-root"
	mkdir -p "$build_root"
	# Build into a private root first: the pair is checked and then installed, and no
	# cargo bookkeeping is left in $PREFIX.
	cargo install --quiet --locked --path "$repo/crates/cli" --root "$build_root" </dev/null
	cargo install --quiet --locked --path "$repo/crates/mcp" --root "$build_root" </dev/null
	[ -x "$build_root/bin/opencrayast" ] || die "cargo did not produce $build_root/bin/opencrayast"
	[ -x "$build_root/bin/opencrayast-mcp" ] || die "cargo did not produce $build_root/bin/opencrayast-mcp"

	stage="$tmp/stage"
	mkdir -p "$stage"
	install -m 0755 "$build_root/bin/opencrayast" "$stage/opencrayast"
	install -m 0755 "$build_root/bin/opencrayast-mcp" "$stage/opencrayast-mcp"

	mkdir -p "$PREFIX/bin"
	install -m 0755 "$stage/opencrayast" "$PREFIX/bin/opencrayast"
	install -m 0755 "$stage/opencrayast-mcp" "$PREFIX/bin/opencrayast-mcp"
	echo "install.sh: installed opencrayast and opencrayast-mcp into $PREFIX/bin"

	install_doc_files "$repo"
}

# Put the licence files where a person who only has the installed binaries can
# read them: a binary that statically links five tree-sitter grammars plus the
# Rust dependency tree redistributes licensed work, and the licences require the
# notice to travel with it.
install_doc_files() {
	repo=$1
	docdir="$PREFIX/$DOC_SUBDIR"
	mkdir -p "$docdir"
	for f in LICENSE THIRD-PARTY-LICENSES.md; do
		if [ -f "$repo/$f" ]; then
			install -m 0644 "$repo/$f" "$docdir/$f"
		fi
	done
	echo "install.sh: licence files installed into $docdir"
}

report_next_step() {
	case ":$PATH:" in
		*":$PREFIX/bin:"*) ;;
		*)
			echo ""
			echo "Add $PREFIX/bin to your PATH:"
			echo "    export PATH=\"$PREFIX/bin:\$PATH\""
			;;
	esac
	echo ""
	echo "Register the MCP server with your agent:"
	echo "    claude mcp add opencrayast -- opencrayast-mcp --workspace ."
	echo ""
	echo "Check the installation:"
	echo "    opencrayast doctor"
}

main() {
	while [ $# -gt 0 ]; do
		case "$1" in
			--prefix)
				PREFIX="${2:?--prefix requires a directory}"
				shift 2
				;;
			--dist)
				DIST_DIR="${2:?--dist requires a directory}"
				shift 2
				;;
			--target)
				TARGET="${2:?--target requires a triple}"
				shift 2
				;;
			--from-source)
				FROM_SOURCE=1
				shift
				;;
			--version)
				VERSION="${2:?--version requires a tag}"
				shift 2
				;;
			--insecure)
				INSECURE=1
				shift
				;;
			--repo)
				REPO="${2:?--repo requires owner/name}"
				shift 2
				;;
			-h|--help)
				usage
				return 0
				;;
			*)
				echo "install.sh: unknown argument: $1" >&2
				usage >&2
				return 2
				;;
		esac
	done

	if [ -z "$PREFIX" ]; then
		if [ -z "${HOME:-}" ]; then
			echo "install.sh: PREFIX is unset and HOME is unset." >&2
			echo "install.sh: pass --prefix DIR (or set PREFIX), or export HOME; nothing installed." >&2
			return 1
		fi
		PREFIX="${HOME}/.local"
	fi

	tmp=$(mktemp -d "${TMPDIR:-/tmp}/opencrayast-install.XXXXXX") ||
		die "cannot create a temporary directory"

	detect_platform

	# 1. a prebuilt release asset, when this platform has one and a release exists.
	if ! install_from_release; then
		# 2. a local artefact tree, when the caller has one.
		if [ "$FROM_SOURCE" -eq 0 ] && [ -d "$DIST_DIR" ]; then
			install_from_dist
		else
			# 3. a cargo build. Say what is happening: the next thing is a
			# multi-minute build and silence would read as a hang.
			if [ ! -d "$DIST_DIR" ]; then
				echo "install.sh: no release asset and no artefact tree at $DIST_DIR; building from source." >&2
				echo "install.sh: this needs a Rust toolchain (1.95.0) and a C compiler." >&2
			fi
			install_from_source
		fi
	fi

	rm -rf "$tmp"
	report_next_step
}

main "$@"