# Benchmarks

Numbers that are supposed to be believed. This file holds what the read
tools actually cost and save, measured with the harness in this repository, on
real code that was already on the machine. It also holds what the numbers do
**not** mean, because a benchmark without its limits is a marketing number.

## The harness

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label NAME --note "what this corpus is" --max-files 400 --seed 20261002
```

`crates/tools/src/bench.rs` walks the corpus with the same `core::walk` and
reads every file with the same `source::load` the tools use, then measures
three things per file: the bytes the boundary read, the bytes `ast_outline`
returned for it, and the bytes `ast_get` returned for its depth<=2 symbols. It
then writes a Markdown report. `--seed` fixes which symbol the retrieval
scenario picks (a hash of the seed and the file's path), so the same seed
picks the same symbols on every machine.

The corpus is identified in the report by a hash over every measured file's
size, path and content, so two copies of the same corpus produce the same id
and a one-byte edit produces a different one. The corpus **path** is never
printed: a committed report must not name the machine it ran on, so `$CORPUS`
below stands for whatever directory you point it at.

Build with `--release`. A debug build parses each file several times per
measurement and takes roughly five times as long; 400 files is about three
minutes either way, 40 files about twenty seconds.

## What was measured, in one paragraph

Bytes of UTF-8 output, and `ceil(bytes / 4)` as a token **estimate**. The
bytes are the measurement; the tokens are a convenience for reading the scale
and are not a tokeniser. Two scenarios: *exploration* (read every file of a
directory, or ask for the directory's outline in one call) and *retrieval*
(fetch one symbol per file with `ast_get`, or read the whole file to find the
same symbol).

## Summary

| Corpus | Files measured | `outline/file` median | p90 | Outline bigger than the file | Retrieval: read the file | Retrieval: `ast_get` |
|---|---|---|---|---|---|---|
| Rust, crate sources | 397 | 25% | 94% | 40 of 397 (10.1%) | 6 579 523 B | 343 325 B |
| Python 3.12 stdlib | 281 | 14% | 67% | 6 of 281 (2.1%) | 5 626 634 B | 767 232 B |
| Go distribution | 387 | 18% | 61% | 12 of 387 (3.1%) | 4 285 049 B | 228 635 B |
| JavaScript/TypeScript, bundled npm packages | 399 | 84% (ts) / 24% (js) | 169% / 141% | 117 of 399 (29.3%) | 2 434 548 B | 167 611 B |
| This project on itself | 93 | 14% | 31% | 7 of 93 (7.5%) | 1 031 245 B | 64 845 B |

Reading the numbers:

- **Retrieval is where the tools win, and they win by 5x to 20x.** One
  `ast_get` per file costs 2% to 14% of reading those files whole. This is
  the case the tools exist for.
- **A file's outline is usually a fraction of the file** (median 14% to 25% on
  hand-written code), but the distribution has a long tail: the p90 is 61% to
  94%, and between 2% and 30% of files have an outline **bigger** than the
  file, worst case 7x.
- **The JavaScript/TypeScript row is the least representative number here.**
  That corpus is npm build output, not source: minified bundles have very long
  lines and few symbols, so an outline of them is mostly overhead. It is
  reported because it is what happens to be on the machine, and because it
  shows how much the answer depends on what the code is.

## Corpora available on this machine

Inventory first, so a missing language is a recorded fact rather than a
silently absent row:

| Language | Corpus | What it is |
|---|---|---|
| Rust | `$CARGO_REGISTRY_SRC` | crate sources unpacked by cargo (crates.io dependencies of this workspace and its build tools) |
| Python | `$PYTHON_STDLIB` | the Python 3.12 standard library as installed by the system |
| Go | `$GO_SRC` | the Go distribution's own source tree (standard library plus `cmd`) |
| JavaScript, TypeScript, TSX | `$NODE_MODULES` | the npm packages bundled with Node.js - build output, see above |
| all of the above | `$REPO` | this project's own source, measured on itself |

TypeScript and TSX grammars are exercised by the `$NODE_MODULES` corpus only;
no hand-written TypeScript was available on this machine, and no fixture in
this repository was used as a stand-in for real code.

## Reports

Each section below is one run of the harness, pasted as produced. The three
closing sections every report ends with - the limits, what the numbers are not,
and the method - are identical across runs, so they are printed once at the end
of this file instead of five times. Re-running the command reproduces them.

### Rust: crate sources

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$CARGO_REGISTRY_SRC" \
  --label cargo-registry-src --max-files 400 --seed 20261002
```


#### Token benchmark: reading tools

Corpus **cargo-registry-src**. Rust crate sources unpacked by cargo (the crates.io dependencies of this workspace and its build tools)

#### Corpus

| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\tpath\tcontent`) |
|---|---|---|---|
| 27348 | 397 | 6579523 | `b9d5c812d61fa3bec3a2075f8de74fd13935ec613965aadec0cf3497505a671e` |

Sampled: every 69th file in path order, because the cap was 397. The numbers below describe those files, not the whole corpus.

Reproduce:

```sh
cargo run -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label cargo-registry-src --seed 20261002 --max-files 400
```

`$CORPUS` is this corpus's directory. It is not in the report because a committed report must not name the machine it was measured on; the corpus id above identifies what was measured.

#### Per language

`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is better, above 100% means the outline was bigger than the file. The ratio columns cover files with content only: 0 empty file(s) have no meaningful ratio and are left out of them (they are still in the byte totals and in "Where the tools lose").

| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max | Outline bigger than file |
|---|---|---|---|---|---|---|---|---|
| rust | 395 | 6475689 | 1275911 | 25% | 94% | 0% | 510% | 39 of 395 |
| typescript | 1 | 11 | 63 | 572% | 572% | 572% | 572% | 1 of 1 |
| javascript | 1 | 103823 | 61985 | 59% | 59% | 59% | 59% | 0 of 1 |

#### Distribution of `outline/file`, all languages

| min | p10 | median | p90 | max | median file bytes |
|---|---|---|---|---|---|
| 0% | 7% | 25% | 97% | 572% | 4004 |

#### Scenarios

##### Exploration: understand a directory

Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call for the whole corpus.

| Variant | Bytes returned | Estimated tokens | Notes |
|---|---|---|---|
| default limits (200 results) | 65519 | 16380 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000 | 65174 | 16294 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000, output cap 8 MiB | 1772933 | 443234 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |

Baseline for the sampled files: 6579523 bytes (~1644881 tokens). A single `ast_outline .` covers the whole corpus, so the honest baseline for this row is the size of every file in the corpus, not just the sampled ones: 27348 files.

##### Retrieval: get one symbol per file

For each measured file one symbol was picked with seed 20261002 and fetched with `ast_get` (doc comment included). Baseline: reading the whole file to find the same symbol.

| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | Saved, sum | Saved, median |
|---|---|---|---|---|---|
| 345 | 6579523 | 343325 | 278 | 6236198 | 3105 |

Fetching one symbol per file cost 343325 bytes against 6579523 bytes for reading those same files whole: 6236198 bytes saved, at a median of 3105 bytes saved per file. This is the scenario the tools are built for, and it is the one where they win by a lot. The section below is where they do not.

##### What fetching EVERY top-level symbol costs

On top of that, up to 5 symbols per file were fetched to measure what a symbol costs rather than what a file costs: 1027912 bytes over 1421 calls, median 815 bytes per call. Every call re-reads and re-parses the file, which is what the tool does.

230 `ast_get` calls could not address a single symbol and were not guessed at (the language allows the same qualified name twice). They are counted here and excluded from the medians.

#### Where the tools lose

40 of 397 measured files (10.1%) have an outline BIGGER than the file itself. For those files, reading the file is the cheaper way to get the same information.

The ten worst, by wasted bytes:

| File | File bytes | Outline bytes | Wasted | Symbols |
|---|---|---|---|---|
| `libc-0.2.182/src/new/newlib/unistd.rs` | 6579 | 11128 | 4549 | 150 |
| `libc-0.2.189/src/new/qurt/unistd.rs` | 9941 | 13869 | 3928 | 207 |
| `winapi-0.3.9/src/shared/cfg.rs` | 6806 | 9606 | 2800 | 113 |
| `windows-sys-0.61.2/src/Windows/Win32/Media/mod.rs` | 7343 | 8966 | 1623 | 132 |
| `linux-raw-sys-0.4.15/src/mips32r6/xdp.rs` | 5230 | 6311 | 1081 | 92 |
| `linux-raw-sys-0.12.1/src/powerpc/auxvec.rs` | 1456 | 2488 | 1032 | 42 |
| `linux-raw-sys-0.12.1/src/riscv64/auxvec.rs` | 1314 | 2240 | 926 | 38 |
| `windows-sys-0.59.0/src/Windows/Win32/Devices/Pwm/mod.rs` | 2715 | 3641 | 926 | 34 |
| `linux-raw-sys-0.4.15/src/powerpc64/loop_device.rs` | 4959 | 5812 | 853 | 84 |
| `serde_derive_internals-0.29.1/src/symbol.rs` | 2523 | 3351 | 828 | 46 |




### Python: the standard library

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$PYTHON_STDLIB" \
  --label python-stdlib --max-files 400 --seed 20261002
```


#### Token benchmark: reading tools

Corpus **python-stdlib**. the Python 3.12 standard library, as installed by the system package manager

#### Corpus

| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\tpath\tcontent`) |
|---|---|---|---|
| 561 | 281 | 5626634 | `5741af5e75d0d55143a85a995a9bf864784736d1c2aefb219e23ad2471017cab` |

Sampled: every 2nd file in path order, because the cap was 281. The numbers below describe those files, not the whole corpus.

Reproduce:

```sh
cargo run -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label python-stdlib --seed 20261002 --max-files 400
```

`$CORPUS` is this corpus's directory. It is not in the report because a committed report must not name the machine it was measured on; the corpus id above identifies what was measured.

#### Per language

`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is better, above 100% means the outline was bigger than the file. The ratio columns cover files with content only: 2 empty file(s) have no meaningful ratio and are left out of them (they are still in the byte totals and in "Where the tools lose").

| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max | Outline bigger than file |
|---|---|---|---|---|---|---|---|---|
| python | 281 | 5626634 | 685016 | 14% | 67% | 0% | 116% | 6 of 279 |

#### Distribution of `outline/file`, all languages

| min | p10 | median | p90 | max | median file bytes |
|---|---|---|---|---|---|
| 0% | 5% | 14% | 67% | 116% | 9644 |

#### Scenarios

##### Exploration: understand a directory

Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call for the whole corpus.

| Variant | Bytes returned | Estimated tokens | Notes |
|---|---|---|---|
| default limits (200 results) | 33508 | 8377 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |
| limit=1000 | 65524 | 16381 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |
| limit=1000, output cap 8 MiB | 84640 | 21160 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |

Baseline for the sampled files: 5626634 bytes (~1406659 tokens). A single `ast_outline .` covers the whole corpus, so the honest baseline for this row is the size of every file in the corpus, not just the sampled ones: 561 files.

##### Retrieval: get one symbol per file

For each measured file one symbol was picked with seed 20261002 and fetched with `ast_get` (doc comment included). Baseline: reading the whole file to find the same symbol.

| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | Saved, sum | Saved, median |
|---|---|---|---|---|---|
| 263 | 5626634 | 767232 | 225 | 4859402 | 9974 |

Fetching one symbol per file cost 767232 bytes against 5626634 bytes for reading those same files whole: 4859402 bytes saved, at a median of 9974 bytes saved per file. This is the scenario the tools are built for, and it is the one where they win by a lot. The section below is where they do not.

##### What fetching EVERY top-level symbol costs

On top of that, up to 5 symbols per file were fetched to measure what a symbol costs rather than what a file costs: 1325183 bytes over 1238 calls, median 2302 bytes per call. Every call re-reads and re-parses the file, which is what the tool does.

46 `ast_get` calls could not address a single symbol and were not guessed at (the language allows the same qualified name twice). They are counted here and excluded from the medians.

#### Where the tools lose

6 of 281 measured files (2.1%) have an outline BIGGER than the file itself. For those files, reading the file is the cheaper way to get the same information.

The ten worst, by wasted bytes:

| File | File bytes | Outline bytes | Wasted | Symbols |
|---|---|---|---|---|
| `ctypes/wintypes.py` | 5629 | 6145 | 516 | 121 |
| `token.py` | 2511 | 2831 | 320 | 76 |
| `xml/dom/__init__.py` | 4019 | 4291 | 272 | 75 |
| `email/mime/__init__.py` | 0 | 40 | 40 | 0 |
| `urllib/__init__.py` | 0 | 36 | 36 | 0 |
| `__phello__/__init__.py` | 97 | 113 | 16 | 2 |




### Go: the distribution's own source

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$GO_SRC" \
  --label go-distribution-src --max-files 400 --seed 20261002
```


#### Token benchmark: reading tools

Corpus **go-distribution-src**. the source tree of the Go distribution: standard library plus cmd

#### Corpus

| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\tpath\tcontent`) |
|---|---|---|---|
| 7347 | 387 | 4285049 | `14375b8a55de24a6a1d527577c353dcff49a87a8493a9d0b936b428beecc4c8f` |

Sampled: every 19th file in path order, because the cap was 387. The numbers below describe those files, not the whole corpus.

Reproduce:

```sh
cargo run -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label go-distribution-src --seed 20261002 --max-files 400
```

`$CORPUS` is this corpus's directory. It is not in the report because a committed report must not name the machine it was measured on; the corpus id above identifies what was measured.

#### Per language

`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is better, above 100% means the outline was bigger than the file. The ratio columns cover files with content only: 0 empty file(s) have no meaningful ratio and are left out of them (they are still in the byte totals and in "Where the tools lose").

| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max | Outline bigger than file |
|---|---|---|---|---|---|---|---|---|
| go | 387 | 4285049 | 599818 | 18% | 61% | 0% | 280% | 12 of 387 |

#### Distribution of `outline/file`, all languages

| min | p10 | median | p90 | max | median file bytes |
|---|---|---|---|---|---|
| 0% | 6% | 18% | 61% | 280% | 2567 |

#### Scenarios

##### Exploration: understand a directory

Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call for the whole corpus.

| Variant | Bytes returned | Estimated tokens | Notes |
|---|---|---|---|
| default limits (200 results) | 65532 | 16383 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000 | 65512 | 16378 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000, output cap 8 MiB | 430596 | 107649 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |

Baseline for the sampled files: 4285049 bytes (~1071263 tokens). A single `ast_outline .` covers the whole corpus, so the honest baseline for this row is the size of every file in the corpus, not just the sampled ones: 7347 files.

##### Retrieval: get one symbol per file

For each measured file one symbol was picked with seed 20261002 and fetched with `ast_get` (doc comment included). Baseline: reading the whole file to find the same symbol.

| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | Saved, sum | Saved, median |
|---|---|---|---|---|---|
| 368 | 4285049 | 228635 | 260 | 4056414 | 1965 |

Fetching one symbol per file cost 228635 bytes against 4285049 bytes for reading those same files whole: 4056414 bytes saved, at a median of 1965 bytes saved per file. This is the scenario the tools are built for, and it is the one where they win by a lot. The section below is where they do not.

##### What fetching EVERY top-level symbol costs

On top of that, up to 5 symbols per file were fetched to measure what a symbol costs rather than what a file costs: 799433 bytes over 1357 calls, median 417 bytes per call. Every call re-reads and re-parses the file, which is what the tool does.

13 `ast_get` calls could not address a single symbol and were not guessed at (the language allows the same qualified name twice). They are counted here and excluded from the medians.

#### Where the tools lose

12 of 387 measured files (3.1%) have an outline BIGGER than the file itself. For those files, reading the file is the cheaper way to get the same information.

The ten worst, by wasted bytes:

| File | File bytes | Outline bytes | Wasted | Symbols |
|---|---|---|---|---|
| `runtime/defs_openbsd_ppc64.go` | 3073 | 4460 | 1387 | 93 |
| `runtime/defs_linux_arm.go` | 3981 | 4515 | 534 | 93 |
| `internal/goarch/zgoarch_arm64be.go` | 582 | 1066 | 484 | 25 |
| `internal/buildcfg/zbootstrap.go` | 586 | 1001 | 415 | 16 |
| `internal/goos/zgoos_ios.go` | 449 | 794 | 345 | 19 |
| `syscall/types_darwin.go` | 5154 | 5489 | 335 | 87 |
| `cmd/api/testdata/src/issue64958/p/p.go` | 35 | 98 | 63 | 1 |
| `cmd/cover/testdata/pkgcfg/noFuncsNoTests/nfnt.go` | 71 | 126 | 55 | 2 |
| `cmd/cgo/internal/testshared/testdata/execgo/exe.go` | 49 | 92 | 43 | 1 |
| `go/internal/gccgoimporter/testdata/notinheap.go` | 50 | 90 | 40 | 1 |




### JavaScript and TypeScript: bundled npm packages

**Read this one with the warning above.** These are build outputs, not source
files: the corpus contains minified bundles whose lines are megabytes long and
whose symbols are few. A high "outline bigger than the file" rate here says
something about the corpus, not about the tools on ordinary code.

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$NODE_MODULES" \
  --label node-bundled-packages --max-files 400 --seed 20261002
```


#### Token benchmark: reading tools

Corpus **node-bundled-packages**. the npm packages bundled with Node.js

#### Corpus

| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\tpath\tcontent`) |
|---|---|---|---|
| 52541 | 399 | 2434548 | `59c9cfaaa0c911c15050e011fcd3096aacc3ae986439b9f4b4077d7f666be7c3` |

Sampled: every 132nd file in path order, because the cap was 399. The numbers below describe those files, not the whole corpus.

Reproduce:

```sh
cargo run -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label node-bundled-packages --seed 20261002 --max-files 400
```

`$CORPUS` is this corpus's directory. It is not in the report because a committed report must not name the machine it was measured on; the corpus id above identifies what was measured.

#### Per language

`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is better, above 100% means the outline was bigger than the file. The ratio columns cover files with content only: 5 empty file(s) have no meaningful ratio and are left out of them (they are still in the byte totals and in "Where the tools lose").

| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max | Outline bigger than file |
|---|---|---|---|---|---|---|---|---|
| typescript | 167 | 747045 | 269226 | 84% | 169% | 0% | 450% | 64 of 167 |
| javascript | 192 | 1617944 | 164661 | 24% | 141% | 0% | 709% | 26 of 190 |
| python | 40 | 69559 | 65359 | 128% | 173% | 23% | 383% | 27 of 37 |

#### Distribution of `outline/file`, all languages

| min | p10 | median | p90 | max | median file bytes |
|---|---|---|---|---|---|
| 0% | 8% | 55% | 169% | 709% | 856 |

#### Scenarios

##### Exploration: understand a directory

Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call for the whole corpus.

| Variant | Bytes returned | Estimated tokens | Notes |
|---|---|---|---|
| default limits (200 results) | 65481 | 16371 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000 | 65118 | 16280 | TRUNCATED by the output cap or the symbol limit; the walk itself stopped at max_scan_files; some files were skipped (see the skipped line in the output) |
| limit=1000, output cap 8 MiB | 5303309 | 1325828 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |

Baseline for the sampled files: 2434548 bytes (~608637 tokens). A single `ast_outline .` covers the whole corpus, so the honest baseline for this row is the size of every file in the corpus, not just the sampled ones: 52541 files.

##### Retrieval: get one symbol per file

For each measured file one symbol was picked with seed 20261002 and fetched with `ast_get` (doc comment included). Baseline: reading the whole file to find the same symbol.

| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | Saved, sum | Saved, median |
|---|---|---|---|---|---|
| 280 | 2434548 | 167611 | 272 | 2266937 | 746 |

Fetching one symbol per file cost 167611 bytes against 2434548 bytes for reading those same files whole: 2266937 bytes saved, at a median of 746 bytes saved per file. This is the scenario the tools are built for, and it is the one where they win by a lot. The section below is where they do not.

##### What fetching EVERY top-level symbol costs

On top of that, up to 5 symbols per file were fetched to measure what a symbol costs rather than what a file costs: 770108 bytes over 953 calls, median 539 bytes per call. Every call re-reads and re-parses the file, which is what the tool does.

39 `ast_get` calls could not address a single symbol and were not guessed at (the language allows the same qualified name twice). They are counted here and excluded from the medians.

#### Where the tools lose

117 of 399 measured files (29.3%) have an outline BIGGER than the file itself. For those files, reading the file is the cheaper way to get the same information.

The ten worst, by wasted bytes:

| File | File bytes | Outline bytes | Wasted | Symbols |
|---|---|---|---|---|
| `pyright/dist/typeshed-fallback/stubs/uWSGI/uwsgidecorators.pyi` | 6263 | 8298 | 2035 | 125 |
| `pyright/dist/typeshed-fallback/stubs/yt-dlp/yt_dlp/socks.pyi` | 1835 | 2853 | 1018 | 48 |
| `pyright/dist/typeshed-fallback/stubs/django-import-export/import_export/widgets.pyi` | 3511 | 4497 | 986 | 57 |
| `@agentclientprotocol/claude-agent-acp/node_modules/zod/v4/core/parse.d.ts` | 3829 | 4628 | 799 | 37 |
| `pyright/dist/typeshed-fallback/stubs/pywin32/win32comext/axdebug/expressions.pyi` | 1984 | 2755 | 771 | 40 |
| `pyright/dist/typeshed-fallback/stubs/gunicorn/gunicorn/http2/errors.pyi` | 2091 | 2701 | 610 | 40 |
| `openclaw/dist/plugin-sdk/extensions/matrix/src/matrix/actions/verification.d.ts` | 4376 | 4922 | 546 | 18 |
| `pyright/dist/typeshed-fallback/stubs/networkx/networkx/algorithms/planarity.pyi` | 4230 | 4772 | 542 | 66 |
| `pyright/dist/typeshed-fallback/stubs/reportlab/reportlab/graphics/barcode/ecc200datamatrix.pyi` | 618 | 1071 | 453 | 19 |
| `pyright/dist/typeshed-fallback/stubs/pyasn1/pyasn1/codec/cer/decoder.pyi` | 1247 | 1630 | 383 | 19 |




### This project on itself

```sh
cargo run --release -p opencrayast-tools --example token_bench -- "$REPO" \
  --label opencrayast-own-source --max-files 400 --seed 20261002
```


#### Token benchmark: reading tools

Corpus **opencrayast-own-source**. this project's own source tree, measured on itself

#### Corpus

| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\tpath\tcontent`) |
|---|---|---|---|
| 93 | 93 | 1031245 | `d7af3dca1f017315a59bba256bcec7e1d594fc643f1232638cdb367f70302c65` |

Reproduce:

```sh
cargo run -p opencrayast-tools --example token_bench -- "$CORPUS" \
  --label opencrayast-own-source --seed 20261002 --max-files 400
```

`$CORPUS` is this corpus's directory. It is not in the report because a committed report must not name the machine it was measured on; the corpus id above identifies what was measured.

#### Per language

`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is better, above 100% means the outline was bigger than the file. The ratio columns cover files with content only: 0 empty file(s) have no meaningful ratio and are left out of them (they are still in the byte totals and in "Where the tools lose").

| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max | Outline bigger than file |
|---|---|---|---|---|---|---|---|---|
| rust | 89 | 1030159 | 132907 | 14% | 31% | 4% | 151% | 3 of 89 |
| typescript | 1 | 414 | 541 | 130% | 130% | 130% | 130% | 1 of 1 |
| javascript | 1 | 187 | 278 | 148% | 148% | 148% | 148% | 1 of 1 |
| python | 1 | 259 | 265 | 102% | 102% | 102% | 102% | 1 of 1 |
| go | 1 | 226 | 297 | 131% | 131% | 131% | 131% | 1 of 1 |

#### Distribution of `outline/file`, all languages

| min | p10 | median | p90 | max | median file bytes |
|---|---|---|---|---|---|
| 4% | 8% | 15% | 43% | 151% | 8490 |

#### Scenarios

##### Exploration: understand a directory

Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call for the whole corpus.

| Variant | Bytes returned | Estimated tokens | Notes |
|---|---|---|---|
| default limits (200 results) | 20204 | 5051 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |
| limit=1000 | 65510 | 16378 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |
| limit=1000, output cap 8 MiB | 94691 | 23673 | TRUNCATED by the output cap or the symbol limit; some files were skipped (see the skipped line in the output) |

Baseline for the sampled files: 1031245 bytes (~257812 tokens). A single `ast_outline .` covers the whole corpus, so the honest baseline for this row is the size of every file in the corpus, not just the sampled ones: 93 files.

##### Retrieval: get one symbol per file

For each measured file one symbol was picked with seed 20261002 and fetched with `ast_get` (doc comment included). Baseline: reading the whole file to find the same symbol.

| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | Saved, sum | Saved, median |
|---|---|---|---|---|---|
| 85 | 1031245 | 64845 | 460 | 966400 | 8008 |

Fetching one symbol per file cost 64845 bytes against 1031245 bytes for reading those same files whole: 966400 bytes saved, at a median of 8008 bytes saved per file. This is the scenario the tools are built for, and it is the one where they win by a lot. The section below is where they do not.

##### What fetching EVERY top-level symbol costs

On top of that, up to 5 symbols per file were fetched to measure what a symbol costs rather than what a file costs: 252339 bytes over 417 calls, median 395 bytes per call. Every call re-reads and re-parses the file, which is what the tool does.

33 `ast_get` calls could not address a single symbol and were not guessed at (the language allows the same qualified name twice). They are counted here and excluded from the medians.

#### Where the tools lose

7 of 93 measured files (7.5%) have an outline BIGGER than the file itself. For those files, reading the file is the cheaper way to get the same information.

The ten worst, by wasted bytes:

| File | File bytes | Outline bytes | Wasted | Symbols |
|---|---|---|---|---|
| `crates/query/tests/fixtures/sample.ts` | 414 | 541 | 127 | 11 |
| `crates/query/tests/fixtures/sample.rs` | 416 | 508 | 92 | 11 |
| `crates/query/tests/fixtures/sample.js` | 187 | 278 | 91 | 6 |
| `crates/query/tests/fixtures/sample.go` | 226 | 297 | 71 | 6 |
| `crates/cli/src/main.rs` | 41 | 62 | 21 | 1 |
| `crates/mcp/src/main.rs` | 41 | 62 | 21 | 1 |
| `crates/query/tests/fixtures/sample.py` | 259 | 265 | 6 | 6 |




## What these numbers do not mean

- **Not a task success rate.** Nothing here ran an agent on a task. A tool that
  returns 20% of the bytes and leaves the agent unable to finish has not saved
  anything, and this benchmark cannot tell that case from a successful one.
- **Not latency, CPU or memory.** No clock was read and no memory was measured.
  `ast_get` re-reads and re-parses the file on every call; the bytes are what
  the tool returned.
- **Not comparable across corpora.** A number from a different corpus, a
  different `Limits` or a different sample size answers a different question.
  The corpus id is what makes two runs comparable.
- **Not a tokenizer measurement.** `ceil(bytes / 4)` is a rough stand-in. A real
  tokeniser charges more for code than for prose, so treat every token figure
  here as the rough number it is. The method in [`TESTING.md`](TESTING.md)
  asks for published tokenisers; that is not done yet, and until it is, these
  are byte counts with a convenient label.
- **Not a claim about any model's context window.**
- **Not a whole-directory answer.** The exploration scenario is the honest one
  to be careful with: on a large directory `ast_outline` returns a truncated
  outline plus an instruction to narrow `path`. A truncated outline is a
  refusal, not a cheaper answer, and every run above says which of its rows
  were truncated.
- **Not the worst case.** Sampling takes every n-th file in path order, which
  is reproducible and spreads across the corpus, but it is still a sample. The
  measured set is stated in every report.

## Known gaps

- Only one published tokeniser is not implemented; see above.
- Task success is not measured at all; the agent task suite is a later
  milestone.
- Per-language numbers exist for the languages whose grammars are built in;
  a corpus of another language is skipped and counted in the report rather
  than guessed at.
- `max_matches` (1000 by default) can stop a retrieval row before every
  measured file has been visited; the reports state the match counts they used.
