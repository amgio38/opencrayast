# Third-party licenses

<!-- Regenerated; do not hand-edit the inventory tables. -->

## How this file was generated

Run from the repository root (requires a lockfile):

```sh
cargo metadata --format-version 1 --locked > /tmp/opencrayast-metadata.json
cargo tree -e normal --workspace   # cross-check the normal dependency set
# then rebuild this file from packages reached by normal (non-dev, non-build)
# edges from workspace members in that metadata JSON.
```

The inventory below is the set of **crates.io packages** reachable from
workspace members via normal dependency edges only (dev-dependencies such
as `tempfile` are omitted). Workspace members themselves are MIT and are
covered by the top-level `LICENSE`, not listed here.

## Redistribution obligations

When you distribute an opencrayast binary (or other compiled form) that
statically or dynamically includes these crates, you must also ship the
license texts that the elected licenses require:

- **MIT**: keep the copyright notice and permission notice in all copies or
  substantial portions. In practice, ship a copy of the MIT license text
  and preserve each crate attribution (name, version, repository).
- **Apache-2.0** (including `Apache-2.0 WITH LLVM-exception`): section 4
  requires a copy of the Apache-2.0 license with any redistribution of the
  Work or Derivative Works, plus a readable NOTICE file if the upstream
  ships one. Prefer shipping the full Apache-2.0 text alongside the binary.
- **Dual / triple SPDX expressions** (`MIT OR Apache-2.0`,
  `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT`): this project
  elects **MIT** wherever MIT is offered, so the MIT redistribution
  condition applies. Shipping both MIT and Apache-2.0 texts remains a
  safe choice if a redistributor prefers not to elect.

This file is the accounting side of that obligation; `deny.toml` is the
policy gate (`cargo deny check`).

## Summary by SPDX expression

| SPDX expression | Crates |
| --- | ---: |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 |
| `Apache-2.0 OR MIT` | 2 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 2 |
| `MIT` | 10 |
| `MIT OR Apache-2.0` | 31 |
| `Unlicense OR MIT` | 2 |

Total third-party normal dependencies: **48**.

## Crate inventory

| Crate | Version | License (SPDX) | Repository |
| --- | --- | --- | --- |
| `aho-corasick` | 1.1.5 | `Unlicense OR MIT` | https://github.com/BurntSushi/aho-corasick |
| `anstyle` | 1.0.14 | `MIT OR Apache-2.0` | https://github.com/rust-cli/anstyle.git |
| `bitflags` | 2.13.2 | `MIT OR Apache-2.0` | https://github.com/bitflags/bitflags |
| `block-buffer` | 0.10.4 | `MIT OR Apache-2.0` | https://github.com/RustCrypto/utils |
| `cfg-if` | 1.0.5 | `MIT OR Apache-2.0` | https://github.com/rust-lang/cfg-if |
| `clap` | 4.6.7 | `MIT OR Apache-2.0` | https://github.com/clap-rs/clap |
| `clap_builder` | 4.6.7 | `MIT OR Apache-2.0` | https://github.com/clap-rs/clap |
| `clap_derive` | 4.6.7 | `MIT OR Apache-2.0` | https://github.com/clap-rs/clap |
| `clap_lex` | 1.1.1 | `MIT OR Apache-2.0` | https://github.com/clap-rs/clap |
| `cpufeatures` | 0.2.17 | `MIT OR Apache-2.0` | https://github.com/RustCrypto/utils |
| `crypto-common` | 0.1.7 | `MIT OR Apache-2.0` | https://github.com/RustCrypto/traits |
| `data-encoding` | 2.11.1 | `MIT` | https://github.com/ia0/data-encoding |
| `digest` | 0.10.7 | `MIT OR Apache-2.0` | https://github.com/RustCrypto/traits |
| `equivalent` | 1.0.2 | `Apache-2.0 OR MIT` | https://github.com/indexmap-rs/equivalent |
| `errno` | 0.3.14 | `MIT OR Apache-2.0` | https://github.com/lambda-fairy/rust-errno |
| `generic-array` | 0.14.7 | `MIT` | https://github.com/fizyk20/generic-array.git |
| `hashbrown` | 0.17.1 | `MIT OR Apache-2.0` | https://github.com/rust-lang/hashbrown |
| `heck` | 0.5.0 | `MIT OR Apache-2.0` | https://github.com/withoutboats/heck |
| `indexmap` | 2.14.2 | `Apache-2.0 OR MIT` | https://github.com/indexmap-rs/indexmap |
| `itoa` | 1.0.18 | `MIT OR Apache-2.0` | https://github.com/dtolnay/itoa |
| `libc` | 0.2.189 | `MIT OR Apache-2.0` | https://github.com/rust-lang/libc |
| `linux-raw-sys` | 0.12.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | https://github.com/sunfishcode/linux-raw-sys |
| `memchr` | 2.8.3 | `Unlicense OR MIT` | https://github.com/BurntSushi/memchr |
| `proc-macro2` | 1.0.107 | `MIT OR Apache-2.0` | https://github.com/dtolnay/proc-macro2 |
| `quote` | 1.0.47 | `MIT OR Apache-2.0` | https://github.com/dtolnay/quote |
| `regex` | 1.13.1 | `MIT OR Apache-2.0` | https://github.com/rust-lang/regex |
| `regex-automata` | 0.4.18 | `MIT OR Apache-2.0` | https://github.com/rust-lang/regex |
| `regex-syntax` | 0.8.11 | `MIT OR Apache-2.0` | https://github.com/rust-lang/regex |
| `rustix` | 1.1.5 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | https://github.com/bytecodealliance/rustix |
| `serde` | 1.0.229 | `MIT OR Apache-2.0` | https://github.com/serde-rs/serde |
| `serde_core` | 1.0.229 | `MIT OR Apache-2.0` | https://github.com/serde-rs/serde |
| `serde_derive` | 1.0.229 | `MIT OR Apache-2.0` | https://github.com/serde-rs/serde |
| `serde_json` | 1.0.151 | `MIT OR Apache-2.0` | https://github.com/serde-rs/json |
| `sha2` | 0.10.9 | `MIT OR Apache-2.0` | https://github.com/RustCrypto/hashes |
| `streaming-iterator` | 0.1.9 | `MIT OR Apache-2.0` | https://github.com/sfackler/streaming-iterator |
| `syn` | 3.0.6 | `MIT OR Apache-2.0` | https://github.com/dtolnay/syn |
| `tree-sitter` | 0.27.0 | `MIT` | https://github.com/tree-sitter/tree-sitter |
| `tree-sitter-go` | 0.25.0 | `MIT` | https://github.com/tree-sitter/tree-sitter-go |
| `tree-sitter-javascript` | 0.25.0 | `MIT` | https://github.com/tree-sitter/tree-sitter-javascript |
| `tree-sitter-language` | 0.1.8 | `MIT` | https://github.com/tree-sitter/tree-sitter |
| `tree-sitter-python` | 0.25.0 | `MIT` | https://github.com/tree-sitter/tree-sitter-python |
| `tree-sitter-rust` | 0.24.2 | `MIT` | https://github.com/tree-sitter/tree-sitter-rust |
| `tree-sitter-typescript` | 0.23.2 | `MIT` | https://github.com/tree-sitter/tree-sitter-typescript |
| `typenum` | 1.20.1 | `MIT OR Apache-2.0` | https://github.com/paholg/typenum |
| `unicode-ident` | 1.0.26 | `(MIT OR Apache-2.0) AND Unicode-3.0` | https://github.com/dtolnay/unicode-ident |
| `windows-link` | 0.2.1 | `MIT OR Apache-2.0` | https://github.com/microsoft/windows-rs |
| `windows-sys` | 0.61.2 | `MIT OR Apache-2.0` | https://github.com/microsoft/windows-rs |
| `zmij` | 1.0.23 | `MIT` | https://github.com/dtolnay/zmij |

## Grammar crates (C source compiled into the binary)

`cargo deny check` reads Rust crate metadata: SPDX licence, source registry and
advisories. It does not read C source, does not know which upstream commit a
grammar was generated from, and does not look at build scripts. This section
records that part, and `scripts/check-grammar-provenance.sh` holds it against
`Cargo.lock` so it cannot rot.

Regenerated from the extracted crate sources by `scripts/gen-third-party-licenses.py`;
do not hand-edit. Read the build-script column as an observation, not a verdict:
it reports the calls the upstream build script makes, so a future release that
starts shelling out or fetching is visible as a diff in this table.

| Crate | Version | Source | Licence | Upstream project | Upstream version | Published from | C shipped | C lines | External scanner | Build script | What the build script does |
| --- | --- | --- | --- | --- | --- | --- | ---: | ---: | --- | --- | --- |
| `tree-sitter` | 0.27.0 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter | 0.27.0 (Cargo.toml) | `6070dbfefd326bd735e5683eb128cc1b57dad0c0` | 43 | 16089 | - | `binding_rust/build.rs` | cc compile of src/lib.c; fs write/copy/remove; bindgen |
| `tree-sitter-go` | 0.25.0 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter-go | 0.25.0 (tree-sitter.json) | `1547678a9da59885853f5f5cc8a99cc203fa2e2c` | 1 | 61521 | - | `bindings/rust/build.rs` | cc compile of src/parser.c |
| `tree-sitter-javascript` | 0.25.0 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter-javascript | 0.25.0 (tree-sitter.json) | `44c892e0be055ac465d5eeddae6d3e194424e7de` | 2 | 94632 | src/scanner.c | `bindings/rust/build.rs` | cc compile of src/parser.c, src/scanner.c |
| `tree-sitter-language` | 0.1.8 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter | 0.1.8 (Cargo.toml) | `6070dbfefd326bd735e5683eb128cc1b57dad0c0` | 3 | 3 | - | `build.rs` | no C compilation |
| `tree-sitter-python` | 0.25.0 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter-python | 0.25.0 (tree-sitter.json) | `293fdc02038ee2bf0e2e206711b69c90ac0d413f` | 2 | 130179 | src/scanner.c | `bindings/rust/build.rs` | cc compile of src/parser.c, src/scanner.c |
| `tree-sitter-rust` | 0.24.2 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter-rust | 0.24.2 (tree-sitter.json) | `e2bee853694a1d3e0f6ef308fe3674542fec95d7` | 2 | 206309 | src/scanner.c | `bindings/rust/build.rs` | cc compile of src/parser.c, src/scanner.c |
| `tree-sitter-typescript` | 0.23.2 | crates.io | `MIT` | https://github.com/tree-sitter/tree-sitter-typescript | 0.23.2 (Cargo.toml) | `f975a621f4e7f532fe322e13c4f79495e0a7b2e7` | 4 | 565418 | tsx/src/scanner.c, typescript/src/scanner.c | `bindings/rust/build.rs` | cc compile of tsx/src/parser.c, tsx/src/scanner.c, typescript/src/parser.c, typescript/src/scanner.c |

## Test-only / oracle dependencies (not shipped)

These crates appear only as `[dev-dependencies]` (or as transitive deps of those).
They are **not** linked into released binaries and are omitted from the inventory
tables above. `cargo deny check` still covers them.

| Crate | Version | License (SPDX) | Why present |
| --- | --- | --- | --- |
| `ast-grep-core` | 0.45.3 | `MIT` | Differential oracle for pattern matching (PAT-05 / ADR-005). **Test use only** — never a runtime dependency of opencrayast. |
| `bit-set` | 0.11.x | `MIT OR Apache-2.0` | Transitive of `ast-grep-core` (dev). |
| `bit-vec` | 0.10.x | `MIT OR Apache-2.0` | Transitive of `bit-set` (dev). |
| `thiserror` | 2.x | `MIT OR Apache-2.0` | Transitive of `ast-grep-core` (dev). |

