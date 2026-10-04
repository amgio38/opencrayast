# Security policy

## Reporting a vulnerability

**Please do not open a public issue for a security problem.** Report it privately
through GitHub's private vulnerability reporting for the public repository
[`amgio38/opencrayast`](https://github.com/amgio38/opencrayast) (Security tab →
*Report a vulnerability*). Include what you ran, what you expected, what happened,
and — if you can — a minimal reproduction. A report that shows a way to read or write
outside the workspace, to apply a change that was not previewed, to leave the
workspace half-modified, or to make a write tool available in read-only mode is
exactly what we want to hear about.

Enabling that GitHub setting is a one-time maintainer action after the first public
push (ADR-016). The text here is the channel; the switch lives in the repository
settings.

We aim to acknowledge a report within **7 days** and to tell you our assessment and
plan within **14 days**. We will credit you in the advisory unless you prefer not to
be named.

### Has this been rehearsed?

**Not yet, and that is a known gap.** The SLA above is a commitment nobody has yet
tested end to end. What has been checked is that the *documents* are consistent with
each other: this page, the invariants in `docs/SECURITY-MODEL.md` and the evidence
table in `docs/TESTING.md` describe the same promises, and the private-reporting
channel above is the one the project intends to use.

The part that has **not** been rehearsed is a person receiving a real report and
working the clock against those two deadlines. Until that happens, treat the 7 and 14
day figures as untested. Rehearsing it — a synthetic report, walked from receipt to
assessment by whoever would hold the maintainer role — is a release gate item before
1.0, tracked in [`docs/RELEASE-READINESS.md`](docs/RELEASE-READINESS.md).

## Supported versions

Until 1.0, only the latest release receives fixes. After 1.0 the supported versions
will be listed here.

## What the project promises

The promises are the *security invariants* S-1 … S-10 in
[`docs/SECURITY-MODEL.md`](docs/SECURITY-MODEL.md), each backed by named tests listed
in [`docs/TESTING.md`](docs/TESTING.md); the promise-to-evidence table that maps each
invariant to those tests is checked by
`crates/tools/tests/security_promises_spec.rs`. In short:

- No file outside the configured workspace is read or written through any tool.
- Nothing is written unless write mode was explicitly enabled; in read-only mode the
  write tools do not exist.
- What is written is exactly what was previewed, and a failed apply leaves the
  workspace unchanged or recoverable to unchanged.
- Nothing is executed and no network access is made.
- Every output is bounded.

If you find that a documented promise does not hold, that is a vulnerability, even if
it would not be one in a tool that made no such promise.

## What it does not promise

Also in the model, and worth reading before relying on it:

- It cannot tell a harmful change from a helpful one. Review plans; prefer read-only
  mode with human application; require client-side approval for apply.
- Returned source can contain text that tries to steer an AI agent. Treat it as data.
- Until the isolated parse worker ships (roadmap M6), the parser runs in the server
  process. Pre-1.0 releases state this in their notes.
- Someone who already runs code as your user is inside the trust boundary.

## Scope

In scope: the `opencrayast-mcp` and `opencrayast` programs, the installers, the
release process and the documentation's security claims. Out of scope: vulnerabilities
in third-party language grammars that do not affect this tool's guarantees (report
them upstream; tell us if our mitigations are insufficient), and attacks that require
prior code execution as the same user.

## Safe harbour

Good-faith research that stays within the scope above, avoids harming others' data,
and gives us a reasonable chance to fix the issue before disclosure will not be
treated as a violation of this policy.
