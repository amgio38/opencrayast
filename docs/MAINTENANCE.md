# Maintenance

How maintainers cut a release, keep the version number truthful, and keep the
changelog in step with what actually shipped. Contributor day-to-day setup lives
in [`CONTRIBUTING.md`](../CONTRIBUTING.md). The pre-release checkbox list is
[`PRERELEASE.md`](PRERELEASE.md).

## Version number: one source of truth

Until 1.0 the project uses the scheme in ADR-013
([`DECISIONS.md`](DECISIONS.md#adr-013-version-scheme)):

| Surface | Form | Example |
|---|---|---|
| Human / changelog label | `V0.YYYYMMDD.NNN` | `V0.20261002.001` |
| Cargo crate version | `0.YYYYMMDD.N` (no leading zeros) | `0.20261002.1` |
| Git tag | `v0.YYYYMMDD.N` | `v0.20261002.1` |

**Sole source of truth:** `[workspace.package].version` in the root
[`Cargo.toml`](../Cargo.toml). Every crate inherits it via
`version.workspace = true`. Do not invent a second version string in code,
installers or documents.

Rules:

1. Bump `Cargo.toml` in the same commit that opens a release.
2. The Git tag **must** equal `v` plus that Cargo version. A release workflow
   (roadmap M7) must refuse a tag that disagrees.
3. At 1.0 the scheme becomes ordinary semantic versioning; update this section
   and ADR-013 together when that happens.

## Changelog ("what's new") sync

User-facing history lives in [`CHANGELOG.md`](../CHANGELOG.md) (Keep a Changelog
shape). There is no separate `whats_new` file.

Sync rules:

1. **While work is open:** put notes under `## [Unreleased]`, grouped as Added /
   Changed / Fixed / Security as appropriate.
2. **When cutting a release:** rename `[Unreleased]` to the human label
   (`[V0.YYYYMMDD.NNN] - YYYY-MM-DD`), leave a fresh empty `[Unreleased]` section
   at the top, and make sure every user-visible change in the tag is mentioned.
3. **Same PR as the bump:** version bump, changelog section cut, and any
   `THIRD-PARTY-LICENSES.md` refresh travel together.
4. **Security advisories** get a `### Security` subsection and a private report
   path as in [`SECURITY.md`](../SECURITY.md); do not bury them only in commit
   messages.
5. Milestone mutation self-proof notes stay under the changelog's mutation
   section (see [`TESTING.md`](TESTING.md)), not as a substitute for the release
   section.

## Licence metadata

`[workspace.package].license` in `Cargo.toml` must match the licence file at the
repository root. Today that is `MIT` and [`LICENSE`](../LICENSE) (MIT License
text). Before every release, run the reconcile check in
[`PRERELEASE.md`](PRERELEASE.md#licence-and-cargo-metadata).

## Release flow (until the M7 pipeline exists)

1. Branch from `main`, finish the change, open a PR.
2. Walk [`PRERELEASE.md`](PRERELEASE.md) and keep the checked results with the
   release notes (PR description or a linked comment).
3. Merge only when CI on Linux, macOS and Windows is green
   ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)).
4. On `main`, set `Cargo.toml` version, cut the changelog section, commit.
5. Tag `v` + Cargo version. Do not retag; fix forward with a new patch `N`.
6. Build release artefacts (static Linux musl binaries as in
   [`CONTRIBUTING.md`](../CONTRIBUTING.md#release-and-static-builds); macOS and
   Windows natives when their toolchains are available). Publish checksums with
   the artefacts once the M7 installers land.
7. Confirm [`SECURITY.md`](../SECURITY.md) still describes the supported-version
   policy you intend for this tag.

## Dependency and advisory hygiene

- Dependabot config: [`.github/dependabot.yml`](../.github/dependabot.yml).
- Licence and advisory policy: [`deny.toml`](../deny.toml); run
  `cargo deny check` locally and in CI.
- Refresh [`THIRD-PARTY-LICENSES.md`](../THIRD-PARTY-LICENSES.md) with
  `scripts/gen-third-party-licenses.py` when the lockfile's runtime set changes.
  The generator also rewrites the grammar provenance table (source, upstream
  commit, build-script behaviour) from the extracted crate sources, so re-running
  it after a grammar bump is what records the new upstream commit.
- Grammar provenance is reconciled against the lockfile in CI by
  `scripts/check-grammar-provenance.sh`; it is not a `cargo deny` concern, because
  `cargo deny` reads Rust metadata and never sees the C a grammar ships.
- Vulnerability intake and timelines: [`SECURITY.md`](../SECURITY.md).

## Documents that must stay true

After any behavioural change, the documents that name the behaviour are part of
the change: at least [`TOOLS.md`](TOOLS.md), [`SECURITY-MODEL.md`](SECURITY-MODEL.md),
[`TESTING.md`](TESTING.md), and any ADR you rely on. `scripts/check-docs.sh`,
`scripts/check-matrix.sh`, and `scripts/check-undeclared-src.sh` catch link,
matrix, and leftover-source drift; they do not replace reading the prose.
