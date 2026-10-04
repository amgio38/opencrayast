# Spike: ADR-005 matching engine

**Date:** 2026-10-02  
**Status:** Evidence for the decision in [`DECISIONS.md` ADR-005](../DECISIONS.md#adr-005-matching-engine)  
**Scope:** Throwaway experiments outside the workspace (no `crates/` changes, no workspace
dependency edits). Scratch crates lived under a temp directory on the builder; only this
document is committed.

Evaluated **ast-grep-core 0.45.3** (MIT, optional `tree-sitter` feature) against a
**purpose-built structural matcher** over our pinned **tree-sitter 0.27**.

---

## 1. Licence and supply chain

### ast-grep-core 0.45.3 direct dependencies

| Crate | Requirement | Licence (crates.io metadata) |
|---|---|---|
| `ast-grep-core` | 0.45.3 | MIT |
| `tree-sitter` | `^0.27.0` (optional, default on) | MIT |
| `bit-set` | `^0.11` | Apache-2.0 OR MIT |
| `regex` | `^1.10` | MIT OR Apache-2.0 |
| `thiserror` | `^2` | MIT OR Apache-2.0 |

`tree-sitter-typescript` appears only as a **dev-dependency** of ast-grep-core (not
pulled into a normal build).

### Scratch crate resolve (ast-grep-core + tree-sitter 0.27 + tree-sitter-javascript)

`cargo deny check licenses` against this repository's `deny.toml` allow-list:
**`licenses ok`** (after giving the unpublished scratch package `license = "MIT"`).

Resolved package count in that graph: **40** crates (including build-time helpers such
as `cc`, `syn`). Unique package names from `cargo tree --prefix none`: about **29**.

Notable licence expressions encountered: MIT, Apache-2.0 OR MIT, Unlicense OR MIT
(`aho-corasick`, `memchr` via `regex`), Unicode-3.0 (via `unicode-ident`). All fit the
current allow-list.

### tree-sitter version coexistence (primary risk — measured)

| Experiment | Result |
|---|---|
| **A.** `ast-grep-core = 0.45.3` + `tree-sitter = 0.27` + `tree-sitter-javascript = 0.25` in one crate | **Builds and tests pass.** `cargo tree -i tree-sitter` shows a **single** `tree-sitter v0.27.0` shared by ast-grep-core and the crate. |
| **B.** Force `tree-sitter` 0.27 and 0.24.7 into one graph (`package =` aliases) | **Cargo refuses to resolve:** both crates set `links = "tree-sitter"`. Error: only one package may use that links value. |
| **C.** `ast-grep-core = 0.45.2` (depends on `tree-sitter ^0.26.3`) + `tree-sitter = 0.27` | **Same hard conflict** (0.26.x vs 0.27 both `links = "tree-sitter"`). |

**Conclusion:** Current ast-grep-core **aligns** with our 0.27 line, so coexistence is
fine *today*. Diverging again (as 0.45.2 did one patch earlier) is not a soft dual-link:
Cargo **cannot** ship two tree-sitter C libraries in one binary. Pinning policy must keep
ast-grep-core on the same `tree-sitter` major/minor line as the workspace, or the
dependency is unusable.

MSRV note: ast-grep-core declares `rust-version = "1.88.0"`; this workspace is already
on `rust-version = "1.95"` / edition 2024, so that is not a blocker here.

---

## 2. API stability

Evidence: [ast-grep CHANGELOG](https://github.com/ast-grep/ast-grep/blob/main/CHANGELOG.md)
and crates.io versions for `ast-grep-core`.

- **Last 12 months:** 22 published versions from `0.39.6` (2025-10) through `0.45.3`
  (2026-08); **six** minor lines (`0.39` … `0.45`).
- Releases are frequent; many entries are dependency bumps (including tree-sitter
  0.25 → 0.26 → 0.27). Public Rust API surface moves with those minors (Language /
  LanguageExt / Pattern builder have evolved; this spike coded against 0.45.3 only).
- **API we need vs what 0.45.3 exposes:**

| Need ([`PATTERNS.md`](../PATTERNS.md)) | Support in ast-grep-core |
|---|---|
| Pattern parse from source fragment | `Pattern::try_new` / `PatternBuilder` |
| `$NAME` capture | Yes (`MetaVarEnv`) |
| `$$$NAME` multi-node | Yes |
| `$_` / `$$$` unnamed | Yes (ast-grep metavariable vocabulary) |
| Relational `inside` / `has` / `not` / `all` / `any` | Yes via `ops::Op` and matchers |
| `kind` constraint | Kind matchers |
| Regex on captures | `RegexMatcher` (Rust `regex` crate — **not** a guaranteed linear-time engine) |
| Explain / dump parsed pattern | `Pattern::dump` → `DumpPattern` (usable for `ast_explain_pattern`) |
| Rewrite templates | `replacer` module (out of M3 search scope but relevant later) |

---

## 3. DoS control (T-09)

**Finding: ast-grep-core has no public match-step / node-visit / wall-clock budget on
the match algorithm itself.** Grep of `0.45.3` sources shows no step counter in
`match_tree`. Comments still mention obsolete `Parser::set_timeout_micros` (removed in
tree-sitter 0.27); parse cancellation is not wired the way our lang crate uses
`progress_callback`.

What we *can* do from outside:

- Stop consuming a `find_all` iterator early (caps returned matches / outer walk time
  only after work already done per candidate).
- Own a `Visitor` loop and count nodes ourselves (still does not cap **backtracking
  inside** one `$$$` match).

### Measured timings (debug profile, Linux x86_64, JavaScript grammar)

Scratch crate using LanguageExt over `tree-sitter-javascript`:

| Case | Parse | Match | Notes |
|---|---|---|---|
| 800-deep nested `[…[0]…]`, pattern `[$$$INNER]` | 3 ms | 4 ms | 800 hits |
| 20 000× `console.log(i);`, pattern `console.log($$$ARGS)` | 363 ms | 275 ms | 20 000 hits |
| Pattern with 40 metavars `f($A0,…,$A39)` | — | 0 ms compile | Succeeds |

Custom prototype (appendix), same 20 000-call file, step budget 50 000 000:
**718 ms** end-to-end, 20 000 hits; with `steps: 50` the walk **stops early** (`ok=false`).

**Stack risk:** ast-grep matching is largely non-recursive in `match_tree` helpers named
`*_non_recursive`, which is good; deep source trees are still a cost in parse + walk.
Pathological `$$$` combinations remain the open DoS question without an inner step
budget.

---

## 4. Diagnostics

`PatternError` variants (thiserror) include the pattern source string and guidance, e.g.:

- `No AST root is detected. Please check the pattern source …`
- `Multiple AST nodes are detected. Please check the pattern source …`
- `Standalone multi meta variable … is invalid. Use $VAR or wrap …`

Observed in the scratch crate:

| Candidate | Result |
|---|---|
| `""` | Error (NoContent) |
| `"a; b;"` | Error (MultipleNode) |
| `"$$$ARGS"` | Error (RootMultiMetaVar) with suggestion |
| `"function ("` | **Accepted** (Ok) — tree-sitter recovers; not treated as invalid |
| `"{ let x = "` | **Accepted** (Ok) |

**Gaps vs our `[invalid_pattern]` bar:** messages name the whole source, not a byte
offset; some broken fragments still parse via recovery and will not fail closed.
`Pattern::dump` is enough to implement a first `ast_explain_pattern`, but we would
still wrap errors to add positions / next-step text.

---

## 5. Determinism

Same source + pattern twice in one process: hit lists `(start, end, text)` were
**byte-identical and same order** (pre-order / document order from `find_all`).

Compatible with our required sort (path, then start, then end) if the tool layer
re-sorts after collecting; do not assume ast-grep's order equals that sort without a
stable post-pass. Nothing time-dependent observed in match payloads.

---

## 6. PATTERNS.md checklist

| Requirement | ast-grep-core | Custom (est.) |
|---|---|---|
| Structural match, ignore trivia between nodes | Native | Native (named-child compare) |
| `$NAME` / `$$$NAME` / `$_` / `$$$` / `$$` | Native | Prototype: `$` / `$$$` only |
| Same metavar ⇒ structurally identical | Native | Prototype: text-equal rebinding only |
| Nested matches all reported | Native | Native (visit every node) |
| One-root pattern or `[invalid_pattern]` | Partial (see §4) | Must implement strictly |
| Rules: kind, inside, has, not, all/any, where.regex | Native ops; regex via `regex` crate | Build; use a linear-time regex crate for `where` |
| Budgets: visits, steps, wall clock | **Not native** | Prototype has steps; wall clock easy |
| Deterministic ordering | Document-order iterator | Sort explicitly |
| Rewrite / precedence paren / comment loss | Replacer exists; not audited here | Separate M3/M4 work |

---

## 7. Custom matcher estimate

| Item | Estimate |
|---|---|
| Core structural match + metavars + `$$$` + rebinding | ~800–1 200 LOC |
| Rule combinators (inside/has/not/all/any/kind) | ~400–600 LOC |
| Pattern preprocess per language (ast-grep's `pre_process_pattern`) | ~100–200 LOC / language |
| Budgets, errors with offsets, explain dump | ~300–500 LOC |
| Tests (golden + adversarial + differential) | dominates schedule |

**Risks if custom:** language-specific pattern preprocessing quirks; getting `$$$`
laziness and identical-structure (not just text) rebinding right; falling behind
ast-grep on edge cases until differential tests (PAT-05) exist.

**Prototype:** ~173 lines of matcher (appendix) already finds `console.log($$$ARGS)`
on 20 000 calls with a hard step budget. Feasibility is not in doubt; productisation
cost is.

---

## 8. Recommendation

**Prefer a purpose-built matcher** behind an internal trait, implemented on tree-sitter
0.27 we already own.

**Confidence:** medium-high.

### Why not adopt ast-grep-core as the engine of record

1. **Criterion 2 (budget control) fails as shipped.** ADR-005 says a matcher we cannot
   bound is rejected. Outer iterator caps are not enough for `$$$` backtracking DoS
   (T-09).
2. **tree-sitter pin fragility.** Coexistence works on 0.45.3; one prior patch line is
   already irreconcilable with 0.27 via Cargo `links`. Staying on ast-grep means their
   tree-sitter bump cadence becomes our release constraint.
3. **Diagnostics and regex** need wrapping anyway (positions; linear-time `where`).

### If Claude still chooses to reuse ast-grep-core

- Pin `=0.45.3` (or later still on tree-sitter `^0.27`), verify `cargo tree -i tree-sitter`
  stays single-version in CI.
- Wrap matching in our visitor with visit/step/wall budgets; treat inner match as
  trusted only inside those outer caps (document residual risk) **or** fork to add an
  inner counter.
- Hide behind the trait ADR-005 already requires so exit cost stays a rewrite of one
  adapter (~days), not the tool surface.

### Exit cost / max risk summary

| Path | Exit cost | Largest risk |
|---|---|---|
| Adopt ast-grep-core now | Adapter rewrite later; pin fights | Unbounded inner match; forced tree-sitter upgrades |
| Custom now | Higher M3 schedule | Missing subtle language preprocess cases until goldens catch them |

---

## Appendix A — Custom prototype (throwaway)

Condensed from the scratch crate (tests omitted). Demonstrates `$` / `$$$`, rebinding,
iterative walk, and a step budget.

```rust
//! Throwaway structural matcher: `$X` / `$$$XS`, rebinding, iterative walk + step budget.
//! Spike appendix only — not production code.
use std::collections::HashMap;
use tree_sitter::{Node, Parser, Tree};

pub struct Budget { pub steps: u64 }
impl Budget {
    fn tick(&mut self) -> bool {
        if self.steps == 0 { return false; }
        self.steps -= 1;
        true
    }
}

fn parse(src: &str) -> Tree {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_javascript::LANGUAGE.into()).unwrap();
    p.parse(src, None).unwrap()
}

#[derive(Clone, Debug)]
enum Pat {
    Lit { kind: String, kids: Vec<Pat> },
    Meta(String),
    Multi(String),
}

fn meta_name(s: &str) -> Option<&str> {
    let rest = s.strip_prefix('$')?;
    if rest.starts_with('$') { return None; }
    if rest.is_empty()
        || !rest
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    Some(rest)
}
fn multi_name(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("$$$")?;
    if rest.is_empty()
        || !rest
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    Some(rest)
}

fn node_to_pat(n: Node, src: &str) -> Pat {
    let text = &src[n.start_byte()..n.end_byte()];
    if n.named_child_count() == 0 && n.is_named() {
        if let Some(m) = multi_name(text) {
            return Pat::Multi(m.to_string());
        }
        if let Some(m) = meta_name(text) {
            return Pat::Meta(m.to_string());
        }
        return Pat::Lit {
            kind: format!("{}#{text}", n.kind()),
            kids: vec![],
        };
    }
    let mut kids = Vec::new();
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        kids.push(node_to_pat(ch, src));
    }
    if kids.is_empty() {
        return Pat::Lit {
            kind: format!("{}#{text}", n.kind()),
            kids: vec![],
        };
    }
    Pat::Lit {
        kind: n.kind().to_string(),
        kids,
    }
}

pub fn compile_pattern(pat_src: &str) -> Pat {
    let tree = parse(pat_src);
    let root = tree.root_node();
    let mut c = root.walk();
    let first = root.named_children(&mut c).next().unwrap_or(root);
    let inner = if first.kind() == "expression_statement" {
        first.named_child(0).unwrap_or(first)
    } else {
        first
    };
    node_to_pat(inner, pat_src)
}

fn named_children<'a>(n: Node<'a>) -> Vec<Node<'a>> {
    let mut out = Vec::new();
    let mut c = n.walk();
    for ch in n.named_children(&mut c) {
        out.push(ch);
    }
    out
}

fn match_pat<'a>(
    pat: &Pat,
    node: Node<'a>,
    src: &str,
    env: &mut HashMap<String, String>,
    budget: &mut Budget,
) -> bool {
    if !budget.tick() {
        return false;
    }
    match pat {
        Pat::Meta(name) => {
            if !node.is_named() {
                return false;
            }
            let t = src[node.start_byte()..node.end_byte()].to_string();
            if let Some(prev) = env.get(name) {
                return prev == &t;
            }
            env.insert(name.clone(), t);
            true
        }
        Pat::Multi(_) => false,
        Pat::Lit { kind, kids } => {
            if kids.is_empty() {
                if let Some((k, t)) = kind.split_once('#') {
                    return node.kind() == k && &src[node.start_byte()..node.end_byte()] == t;
                }
                return node.kind() == kind.as_str();
            }
            if node.kind() != kind.as_str() {
                return false;
            }
            match_seq(kids, &named_children(node), src, env, budget)
        }
    }
}

fn match_seq<'a>(
    pats: &[Pat],
    nodes: &[Node<'a>],
    src: &str,
    env: &mut HashMap<String, String>,
    budget: &mut Budget,
) -> bool {
    fn rec<'a>(
        pi: usize,
        ni: usize,
        pats: &[Pat],
        nodes: &[Node<'a>],
        src: &str,
        env: &mut HashMap<String, String>,
        budget: &mut Budget,
    ) -> bool {
        if !budget.tick() {
            return false;
        }
        if pi == pats.len() {
            return ni == nodes.len();
        }
        match &pats[pi] {
            Pat::Multi(name) => {
                for take in 0..=(nodes.len().saturating_sub(ni)) {
                    let text = nodes[ni..ni + take]
                        .iter()
                        .map(|n| &src[n.start_byte()..n.end_byte()])
                        .collect::<Vec<_>>()
                        .join(",");
                    let saved = env.clone();
                    if let Some(prev) = env.get(name) {
                        if prev != &text {
                            continue;
                        }
                    } else {
                        env.insert(name.clone(), text);
                    }
                    if rec(pi + 1, ni + take, pats, nodes, src, env, budget) {
                        return true;
                    }
                    *env = saved;
                }
                false
            }
            other => {
                if ni >= nodes.len() {
                    return false;
                }
                let saved = env.clone();
                if match_pat(other, nodes[ni], src, env, budget)
                    && rec(pi + 1, ni + 1, pats, nodes, src, env, budget)
                {
                    return true;
                }
                *env = saved;
                false
            }
        }
    }
    rec(0, 0, pats, nodes, src, env, budget)
}

pub fn find_all(src: &str, pat: &Pat, mut budget: Budget) -> (Vec<(usize, usize)>, bool) {
    let tree = parse(src);
    let mut hits = Vec::new();
    let mut stack = vec![tree.root_node()];
    let mut ok = true;
    while let Some(n) = stack.pop() {
        if !budget.tick() {
            ok = false;
            break;
        }
        let mut env = HashMap::new();
        if match_pat(pat, n, src, &mut env, &mut budget) {
            hits.push((n.start_byte(), n.end_byte()));
        }
        let mut c = n.walk();
        let ch: Vec<_> = n.children(&mut c).collect();
        for x in ch.into_iter().rev() {
            stack.push(x);
        }
    }
    hits.sort_unstable();
    (hits, ok)
}
```

## Appendix B — How to reproduce (not committed)

Scratch layout used on the builder (deleted at leisure):

- coexist crate: ast-grep-core 0.45.3 + tree-sitter 0.27
- dual_conflict / old_asg crates: prove `links = "tree-sitter"` hard-fail
- custom_proto crate: appendix algorithm + timings

Commands of record: `cargo test`, `cargo tree -i tree-sitter`, `cargo deny check licenses`
with a copy of this repository's `deny.toml`.
