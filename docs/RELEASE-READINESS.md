# Release readiness: recorded run

The output of `scripts/verify-release.sh` on the tree as of this commit, kept in the repository
so the checklist has a recorded result rather than a promise. The checklist itself, with the
reasoning behind each item, is in [`RELEASE-CHECKLIST.md`](RELEASE-CHECKLIST.md).

Re-run it with:

```sh
# Release mode: mechanical checks AND every manual item signed off.
bash scripts/verify-release.sh --require-manual

# After a person has actually confirmed each manual item.
bash scripts/verify-release.sh --ack-manual M1,M2,M3,M4,M5,M6,M7
```

## Recorded run

Rust `1.95.0`, `CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=4`, run with `--require-manual`. The
manual items are reported `PENDING` and the script exits **1** — which is the intended
behaviour, not a failure of the run: nothing has been signed off yet.

```
Community files
  PASS  PKG-01    LICENSE exists and is non-empty
  PASS  PKG-01    CONTRIBUTING.md exists and is non-empty
  PASS  PKG-01    CODE_OF_CONDUCT.md exists and is non-empty
  PASS  PKG-01    SECURITY.md exists and is non-empty
  PASS  PKG-01    CHANGELOG.md exists and is non-empty
  PASS  PKG-01    ROADMAP.md exists and is non-empty

Issue and PR templates
  PASS  PKG-02    .github/ISSUE_TEMPLATE/bug_report.yml
  PASS  PKG-02    .github/ISSUE_TEMPLATE/feature_request.yml
  PASS  PKG-02    .github/ISSUE_TEMPLATE/config.yml
  PASS  PKG-02    .github/pull_request_template.md

Dependency automation
  PASS  PKG-03    .github/dependabot.yml declares package ecosystems

Licence
  PASS  LIC-01    Cargo.toml license='MIT'; LICENSE starts with 'MIT License'

Licence text
  PASS  LIC-02    LICENSE contains the MIT grant text

Version consistency
  PASS  VER-01    version 0.20261002.1 (rust-version 1.95); 7 crates inherit it
  PASS  VER-03    toolchain 1.95.0 pinned; rust-version 1.95

Documentation checks
  PASS  DOC-01    scripts/check-docs.sh: documentation check passed: 25 files
  PASS  DOC-02    scripts/check-matrix.sh: matrix check passed: 124 catalogue rows (89 at or
                  below M4, 2 above it, 33 deferred); 91 targets resolved on disk; 124
                  identifiers referenced across 19 documents; 34 threats, each naming a test
  PASS  DOC-03    scripts/check-layering.sh: layering check passed: 7 crates

Dependency policy
  PASS  DENY-01   cargo deny check: advisories ok, bans ok, licenses ok, sources ok

CI wiring
  PASS  GIT-01    4 actions pinned to full SHAs
  PASS  CI-01     coverage gate and its self-test are both wired into ci.yml

Repository state
  PASS  GIT-02    working tree is clean

Changelog
  PASS  HIST-01   CHANGELOG.md has a current section

Manual sign-off (cannot be automated)
  PENDING M1    CI-green-on-all-three-OS
  PENDING M2    artifacts-built-and-checksums-recorded
  PENDING M3    changelog-entry-names-every-user-visible-change
  PENDING M4    security-supported-versions-still-true
  PENDING M5    history-scan-for-secrets-and-personal-data-approved
  PENDING M6    maintainer-reviewed-the-release-tag
  PENDING M7    vulnerability-reporting-path-rehearsed-once

summary: 22 passed, 1 failed, 0 skipped
```

## The two non-green results, kept rather than tidied away

**`GIT-02` failed on its own commit.** The script saw the files being committed as uncommitted
changes in the tree it was verifying. That is the check doing its job — it refuses to certify a
release from a dirty tree — and re-running it until it passed would have hidden the only
interesting thing in the output. On a clean checkout of the committed tree it passes.

## Publication path (ADR-016 / ADR-019)

Before the first public push, from the working tree:

```sh
bash scripts/prepublish-scan.sh
bash scripts/clean-export.sh /tmp/opencrayast-export
# then push the export directory to github.com/amgio38/opencrayast (human)
# then enable private vulnerability reporting on that repo (human)
```

Manual items `M1`–`M7` below stay `PENDING` until a person acknowledges them on a
real Actions run / clean-machine drill. They are **not** satisfied by this document
existing. Attestation is out of scope before 1.0 (ADR-019).

**The manual items are `PENDING` and the script exits 1.** By design. A release checklist that
reports success while six human judgements are unconfirmed is a checklist that has stopped
meaning anything, so the run is recorded as non-green and the sign-offs are left to a person.

## Not yet satisfied

Nothing has been signed off yet, and in particular the repository has **not** had the manual
history scan (`M5`) — the internal-repository-id and personal-mailbox rewrite work is a separate,
open item. Do not read the 22 mechanical passes as "ready to tag".