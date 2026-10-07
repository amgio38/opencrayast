# Release and REL1 checks for opencrayast.
# See docs/RELEASE.md. Contributor day-to-day commands stay in CONTRIBUTING.md.
#
# Shared-host caps (do not raise casually; see CONTRIBUTING.md):
CARGO_BUILD_JOBS ?= 8
RUST_TEST_THREADS ?= 4
export CARGO_BUILD_JOBS RUST_TEST_THREADS

TARGET ?= x86_64-unknown-linux-musl
DIST ?= dist
CARGO ?= cargo

.PHONY: release-static repro-check deny test-rel1 coverage coverage-gate test-install-posix verify-release \
	verify-release-strict \
	test-rel1-version test-rel1-deny test-rel1-layout \
	test-rel1-install-ok test-rel1-install-bad test-rel1-install-truncated \
	test-rel2 help

help:
	@echo "Targets:"
	@echo "  release-static   Build musl release binaries into $(DIST)/"
	@echo "  repro-check      Two clean builds; require identical SHA-256 (REL1-06)"
	@echo "  deny             cargo deny check (REL1-04)"
	@echo "  coverage         Collect line coverage and enforce the per-crate floor"
	@echo "  coverage-gate    Self-test: prove the coverage gate turns red when it should"
	@echo "  verify-release   Pre-release readiness checks (docs/RELEASE-CHECKLIST.md)"
	@echo "  test-rel1        REL1-01 … REL1-05, REL1-07 … REL1-10 (REL1-06 via repro-check)"
	@echo "  test-rel2        install.ps1 contract cases (execution cases skip without pwsh)"

# ---------------------------------------------------------------------------
# Line-coverage floor for core / edit / query.
#
# Needs the llvm-tools-preview component (`rustup component add llvm-tools-preview`).
# The floor lives in scripts/check-coverage.sh; this target only wires it up, so
# that CI and a maintainer's laptop enforce exactly the same number.
# ---------------------------------------------------------------------------

coverage:
	bash scripts/check-coverage.sh

coverage-gate:
	bash scripts/test-check-coverage.sh

# The README one-liner pipes install.sh into `sh` (dash on Debian/Ubuntu).
test-install-posix:
	bash scripts/test-install-posix.sh

# ---------------------------------------------------------------------------
# Pre-release readiness. `make verify-release` is the CI mode: mechanical checks,
# manual items reported PENDING. `make verify-release-strict` is the release mode
# and exits non-zero while any manual sign-off is open.
# ---------------------------------------------------------------------------

verify-release:
	bash scripts/verify-release.sh

verify-release-strict:
	bash scripts/verify-release.sh --require-manual

# ---------------------------------------------------------------------------
# §2–§3: static build → fixed dist/ layout
# ---------------------------------------------------------------------------

release-static:
	@command -v $(CARGO) >/dev/null
	$(CARGO) build --release --workspace --locked --target $(TARGET)
	rm -rf $(DIST)
	mkdir -p $(DIST)/$(TARGET)
	cp -f target/$(TARGET)/release/opencrayast \
		target/$(TARGET)/release/opencrayast-mcp \
		$(DIST)/$(TARGET)/
	cp -f LICENSE $(DIST)/LICENSE
	# Third-party licence notices ship WITH the artefact, not only in the repository. The
	# binaries statically link five tree-sitter grammars plus the Rust dependency tree, so a
	# tarball without this file redistributes licensed work with no notice attached — which is
	# the one thing the licences actually require. Generated from the lockfile, and staged
	# alongside LICENSE so the two travel together.
	@test -s THIRD-PARTY-LICENSES.md || { echo "release-static: THIRD-PARTY-LICENSES.md missing or empty (run: python3 scripts/gen-third-party-licenses.py)"; exit 1; }
	cp -f THIRD-PARTY-LICENSES.md $(DIST)/THIRD-PARTY-LICENSES.md
	@python3 -c 'import re, pathlib, sys; t=pathlib.Path("Cargo.toml").read_text(); m=re.search(r"(?ms)^\[workspace\.package\].*?^version\s*=\s*\"([^\"]+)\"", t); \
assert m, "no workspace.package version"; v=m.group(1); d=pathlib.Path("$(DIST)"); d.joinpath("VERSION").write_text(v+"\n"); \
d.joinpath("README.md").write_text("# opencrayast release artefacts\n\nVersion: `"+v+"`\n\nProduced by `make release-static`. From the repository root:\n\n```sh\n./install.sh --dist dist --prefix \"$$HOME/.local\"\n```\n\nVerify with `SHA256SUMS`. Download-from-release is not enabled yet (see docs/RELEASE.md).\n"); print("release-static: staged version", v)'
	cd $(DIST) && sha256sum $(TARGET)/opencrayast $(TARGET)/opencrayast-mcp > SHA256SUMS

# ---------------------------------------------------------------------------
# REL1-06: two clean builds, identical hashes
# ---------------------------------------------------------------------------

repro-check:
	@command -v $(CARGO) >/dev/null
	$(CARGO) clean --release --target $(TARGET)
	$(CARGO) build --release --workspace --locked --target $(TARGET)
	sha256sum target/$(TARGET)/release/opencrayast \
		target/$(TARGET)/release/opencrayast-mcp > /tmp/opencrayast-repro-1.sha256
	$(CARGO) clean --release --target $(TARGET)
	$(CARGO) build --release --workspace --locked --target $(TARGET)
	sha256sum target/$(TARGET)/release/opencrayast \
		target/$(TARGET)/release/opencrayast-mcp > /tmp/opencrayast-repro-2.sha256
	@diff -u /tmp/opencrayast-repro-1.sha256 /tmp/opencrayast-repro-2.sha256
	@echo "REL1-06 ok: two clean builds produced identical SHA-256"
	@cat /tmp/opencrayast-repro-1.sha256

deny:
	cargo deny check
	@echo "REL1-04 ok: cargo deny check"

# ---------------------------------------------------------------------------
# REL1 tests
# ---------------------------------------------------------------------------

test-rel1-version:
	@python3 -c 'import re,sys; from pathlib import Path; root=Path("Cargo.toml").read_text(); \
block=re.search(r"(?ms)^\[workspace\.package\](.*?)(?=^\[|\Z)", root); \
assert block, "REL1-01 FAIL: missing [workspace.package]"; \
versions=re.findall(r"(?m)^version\s*=\s*\"([^\"]+)\"", block.group(1)); \
assert len(versions)==1, versions; print("REL1-01 ok: workspace.package version=%r"%versions[0]); \
lit=re.compile(r"(?m)^version\s*=\s*\""); ws=re.compile(r"(?m)^version\.workspace\s*=\s*true\s*$$"); \
crates=sorted(Path("crates").glob("*/Cargo.toml")); assert crates; \
\
[sys.exit("REL1-02 FAIL: literal version in %s"%p) for p in crates if lit.search(p.read_text())]; \
[sys.exit("REL1-03 FAIL: missing version.workspace in %s"%p) for p in crates if not ws.search(p.read_text())]; \
print("REL1-02 ok: no literal version in %d crate manifests"%len(crates)); \
print("REL1-03 ok: version.workspace = true in %d crate manifests"%len(crates)); \
v=versions[0]; \
WSDEPS=("opencrayast-core","opencrayast-lang","opencrayast-query","opencrayast-edit","opencrayast-tools"); \
inline=re.compile(r"(?m)^(opencrayast-[a-z]+)\s*=\s*\{([^}]*)\}"); ver=re.compile(r"version\s*=\s*\"([^\"]+)\""); \
bad=[(m.group(1), ver.search(m.group(2)).group(1), w) for src,w in [(root,"Cargo.toml")]+[(p.read_text(),str(p)) for p in crates] \
     for m in inline.finditer(src) if m.group(1) in WSDEPS and ver.search(m.group(2)) \
     and ver.search(m.group(2)).group(1) != v]; \
sys.exit("REL1-10 FAIL: %s"%bad) if bad else None; \
n=sum(1 for src in [root]+[p.read_text() for p in crates] \
      for m in inline.finditer(src) if m.group(1) in WSDEPS and ver.search(m.group(2))); \
print("REL1-10 ok: %d workspace path deps pin version=%r"%(n, v))'

test-rel1-deny: deny

test-rel1-layout: release-static
	@python3 -c 'from pathlib import Path; import sys; dist=Path("dist"); \
req=["VERSION","LICENSE","THIRD-PARTY-LICENSES.md","README.md","SHA256SUMS","x86_64-unknown-linux-musl/opencrayast","x86_64-unknown-linux-musl/opencrayast-mcp"]; \
miss=[p for p in req if not (dist/p).is_file()]; \
assert not miss, "REL1-05 FAIL: missing %r"%miss; \
allowed={"VERSION","LICENSE","THIRD-PARTY-LICENSES.md","README.md","SHA256SUMS","x86_64-unknown-linux-musl"}; \
extra=sorted(p.name for p in dist.iterdir() if p.name not in allowed); \
assert not extra, "REL1-05 FAIL: unexpected %r"%extra; \
print("REL1-05 ok: dist/ layout matches docs/RELEASE.md")'

# --repo points at a throwaway name so a live GitHub release cannot satisfy these
# DistDir contract cases (install.sh tries the release asset first).
REL1_REPO ?= amgio38/opencrayast-no-such-repo-for-rel1

test-rel1-install-ok: release-static
	@tmpdir=$$(mktemp -d) && \
	./install.sh --dist dist --prefix "$$tmpdir" --target $(TARGET) --repo $(REL1_REPO) && \
	test -x "$$tmpdir/bin/opencrayast" && \
	test -x "$$tmpdir/bin/opencrayast-mcp" && \
	rm -rf "$$tmpdir" && \
	echo "REL1-07 ok: install.sh succeeded on a good dist/"

test-rel1-install-bad: release-static
	@set -e; \
	tmpdir=$$(mktemp -d); \
	baddir=$$(mktemp -d); \
	cp -a dist/. "$$baddir/"; \
	python3 -c 'import pathlib,sys; p=pathlib.Path(sys.argv[1]); lines=p.read_text().splitlines(True); h,r=lines[0].split("  ",1); lines[0]=("0" if h[0]!="0" else "1")+h[1:]+"  "+r; p.write_text("".join(lines))' "$$baddir/SHA256SUMS"; \
	if ./install.sh --dist "$$baddir" --prefix "$$tmpdir" --target $(TARGET) --repo $(REL1_REPO); then \
		echo "REL1-08 FAIL: install.sh accepted a bad SHA256SUMS" >&2; \
		rm -rf "$$tmpdir" "$$baddir"; \
		exit 1; \
	fi; \
	if [ -e "$$tmpdir/bin/opencrayast" ] || [ -e "$$tmpdir/bin/opencrayast-mcp" ]; then \
		echo "REL1-08 FAIL: bad install still wrote binaries under prefix" >&2; \
		rm -rf "$$tmpdir" "$$baddir"; \
		exit 1; \
	fi; \
	rm -rf "$$tmpdir" "$$baddir"; \
	echo "REL1-08 ok: bad SHA256SUMS refused; prefix left empty of binaries"

# REL1-09: truncated SHA256SUMS that still verifies for the remaining line must
# not install an uncovered (and possibly mutated) binary.
test-rel1-install-truncated: release-static
	@set -e; \
	tmpdir=$$(mktemp -d); \
	baddir=$$(mktemp -d); \
	cp -a dist/. "$$baddir/"; \
	python3 -c 'import pathlib,sys; d=pathlib.Path(sys.argv[1]); t=sys.argv[2]; \
sums=d/"SHA256SUMS"; keep=[ln for ln in sums.read_text().splitlines(True) if ln.rstrip().endswith("/opencrayast-mcp")]; \
assert len(keep)==1, keep; sums.write_text("".join(keep)); \
cli=d/t/"opencrayast"; cli.write_bytes(cli.read_bytes()+b"X")' "$$baddir" "$(TARGET)"; \
	if ./install.sh --dist "$$baddir" --prefix "$$tmpdir" --target $(TARGET) --repo $(REL1_REPO); then \
		echo "REL1-09 FAIL: truncated SHA256SUMS still installed" >&2; \
		rm -rf "$$tmpdir" "$$baddir"; \
		exit 1; \
	fi; \
	if [ -e "$$tmpdir/bin/opencrayast" ] || [ -e "$$tmpdir/bin/opencrayast-mcp" ]; then \
		echo "REL1-09 FAIL: truncated sums still wrote binaries under prefix" >&2; \
		rm -rf "$$tmpdir" "$$baddir"; \
		exit 1; \
	fi; \
	rm -rf "$$tmpdir" "$$baddir"; \
	echo "REL1-09 ok: truncated SHA256SUMS refused; prefix left empty of binaries"

# ---------------------------------------------------------------------------
# REL2 tests: the Windows installer.
#
# Unlike test-rel1, this needs no release build and no PowerShell to be meaningful: the nine
# contract cases check properties of install.ps1's text that decide whether it is safe, and
# they run everywhere. The three execution cases skip loudly without pwsh — see
# scripts/tests/rel2_install_ps1_spec.sh and docs/RELEASE.md §4a.
# ---------------------------------------------------------------------------

test-rel2:
	@bash scripts/tests/rel2_install_ps1_spec.sh

test-rel1: test-rel1-version test-rel1-deny test-rel1-layout test-rel1-install-ok test-rel1-install-bad test-rel1-install-truncated
	@echo "REL1: REL1-01…05, REL1-07…10 passed (run make repro-check for REL1-06)"
