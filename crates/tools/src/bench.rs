//! Measuring what the read tools cost, and what they save (docs/BENCHMARKS.md,
//! docs/TESTING.md "Token-savings benchmark").
//!
//! This module is the measurement; `examples/token_bench.rs` is only its command line. It lives
//! in the library rather than in the example so that `tests/bench_spec.rs` can assert on a real
//! report instead of on a copy of the logic.
//!
//! # What is measured, and what is not
//!
//! Every number here is **bytes of UTF-8 output**, plus a token estimate of
//! `ceil(bytes / 4)`. That estimate is a crude stand-in, not a tokeniser: no published
//! tokeniser is used, because doing it properly needs a dependency this workspace does not
//! have, and a wrong precise-looking number is worse than an obviously rough one. The bytes are
//! the real measurement; the tokens are a convenience for reading the scale.
//!
//! The comparison is "bytes an agent sends to the model", not "work an agent does". Two
//! consequences are stated in the report itself and repeated here, because they are the easiest
//! thing to get wrong when reading a benchmark:
//!
//! - Reading a whole file is not a fair opponent for an outline when the file is tiny: an
//!   outline of a ten-line file is longer than the file. The report counts those files instead
//!   of hiding them.
//! - An outline that was cut off by `max_output_bytes` has not saved anything by being small;
//!   it has refused. The report says when that happened.

use crate::context::{Mode, ToolContext};
use crate::get::{GetArgs, ast_get};
use crate::outline::{OutlineArgs, ast_outline};
use opencrayast_core::boundary::{Boundary, BoundaryConfig};
use opencrayast_core::error::{ErrorCode, ToolError};
use opencrayast_core::hash::ContentHash;
use opencrayast_core::limits::Limits;
use opencrayast_core::walk::{WalkOptions, walk};
use opencrayast_lang::Language;
use opencrayast_query::{OutlineOptions, Symbol, outline as query_outline};

/// Bytes per estimated token. Deliberately a round number and deliberately labelled an estimate
/// everywhere it appears.
pub const BYTES_PER_TOKEN: u64 = 4;

/// Deepest symbol level the retrieval cost is measured for: an agent asking "what is in here"
/// looks at the top of the file, not at everything nested inside every closure.
const GET_MAX_DEPTH: usize = 2;

/// How the benchmark was asked to run.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Directory to measure. It becomes the workspace root, so every reported path is relative
    /// to it.
    pub corpus: std::path::PathBuf,
    /// Name for the corpus in the report. A LABEL, not a path: the report is committed, and a
    /// committed report must not contain the machine it was measured on.
    pub label: String,
    /// Seed for the retrieval scenario. Same seed, same symbols, every run.
    pub seed: u64,
    /// Most files to measure. A corpus bigger than this is sampled deterministically (every
    /// n-th file in path order), and the report says how many of how many were measured.
    pub max_files: usize,
    /// Most `ast_get` calls spent on ONE file when measuring what fetching a symbol costs.
    /// Every call re-reads and re-parses the file (that is what the tool does), so measuring
    /// every symbol of every file of a large corpus would take hours. The first N symbols in
    /// source order are measured and the report says how many of how many.
    pub max_gets_per_file: usize,
    /// A note about what this corpus is, printed verbatim in the report ("the Python standard
    /// library", "this project's own source").
    pub note: String,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            corpus: std::path::PathBuf::new(),
            label: "corpus".to_string(),
            seed: 0,
            max_files: 400,
            max_gets_per_file: 5,
            note: String::new(),
        }
    }
}

/// One file's measurements.
struct FileCosts {
    /// Path relative to the corpus root.
    rel: String,
    /// Language it was measured as.
    language: Language,
    /// Bytes of the file as read.
    bytes: u64,
    /// Hex sha256 of those bytes, for the corpus identity.
    content_hash: String,
    /// Bytes `ast_outline <file>` returned.
    outline_bytes: u64,
    /// Bytes of the `ast_get` outputs for this file's SAMPLED depth<=2 symbols (see
    /// [`BenchConfig::max_gets_per_file`]), summed.
    get_total_bytes: u64,
    /// Median of those outputs.
    get_median_bytes: u64,
    /// How many depth<=2 symbols the file has.
    symbols: usize,
    /// How many of them were actually measured.
    symbols_measured: usize,
    /// Bytes of the ONE symbol the retrieval scenario picked (seeded by path and seed).
    get_one_bytes: u64,
    /// How many of those could not be addressed by name alone (an overload the language allows
    /// and the tool therefore refuses to guess between).
    #[allow(
        dead_code,
        reason = "kept per file so a future report can break it down by file"
    )]
    get_unresolved: usize,
}

/// Run the benchmark and return the Markdown report.
pub fn run(cfg: &BenchConfig) -> Result<String, ToolError> {
    let ctx = context(cfg, Limits::default())?;
    let budget = Limits::default();

    // The file inventory. `max_files` for the walk is generous on purpose: the report needs to
    // know how many files the corpus HAS, and the sampling below decides which of them are
    // measured. A walk that stopped at the sampling cap could not tell those apart.
    let inventory = walk(
        &ctx.boundary,
        &ctx.boundary.resolve_read(".")?,
        &WalkOptions {
            max_files: 200_000,
            respect_gitignore: true,
            extra_ignore: vec![],
        },
    )?;
    let mut all: Vec<(String, Language)> = inventory
        .files
        .iter()
        .filter_map(|f| Language::detect(&f.rel, None).map(|l| (f.rel.clone(), l)))
        .collect();
    // Sort by path bytes so the sampling and the report are both independent of walk order.
    all.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let available = all.len();
    let picked = sample(&all, cfg.max_files);

    let mut costs: Vec<FileCosts> = Vec::with_capacity(picked.len());
    let mut get_unresolved_total = 0usize;
    // Files that could not be read or outlined at all. They are NOT in any table, so they
    // have to be counted here or the corpus table would quietly describe fewer files than the
    // corpus has.
    let mut unreadable = 0usize;
    let mut unoutlineable = 0usize;
    for (rel, language) in picked {
        let path = match ctx.boundary.resolve_read(&rel) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // The bytes of the file as the boundary read it: this is what "read the whole file"
        // costs an agent, and it is measured, not estimated from a stat.
        let Ok(loaded) = crate::source::load(&ctx, &path) else {
            unreadable += 1;
            continue;
        };
        let symbols = query_outline(
            &loaded.parsed,
            &loaded.text,
            &OutlineOptions {
                max_depth: GET_MAX_DEPTH,
                ..OutlineOptions::default()
            },
        );
        let outline_bytes = match ast_outline(
            &ctx,
            &OutlineArgs {
                path: rel.clone(),
                ..Default::default()
            },
        ) {
            Ok(text) => text.len() as u64,
            // A file the outline cannot handle has no outline cost to report; it is counted
            // rather than folded into a zero that would look like a free outline.
            Err(_) => {
                unoutlineable += 1;
                continue;
            }
        };

        // What fetching a symbol costs, measured on the first `max_gets_per_file` symbols in
        // source order (the cap is reported) ...
        let mut outputs: Vec<u64> = Vec::with_capacity(symbols.len().min(cfg.max_gets_per_file));
        let mut unresolved = 0usize;
        for symbol in symbols.iter().take(cfg.max_gets_per_file) {
            match fetch(&ctx, &rel, symbol) {
                Ok(bytes) => outputs.push(bytes),
                // Ambiguous means the language allows two symbols with this qualified name and
                // the tool refused to pick. That is the tool working; the call is counted, not
                // retried with a guess.
                Err(_) => unresolved += 1,
            }
        }
        get_unresolved_total += unresolved;

        // ... and the retrieval scenario, which is ONE symbol per file, chosen by a hash of
        // the seed and the path so the same seed picks the same symbol on every machine.
        let pick = symbols
            .get(pick_index(cfg.seed, &rel, symbols.len()))
            .and_then(|symbol| fetch(&ctx, &rel, symbol).ok());
        if symbols.len() > cfg.max_gets_per_file && pick.is_none() {
            get_unresolved_total += 1;
        }

        costs.push(FileCosts {
            rel,
            language,
            bytes: loaded.text.len() as u64,
            content_hash: ContentHash::of(loaded.text.as_bytes())
                .0
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            outline_bytes,
            get_total_bytes: outputs.iter().sum(),
            get_median_bytes: median(&mut outputs),
            symbols: symbols.len(),
            symbols_measured: outputs.len() + usize::from(unresolved > 0),
            get_one_bytes: pick.unwrap_or(0),
            get_unresolved: unresolved,
        });
    }

    // The exploration table needs a context whose output cap is high enough that the corpus is
    // not cut off, so the reader can see both: what the default limits return, and what the
    // outline actually costs when nothing truncates it.
    // `limit=1000` is only a legal argument when `max_results` allows it (the default is 200,
    // hard maximum 1000), so the "no truncation" variant raises both. Reporting the refusal
    // instead would have been an honest but useless table row.
    let many = context(
        cfg,
        Limits {
            max_results: 1000,
            ..Limits::default()
        },
    )?;
    let wide = context(
        cfg,
        Limits {
            max_output_bytes: 8 * 1024 * 1024,
            max_scan_files: 200_000,
            max_results: 1000,
            ..Limits::default()
        },
    )?;

    Ok(report(
        &ReportInputs {
            cfg,
            default_ctx: &ctx,
            many_ctx: &many,
            wide_ctx: &wide,
            budget: &budget,
            available,
            get_unresolved: get_unresolved_total,
            unreadable,
            unoutlineable,
        },
        &costs,
    ))
}

/// Fetch one symbol with `ast_get`, addressed by its qualified name and, if the language allows
/// the same qualified name twice, by its bare name.
///
/// The fallback is a narrowing, not a guess: `ast_get` refuses to choose between two symbols
/// and the bare name is the next most specific thing the caller has. When even that is
/// ambiguous the call is counted as unresolved and no bytes are recorded, so the retrieval
/// figure never quietly becomes "whatever answered".
fn fetch(ctx: &ToolContext, rel: &str, symbol: &Symbol) -> Result<u64, ToolError> {
    for query in [&symbol.qualified, &symbol.name] {
        match ast_get(
            ctx,
            &GetArgs {
                symbol: query.clone(),
                path: Some(rel.to_string()),
                ..Default::default()
            },
        ) {
            Ok(text) => return Ok(text.len() as u64),
            Err(e) if e.code == ErrorCode::Ambiguous => continue,
            Err(e) => return Err(e),
        }
    }
    Err(ToolError::new(
        ErrorCode::Ambiguous,
        "no query addressed this symbol uniquely",
        "The language allows the same name twice.",
    ))
}

/// The files of a group that have content, which are the only ones a byte ratio means anything
/// for.
fn non_empty<'a>(group: &[&'a FileCosts]) -> Vec<&'a FileCosts> {
    group.iter().copied().filter(|c| c.bytes > 0).collect()
}

/// Which symbol the retrieval scenario picks for this file: a hash of the seed and the path,
/// so the choice is reproducible without a random number generator and does not depend on the
/// order the files were walked in.
fn pick_index(seed: u64, rel: &str, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    // FNV-1a over the seed and the path: cheap, stable across platforms and Rust versions.
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for byte in rel.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % len as u64) as usize
}

/// A context whose workspace root is the corpus, so reported paths are corpus-relative.
fn context(cfg: &BenchConfig, limits: Limits) -> Result<ToolContext, ToolError> {
    Ok(ToolContext {
        config_source: Default::default(),
        // The boundary is held to the SAME limits the tools are given. It used to be built
        // with `Limits::default()` while `limits` went into the ToolContext beside it, so one
        // struct carried two different sets and the benchmark measured a boundary that was not
        // the one it thought it was measuring (SEC-FIX 5 CR).
        boundary: Boundary::new(BoundaryConfig::new(cfg.corpus.clone(), limits.clone()))?,
        limits,
        mode: Mode::ReadOnly,
        write: None,
        version: env!("CARGO_PKG_VERSION").to_string(),
        workspace_id: "bench".to_string(),
        respect_gitignore: true,
        extra_ignore: vec![],
    })
}

/// Every n-th file in path order, so a capped run still covers the whole corpus alphabetically
/// instead of only its first directory.
fn sample(all: &[(String, Language)], max_files: usize) -> Vec<(String, Language)> {
    if all.len() <= max_files || max_files == 0 {
        return all.to_vec();
    }
    let stride = all.len().div_ceil(max_files);
    all.iter()
        .step_by(stride)
        .take(max_files)
        .cloned()
        .collect()
}

/// Median of a signed list: a negative median is a real answer (the tool cost more than the
/// file), so nothing here clamps.
fn median_i64(values: &mut [i64]) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

/// Median of a value list (sorted in place, so the caller gives up the order it had).
fn median(values: &mut [u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

/// The `p` quantile of an already-sorted list: nearest-rank, `p` in 0..=100.
fn quantile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (p * (sorted.len() - 1)) / 100;
    sorted[rank]
}

/// Estimated tokens for a byte count. `ceil`, because a fraction of a token is still a token.
fn tokens(bytes: u64) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

/// `a / b` as a percentage, rounded to one decimal. `None` when there is no denominator.
fn share(numerator: u64, denominator: u64) -> String {
    if denominator == 0 {
        return "-".to_string();
    }
    format!("{:.1}", (numerator as f64 / denominator as f64) * 100.0)
}

/// `3rd`, `21st`, ... for the sampling sentence.
fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The corpus identity: a hash over `size`, `path` and `content hash` of every measured file, in
/// order.
///
/// The absolute path is deliberately NOT part of it: what makes two runs comparable is the same
/// files with the same contents, not the directory they were unpacked into. The CONTENT hash is
/// part of it because a hash of sizes and paths alone cannot tell `x = 1` from `x = 2` - two
/// different corpora with the same shape would have shared an id, and the id is what makes a
/// number comparable between runs.
fn corpus_id(costs: &[FileCosts]) -> String {
    let mut buf = String::new();
    for c in costs {
        buf.push_str(&format!("{}\t{}\t{}\n", c.bytes, c.rel, c.content_hash));
    }
    let hex = ContentHash::of(buf.as_bytes()).0;
    hex.iter().map(|b| format!("{b:02x}")).collect()
}

/// Everything the report needs that is not a measurement: the configuration, the three
/// contexts the scenario table calls with, and the counts the walk produced. One struct,
/// because nine positional arguments is where a reader stops checking.
struct ReportInputs<'a> {
    cfg: &'a BenchConfig,
    /// Default limits: what a caller gets without configuring anything.
    default_ctx: &'a ToolContext,
    /// Default output cap, `max_results` raised so `limit=1000` is a legal argument.
    many_ctx: &'a ToolContext,
    /// `limit=1000` with the output cap raised, so the outline is not cut off.
    wide_ctx: &'a ToolContext,
    budget: &'a Limits,
    /// Files with a grammar in the corpus.
    available: usize,
    /// `ast_get` calls that could not address a single symbol.
    get_unresolved: usize,
    /// Sampled files that could not be read at all.
    unreadable: usize,
    /// Measured files that could not be outlined.
    unoutlineable: usize,
}

/// Build the report.
fn report(inp: &ReportInputs<'_>, costs: &[FileCosts]) -> String {
    let ReportInputs {
        cfg,
        default_ctx: ctx,
        many_ctx: many,
        wide_ctx: wide,
        budget,
        available,
        get_unresolved: get_unresolved_total,
        unreadable,
        unoutlineable,
    } = *inp;
    let total_symbols: usize = costs.iter().map(|c| c.symbols).sum();
    let measured_symbols: usize = costs.iter().map(|c| c.symbols_measured).sum();
    let mut out = String::new();
    let measured: u64 = costs.iter().map(|c| c.bytes).sum();
    let get_total: u64 = costs.iter().map(|c| c.get_total_bytes).sum();
    let get_median_sum: u64 = costs.iter().map(|c| c.get_median_bytes).sum();

    out.push_str("# Token benchmark: reading tools\n\n");
    out.push_str(&format!(
        "Corpus **{}**. {}\n\n",
        cfg.label,
        if cfg.note.is_empty() {
            "No description was given for this corpus.".to_string()
        } else {
            cfg.note.clone()
        }
    ));

    // ---------------------------------------------------------------- corpus
    out.push_str("## Corpus\n\n");
    out.push_str(&format!(
        "| Files with a grammar | Files measured | Bytes measured | Corpus id (sha256 of `size\\tpath\\tcontent`) |\n\
         |---|---|---|---|\n| {} | {} | {} | `{}` |\n\n",
        available,
        costs.len(),
        measured,
        corpus_id(costs)
    ));
    if costs.len() < available {
        out.push_str(&format!(
            "Sampled: every {} file in path order, because the cap was {}. The numbers below \
             describe those files, not the whole corpus.\n\n",
            ordinal(available.div_ceil(costs.len().max(1))),
            costs.len()
        ));
    }
    if unreadable > 0 || unoutlineable > 0 {
        out.push_str(&format!(
            "{unreadable} sampled file(s) could not be read at all (no grammar, not UTF-8, or \
             over `max_file_bytes`) and {unoutlineable} could not be outlined. They are in \
             neither table below, so the measured set is smaller than the sampled set.\n\n"
        ));
    }
    out.push_str(&format!(
        "Reproduce:\n\n```sh\ncargo run -p opencrayast-tools --example token_bench -- \"$CORPUS\" \\\n  \
         --label {} --seed {} --max-files {}\n```\n\n",
        cfg.label, cfg.seed, cfg.max_files
    ));
    out.push_str(
        "`$CORPUS` is this corpus's directory. It is not in the report because a committed report \
         must not name the machine it was measured on; the corpus id above identifies what was \
         measured.\n\n",
    );

    // ---------------------------------------------------------------- per language
    out.push_str("## Per language\n\n");
    let empty = costs.iter().filter(|c| c.bytes == 0).count();
    out.push_str(&format!(
        "`outline/file` is the outline's bytes as a percentage of the file's bytes: lower is \
         better, above 100% means the outline was bigger than the file. The ratio columns cover \
         files with content only: {} empty file(s) have no meaningful ratio and are left out of \
         them (they are still in the byte totals and in \"Where the tools lose\").\n\n",
        empty
    ));
    out.push_str(
        "| Language | Files | File bytes | Outline bytes | `outline/file` median | p90 | min | max \
         | Outline bigger than file |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    for language in Language::all() {
        let group: Vec<&FileCosts> = costs.iter().filter(|c| c.language == *language).collect();
        if group.is_empty() {
            continue;
        }
        let mut ratios: Vec<u64> = non_empty(&group)
            .iter()
            .map(|c| c.outline_bytes * 100 / c.bytes)
            .collect();
        ratios.sort_unstable();
        let non_empty_count = non_empty(&group).len();
        let bigger = group.iter().filter(|c| c.outline_bytes > c.bytes).count();
        out.push_str(&format!(
            "| {} | {} | {} | {} | {}% | {}% | {}% | {}% | {} of {} |\n",
            language.id(),
            group.len(),
            group.iter().map(|c| c.bytes).sum::<u64>(),
            group.iter().map(|c| c.outline_bytes).sum::<u64>(),
            quantile(&ratios, 50),
            quantile(&ratios, 90),
            ratios.first().copied().unwrap_or(0),
            ratios.last().copied().unwrap_or(0),
            bigger,
            non_empty_count,
        ));
    }
    out.push('\n');

    // ---------------------------------------------------------------- distribution
    let mut all_ratios: Vec<u64> = non_empty(&costs.iter().collect::<Vec<_>>())
        .iter()
        .map(|c| c.outline_bytes * 100 / c.bytes)
        .collect();
    all_ratios.sort_unstable();
    out.push_str("## Distribution of `outline/file`, all languages\n\n");
    out.push_str(&format!(
        "| min | p10 | median | p90 | max | median file bytes |\n|---|---|---|---|---|---|\n\
         | {}% | {}% | {}% | {}% | {}% | {} |\n\n",
        all_ratios.first().copied().unwrap_or(0),
        quantile(&all_ratios, 10),
        quantile(&all_ratios, 50),
        quantile(&all_ratios, 90),
        all_ratios.last().copied().unwrap_or(0),
        median(&mut costs.iter().map(|c| c.bytes).collect::<Vec<_>>()),
    ));

    // ---------------------------------------------------------------- scenarios
    out.push_str("## Scenarios\n\n");
    out.push_str("### Exploration: understand a directory\n\n");
    out.push_str(
        "Baseline: an agent reads every file of the corpus. Treatment: one `ast_outline .` call \
         for the whole corpus.\n\n",
    );
    out.push_str("| Variant | Bytes returned | Estimated tokens | Notes |\n|---|---|---|---|\n");
    let baseline = measured;
    for (name, variant_ctx, limit) in [
        ("default limits (200 results)", ctx, None),
        ("limit=1000", many, Some(1000u64)),
        ("limit=1000, output cap 8 MiB", wide, Some(1000u64)),
    ] {
        let args = OutlineArgs {
            path: ".".to_string(),
            limit,
            ..Default::default()
        };
        let (bytes, note) = match ast_outline(variant_ctx, &args) {
            Ok(text) => {
                let mut notes: Vec<String> = Vec::new();
                if text.contains("[truncated:") {
                    notes.push("TRUNCATED by the output cap or the symbol limit".to_string());
                }
                if text.contains("[walk truncated") {
                    notes.push("the walk itself stopped at max_scan_files".to_string());
                }
                if text.contains("[skipped:") {
                    notes.push(
                        "some files were skipped (see the skipped line in the output)".to_string(),
                    );
                }
                (text.len() as u64, notes.join("; "))
            }
            Err(e) => (
                0,
                format!("the call failed: {} ({})", e.code.as_str(), e.message),
            ),
        };
        out.push_str(&format!(
            "| {name} | {bytes} | {} | {} |\n",
            tokens(bytes),
            if note.is_empty() {
                "-".to_string()
            } else {
                note
            }
        ));
    }
    out.push_str(&format!(
        "\nBaseline for the sampled files: {baseline} bytes (~{} tokens). A single `ast_outline .` \
         covers the whole corpus, so the honest baseline for this row is the size of every file in \
         the corpus, not just the sampled ones: {} files.\n\n",
        tokens(baseline),
        available
    ));

    out.push_str("### Retrieval: get one symbol per file\n\n");
    out.push_str(&format!(
        "For each measured file one symbol was picked with seed {} and fetched with `ast_get` \
         (doc comment included). Baseline: reading the whole file to find the same symbol.\n\n",
        cfg.seed
    ));
    let retrieval_files = costs.iter().filter(|c| c.get_one_bytes > 0).count();
    let get_one_total: u64 = costs.iter().map(|c| c.get_one_bytes).sum();
    let fetched: Vec<&FileCosts> = costs.iter().filter(|c| c.get_one_bytes > 0).collect();
    let mut get_one: Vec<u64> = fetched.iter().map(|c| c.get_one_bytes).collect();
    // The median of the per-file SAVING, which is not the baseline minus the median: one of
    // those two would answer a question nobody asked. Signed, because a negative saving is a
    // real result - `ast_get` on a three-line file returns a header and a fence around it, and
    // that is more bytes than the file. Clamping it to zero would be inventing a number.
    let saved_sum = measured as i64 - get_one_total as i64;
    let mut saving: Vec<i64> = fetched
        .iter()
        .map(|c| c.bytes as i64 - c.get_one_bytes as i64)
        .collect();
    out.push_str(&format!(
        "| Files with a symbol | Read the file | `ast_get` (sum) | `ast_get` (median per file) | \
         Saved, sum | Saved, median |\n|---|---|---|---|---|---|\n\
         | {retrieval_files} | {baseline} | {get_one_total} | {} | {} | {} |\n\n",
        median(&mut get_one),
        saved_sum,
        median_i64(&mut saving),
    ));
    out.push_str(&format!(
        "Fetching one symbol per file cost {} bytes against {} bytes for reading those same \
         files whole: {} bytes saved, at a median of {} bytes saved per file. This is the \
         scenario the tools are built for, and it is the one where they win by a lot. The \
         section below is where they do not.\n\n",
        get_one_total,
        baseline,
        saved_sum,
        median_i64(&mut saving),
    ));

    out.push_str(&format!(
        "### What fetching EVERY top-level symbol costs\n\n\
         On top of that, up to {} symbols per file were fetched to measure what a symbol costs \
         rather than what a file costs: {} bytes over {} calls, median {} bytes per call. Every \
         call re-reads and re-parses the file, which is what the tool does.\n\n",
        cfg.max_gets_per_file,
        get_total,
        costs.iter().map(|c| c.symbols_measured).sum::<usize>(),
        get_median_sum
            / costs
                .iter()
                .filter(|c| c.symbols_measured > 0)
                .count()
                .max(1) as u64,
    ));
    if get_unresolved_total > 0 {
        out.push_str(&format!(
            "{get_unresolved_total} `ast_get` calls could not address a single symbol and were \
             not guessed at (the language allows the same qualified name twice). They are counted \
             here and excluded from the medians.\n\n"
        ));
    }

    // ---------------------------------------------------------------- the ugly part
    let bigger: Vec<&FileCosts> = costs.iter().filter(|c| c.outline_bytes > c.bytes).collect();
    out.push_str("## Where the tools lose\n\n");
    out.push_str(&format!(
        "{} of {} measured files ({}%) have an outline BIGGER than the file itself. For those \
         files, reading the file is the cheaper way to get the same information.\n\n",
        bigger.len(),
        costs.len(),
        share(bigger.len() as u64, costs.len() as u64)
    ));
    if !bigger.is_empty() {
        let mut worst: Vec<&&FileCosts> = bigger.iter().collect();
        worst.sort_by_key(|c| std::cmp::Reverse(c.outline_bytes.saturating_sub(c.bytes)));
        out.push_str("The ten worst, by wasted bytes:\n\n");
        out.push_str(
            "| File | File bytes | Outline bytes | Wasted | Symbols |\n|---|---|---|---|---|\n",
        );
        for c in worst.iter().take(10) {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                c.rel,
                c.bytes,
                c.outline_bytes,
                c.outline_bytes - c.bytes,
                c.symbols
            ));
        }
        out.push('\n');
    }

    // ---------------------------------------------------------------- limits
    out.push_str("## Limits, and where this is unfair\n\n");
    out.push_str(
        "- **Bytes are the measurement; tokens are `ceil(bytes / 4)`.** That is not a tokeniser. \
         A real tokeniser charges more for code than for English, so every token figure here is \
         optimistic in both directions.\n\
         - **Reading a whole file is unbeatable on a small file.** The outline of a file with one \
         short function is longer than the file. The table above counts those files instead of \
         averaging them away.\n\
         - **An outline that was truncated has not saved anything.** It has refused and said so; \
         a smaller truncated outline is not a cheaper answer to the question. The exploration \
         table marks every truncated run.\n\
         - **The baseline is a whole file read, not a targeted read.** An agent that already \
         knows which lines it wants could read just those. That would make the baseline smaller \
         and the savings smaller than reported here.\n\
         - **`ast_get` re-reads and re-parses the file on every call.** The bytes are what the \
         tool returned; the CPU is not measured at all.\n\
         - **No task was attempted.** Nothing here says an agent using these tools does the job \
         better, faster or at all. Returning less is only a saving if what was returned was \
         enough.\n\
         - **Files that could not be outlined are not in the table.** A file whose language has \
         no grammar, or that is not UTF-8, or that is over the size limit, is skipped by \
         `ast_outline` and counted there; excluding it here makes the corpus smaller than the \
         directory on disk.\n\
         - **The corpus is one machine's copy of some projects.** Which crates, which standard \
         library, which bundled JavaScript happened to be unpacked here is not a sample of what \
         agents read in the world.\n\n",
    );

    // ---------------------------------------------------------------- what it is not
    out.push_str("## What these numbers do not mean\n\n");
    out.push_str(
        "- They are **not** a task success rate. Nothing here ran an agent on a task.\n\
         - They are **not** latency, CPU time or memory. No clock was read.\n\
         - They are **not** a claim about any model's context window or about how many tokens a \
         model will actually spend.\n\
         - They are **not** comparable with a number produced by a different corpus, a different \
         machine or a different `Limits`. The corpus id is what makes two runs comparable.\n\
         - They are **not** a marketing number. The worst table above is part of the result.\n",
    );

    out.push_str(&format!(
        "\n## Method\n\n\
         - Corpus walked with `core::walk` (`respect_gitignore`), language by \
           `Language::detect` on the file name; files with no grammar are skipped.\n\
         - Per file: the bytes the boundary read; the bytes `ast_outline <file>` returned; and \
           one `ast_get` per symbol at depth <= {GET_MAX_DEPTH}, addressed by its qualified name.\n\
         - Exploration uses `ast_outline .` on the whole corpus; retrieval sums one `ast_get` per \
           measured file.\n\
         - The symbol for the retrieval row is chosen by a hash of the seed and the file's \
           relative path, so the same seed picks the same symbols on every machine. When the \
           qualified name is ambiguous the bare name is tried; when that is ambiguous too the \
           call is counted as unresolved and no bytes are recorded.\n\
         - At most {} `ast_get` calls per file were spent measuring what a symbol costs; files \
           with more symbols than that contributed only their first {} in source order. Total \
           symbols found: {}, measured: {}.\n\
         - Limits for the per-file measurements are the defaults: file {} bytes, output {} bytes, \
           results {}, scan {} files.\n",
        cfg.max_gets_per_file,
        cfg.max_gets_per_file,
        total_symbols,
        measured_symbols,
        budget.max_file_bytes,
        budget.max_output_bytes,
        budget.max_results,
        budget.max_scan_files,
    ));
    out
}
