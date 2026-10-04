#!/usr/bin/env bash
# Enforce crate layering for the opencrayast workspace.
#
# Invariant (docs/ARCHITECTURE.md «Layers and crates»):
#   dependency edges only point downward; no path dependency may leave the
#   workspace.
#
# Allowed workspace edges (package name → allowed workspace deps):
#   opencrayast-core  → (none)
#   opencrayast-lang  → opencrayast-core
#   opencrayast-query → opencrayast-core, opencrayast-lang
#   opencrayast-edit  → opencrayast-core, opencrayast-lang, opencrayast-query
#   opencrayast-tools → opencrayast-core, opencrayast-lang, opencrayast-query, opencrayast-edit
#   opencrayast-mcp   → opencrayast-tools, opencrayast-core
#   opencrayast (cli) → opencrayast-tools, opencrayast-core, opencrayast-edit
#
# [dev-dependencies] and [build-dependencies] are checked the same way
# (core must not take a workspace crate as a dev-dependency either).
#
# Usage: check-layering.sh [--root DIR]
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
while [ $# -gt 0 ]; do
	case "$1" in
		--root)
			root=$(CDPATH= cd -- "$2" && pwd)
			shift 2
			;;
		-h|--help)
			echo "usage: check-layering.sh [--root DIR]"
			exit 0
			;;
		*)
			echo "unknown argument: $1" >&2
			exit 2
			;;
	esac
done

crates_dir="$root/crates"
if [ ! -d "$crates_dir" ]; then
	echo "error: no crates/ directory under $root" >&2
	exit 2
fi

# Resolve a path (absolute or relative to $1) to a canonical absolute path when
# the target exists; otherwise return a best-effort absolute form. Portable
# across Linux and macOS (no GNU realpath -m).
resolve_path() {
	local base=$1 rel=$2 target parent
	case "$rel" in
		/*) target=$rel ;;
		*) target=$base/$rel ;;
	esac
	if [ -d "$target" ]; then
		CDPATH= cd -- "$target" && pwd
		return
	fi
	parent=$(dirname -- "$target")
	if [ -d "$parent" ]; then
		printf '%s/%s\n' "$(CDPATH= cd -- "$parent" && pwd)" "$(basename -- "$target")"
		return
	fi
	# Non-existent parent (e.g. /opt/external): keep absolute spelling.
	case "$target" in
		/*) printf '%s\n' "$target" ;;
		*) printf '%s/%s\n' "$base" "$rel" ;;
	esac
}

# Emit: package<TAB>dep<TAB>section<TAB>kind<TAB>value<TAB>crate_dir
# kind is path|git| (empty for crates.io / workspace = true).
edges() {
	local manifest
	for manifest in "$crates_dir"/*/Cargo.toml; do
		[ -f "$manifest" ] || continue
		awk '
			/^\[package\]/ { section = "package"; next }
			/^\[dependencies\]/ { section = "dependencies"; next }
			/^\[dev-dependencies\]/ { section = "dev-dependencies"; next }
			/^\[build-dependencies\]/ { section = "build-dependencies"; next }
			/^\[.*dependencies\]/ { section = "dependencies"; next }
			/^\[/ { section = ""; next }
			section == "package" && /^[[:space:]]*name[[:space:]]*=/ {
				name = $0
				sub(/^[^=]*=[[:space:]]*/, "", name)
				gsub(/["[:space:]]/, "", name)
				next
			}
			(section == "dependencies" || section == "dev-dependencies" || section == "build-dependencies") \
				&& /^[[:space:]]*[A-Za-z0-9_-]+(\.workspace)?[[:space:]]*=/ {
				line = $0
				dep = line
				sub(/[[:space:]]*=.*/, "", dep)
				gsub(/^[[:space:]]+/, "", dep)
				# Cargo shorthand: `foo.workspace = true` → package name is `foo`.
				if (dep ~ /\.workspace$/) {
					sub(/\.workspace$/, "", dep)
				}
				kind = ""
				value = ""
				if (match(line, /path[[:space:]]*=[[:space:]]*"[^"]*"/)) {
					value = substr(line, RSTART, RLENGTH)
					sub(/^path[[:space:]]*=[[:space:]]*"/, "", value)
					sub(/"$/, "", value)
					kind = "path"
				} else if (match(line, /git[[:space:]]*=[[:space:]]*"[^"]*"/)) {
					value = substr(line, RSTART, RLENGTH)
					sub(/^git[[:space:]]*=[[:space:]]*"/, "", value)
					sub(/"$/, "", value)
					kind = "git"
				}
				crate_dir = FILENAME
				sub(/\/Cargo\.toml$/, "", crate_dir)
				printf "%s\t%s\t%s\t%s\t%s\t%s\n", name, dep, section, kind, value, crate_dir
			}
		' "$manifest"
	done
}

workspace_names() {
	local manifest
	for manifest in "$crates_dir"/*/Cargo.toml; do
		[ -f "$manifest" ] || continue
		awk '
			/^\[package\]/ { p = 1; next }
			/^\[/ { p = 0 }
			p && /^[[:space:]]*name[[:space:]]*=/ {
				n = $0
				sub(/^[^=]*=[[:space:]]*/, "", n)
				gsub(/["[:space:]]/, "", n)
				print n
			}
		' "$manifest"
	done
}

# Allowed workspace deps for each package (space-separated, trailing spaces ok).
# Empty value means no workspace crate is allowed.
allowed_for() {
	case "$1" in
		opencrayast-core)  echo "" ;;
		opencrayast-lang)  echo "opencrayast-core" ;;
		opencrayast-query) echo "opencrayast-core opencrayast-lang" ;;
		opencrayast-edit)  echo "opencrayast-core opencrayast-lang opencrayast-query" ;;
		opencrayast-tools) echo "opencrayast-core opencrayast-lang opencrayast-query opencrayast-edit" ;;
		opencrayast-mcp)   echo "opencrayast-tools opencrayast-core" ;;
		opencrayast)       echo "opencrayast-tools opencrayast-core opencrayast-edit" ;;
		*)                 echo "" ;; # unknown crate: no workspace deps allowed
	esac
}

is_workspace_crate() {
	case " $ws_names " in
		*" $1 "*) return 0 ;;
		*) return 1 ;;
	esac
}

dep_allowed() {
	local pkg=$1 dep=$2 allow
	allow=$(allowed_for "$pkg")
	case " $allow " in
		*" $dep "*) return 0 ;;
		*) return 1 ;;
	esac
}

reason_for_edge() {
	local pkg=$1 dep=$2
	case "$pkg" in
		opencrayast-core)
			printf 'core must not depend on any workspace crate'
			;;
		opencrayast-lang)
			printf 'lang may only depend on: opencrayast-core'
			;;
		opencrayast-query)
			printf 'query may only depend on: opencrayast-core, opencrayast-lang'
			;;
		opencrayast-edit)
			printf 'edit may only depend on: opencrayast-core, opencrayast-lang, opencrayast-query'
			;;
		opencrayast-tools)
			printf 'tools may only depend on: opencrayast-core, opencrayast-lang, opencrayast-query, opencrayast-edit'
			;;
		opencrayast-mcp)
			printf 'mcp may only depend on: opencrayast-tools, opencrayast-core'
			;;
		opencrayast)
			printf 'cli may only depend on: opencrayast-tools, opencrayast-core, opencrayast-edit'
			;;
		*)
			printf 'crate is not in the layering table; no workspace dependency is allowed'
			;;
	esac
}

ws_names=$(workspace_names | tr '\n' ' ')
crate_count=0
for _m in "$crates_dir"/*/Cargo.toml; do
	[ -f "$_m" ] || continue
	crate_count=$((crate_count + 1))
done

violations=0
report() {
	# $1 = crate, $2 = dep, $3 = reason
	violations=$((violations + 1))
	printf 'VIOLATION: %s -> %s (%s)\n' "$1" "$2" "$3"
}

while IFS=$'\t' read -r pkg dep section kind value crate_dir; do
	[ -n "${pkg:-}" ] || continue
	[ -n "${dep:-}" ] || continue

	# Path must stay inside the workspace root.
	if [ "$kind" = "path" ]; then
		resolved=$(resolve_path "$crate_dir" "$value")
		case "$resolved" in
			"$root"|"$root"/*) ;;
			*)
				report "$pkg" "$dep" "path dependency outside the workspace"
				continue
				;;
		esac
	fi

	# Git deps are not crates.io and not workspace members → treat as external.
	if [ "$kind" = "git" ]; then
		report "$pkg" "$dep" "git dependency is not allowed (must be crates.io or a workspace crate)"
		continue
	fi

	# Layering: only when the dependency names a workspace crate.
	if is_workspace_crate "$dep"; then
		if ! dep_allowed "$pkg" "$dep"; then
			report "$pkg" "$dep" "$(reason_for_edge "$pkg" "$dep")"
		fi
	fi
done < <(edges)

if [ "$violations" -ne 0 ]; then
	exit 1
fi
printf 'layering check passed: %s crates\n' "$crate_count"
