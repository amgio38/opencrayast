## What and why

<!-- What does this change, and what problem does it solve? Link the issue or ADR. -->

## How it was checked

<!-- Commands you ran and what they showed. For a bug fix, name the test that fails without the fix. -->

## Checklist

- [ ] If a guarantee, layering, contract or dependency policy changed: the documents and an ADR are updated first.
- [ ] Security-relevant change: the threat-model row, the test-catalogue entry and the test are added or updated, and `scripts/check-matrix.sh` passes.
- [ ] A safety check was shown to be tested by removing it and seeing a test fail.
- [ ] All file access goes through `Boundary`; `unsafe` stays forbidden (`unsafe_code = "forbid"`). Any future exception needs an ADR and a dedicated safety-boundary crate that does not exist today.
- [ ] Documentation claims match the tree: for doc-only PRs, search the Markdown for the claim (not one keyword) and list every hit; do not leave the same false present-tense fact in a second file.
- [ ] Linux, macOS and Windows are all considered (CI green on all three).
- [ ] `CHANGELOG.md` updated for user-visible changes.
- [ ] No secrets, personal paths, private tracker links or private data. Opaque work-item ids in commits are fine; do not link a private tracker.

By contributing you agree to follow the [Code of Conduct](../CODE_OF_CONDUCT.md) and that your contribution is licensed under the MIT licence.
