//! Measure what the read tools cost, and what they save.
//!
//! ```sh
//! cargo run -p opencrayast-tools --example token_bench -- "$CARGO_REGISTRY_SRC" \
//!   --label cargo-registry-src --note "crate sources unpacked by cargo" --max-files 1500
//! ```
//!
//! Prints a Markdown report on stdout, or writes it to `--out`. The corpus directory is never
//! printed: the report carries a `--label` instead, because a report gets committed and a
//! committed report must not name the machine it was measured on. Everything else in the output
//! is a function of the files themselves, so two runs on the same corpus produce the same
//! bytes.

use opencrayast_tools::bench::{BenchConfig, run};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut corpus: Option<String> = None;
    let mut cfg = BenchConfig::default();
    let mut out: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) => cfg.seed = v,
                None => return fail("--seed needs a number"),
            },
            "--max-files" => match args.next().and_then(|v| v.parse().ok()) {
                Some(v) => cfg.max_files = v,
                None => return fail("--max-files needs a number"),
            },
            "--label" => match args.next() {
                Some(v) => cfg.label = v,
                None => return fail("--label needs a name"),
            },
            "--note" => match args.next() {
                Some(v) => cfg.note = v,
                None => return fail("--note needs a sentence"),
            },
            "--out" => match args.next() {
                Some(v) => out = Some(v),
                None => return fail("--out needs a path"),
            },
            "--help" | "-h" => {
                println!(
                    "usage: token_bench <corpus-dir> [--label NAME] [--note TEXT] [--seed N] \
                     [--max-files N] [--out FILE]"
                );
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                return fail(&format!("unknown option {other}"));
            }
            other if corpus.is_none() => corpus = Some(other.to_string()),
            other => return fail(&format!("unexpected argument {other}")),
        }
    }

    let Some(corpus) = corpus else {
        return fail("a corpus directory is required");
    };
    cfg.corpus = std::path::PathBuf::from(corpus);

    match run(&cfg) {
        Ok(report) => match out {
            Some(path) => match std::fs::write(&path, report) {
                Ok(()) => {
                    println!("wrote {path}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&format!("could not write {path}: {e}")),
            },
            None => {
                print!("{report}");
                ExitCode::SUCCESS
            }
        },
        Err(e) => {
            eprintln!("token_bench failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("token_bench: {message}");
    ExitCode::FAILURE
}
