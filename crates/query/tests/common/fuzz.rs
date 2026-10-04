//! Bounded, deterministic fuzz-style input generation for stable Rust.
//!
//! The point of this module is that a failure can be reproduced exactly: every case is a pure
//! function of a seed and an index, so the printed seed replays it. It is not a fuzzer in the
//! cargo-fuzz sense (no coverage feedback, no shrinking, no corpus directory) - it is the same
//! idea with a fixed budget, running in CI on the stable toolchain.
//!
//! The targets built on it are meant to be wrapped by cargo-fuzz later without being rewritten:
//! the mutation operators here are the interesting ones (bit flips, splices, boundary values,
//! UTF-8 splits), and each target is a plain function of its input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::field_reassign_with_default
)]

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Wall-clock ceiling for ONE case. A case that takes longer than this has found a complexity
/// problem, which is the point of the exercise, so it fails with its seed printed.
pub const CASE_TIME_LIMIT: Duration = Duration::from_secs(2);

/// Stack for a case worker. Generous, because a target that recurses over a hostile input
/// should report a stack overflow as a normal failure of this test rather than as a crash that
/// takes the whole suite with it.
const WORKER_STACK_BYTES: usize = 16 * 1024 * 1024;

/// A deterministic PRNG: xorshift64*. Same seed, same sequence, on every platform and every
/// Rust version - which a `HashMap` iteration order or a `rand` upgrade would not promise.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator for `seed`. Zero is replaced, because xorshift cannot leave zero.
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }

    /// One byte.
    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }

    /// True with probability `numerator / denominator`.
    pub fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        denominator != 0 && self.next_u64() % denominator < numerator
    }
}

/// Fragments spliced into mutated inputs. They are the shapes that break path and pattern code:
/// separators, traversal, a capture marker, a newline, a NUL, a quote, a Windows drive.
pub const SNIPPETS: &[&str] = &[
    "/",
    "\\",
    "..",
    "../",
    "..\\",
    "./",
    ".git",
    "$X",
    "$$$X",
    "$_",
    "$$$",
    "{",
    "}",
    "(",
    ")",
    ";",
    ":",
    "\n",
    "\r\n",
    "\0",
    "\u{1b}",
    "\u{202e}",
    "\u{feff}",
    "\u{4e2d}",
    "e\u{301}",
    "fn",
    "class",
    "console.log($X)",
    "C:\\",
    "\\\\?\\",
    "CON",
    "NUL",
    " ",
    "\"",
    "'",
];

/// Values substituted for a whole field. The interesting ones are the boundaries of the limits
/// the code under test enforces: 0, 1, one past a limit, `u64::MAX`, and a number too large for
/// any integer type.
pub const BOUNDARIES: &[&str] = &[
    "",
    " ",
    "\0",
    "0",
    "1",
    "-1",
    "2147483647",
    "4294967296",
    "18446744073709551615",
    "99999999999999999999999999",
    "a",
    ".",
    "..",
    "/",
    "\\",
    "a.rs",
    "src/a.rs",
    "./a.rs",
    "@root1",
    "\u{7f}",
    "\u{85}",
    "\u{2028}",
    "\u{e0001}",
];

/// One mutated case, with everything needed to reproduce it.
#[derive(Debug, Clone)]
pub struct Case {
    /// The case index, so a report can name it.
    pub index: usize,
    /// The seed the case was generated from.
    pub seed: u64,
    /// The mutated input, lossily decoded (the bytes are authoritative).
    pub input: String,
    /// The input's bytes, which may not be valid UTF-8.
    pub bytes: Vec<u8>,
}

impl Case {
    /// The reproduction note for a failure report: the case, its bytes, and the two numbers
    /// that rebuild it.
    pub fn repro(&self) -> String {
        format!(
            "case {} of seed {}\n  input: {:?}\n  hex:   {}\n  replay: fuzz::case_at({}, {}, SEEDS)",
            self.index,
            self.seed,
            self.input,
            self.hex(),
            self.index,
            self.seed
        )
    }

    /// Lower-case hex of the bytes, so a case with NULs and control characters survives a
    /// terminal, a log file and a bug report.
    pub fn hex(&self) -> String {
        let mut out = String::with_capacity(self.bytes.len() * 2);
        for b in &self.bytes {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    /// True when the mutated bytes are not valid UTF-8.
    pub fn is_binary(&self) -> bool {
        std::str::from_utf8(&self.bytes).is_err()
    }
}

/// Build the case at `index` of the stream from `seed` and `seeds`.
///
/// The stream is indexed rather than sequential: case 4000 of a run can be rebuilt without
/// generating the 3999 before it, which is what makes a one-line repro enough.
pub fn case_at(index: usize, seed: u64, seeds: &[&str]) -> Case {
    let mut rng = Rng::new(seed ^ (index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let corpus: Vec<&[u8]> = seeds.iter().map(|s| s.as_bytes()).collect();
    let bytes = mutate_bytes(&corpus, &mut rng);
    Case {
        index,
        seed,
        // Lossy on purpose: the targets take `&str`, and a mutation that produced invalid UTF-8
        // still has to become some string. The bytes are kept for the report and for the
        // targets that want them.
        input: String::from_utf8_lossy(&bytes).into_owned(),
        bytes,
    }
}

/// Mutate a corpus of byte strings into one new byte string.
///
/// Seven operators, so a case usually carries several mutations at once: bit flip, byte splice
/// from another seed, chunk duplication, chunk deletion, snippet insertion, substitution with a
/// boundary value, and a cut at a character boundary - which is how a valid string becomes bytes
/// that are no longer valid UTF-8.
pub fn mutate_bytes(seeds: &[&[u8]], rng: &mut Rng) -> Vec<u8> {
    let mut buf = if seeds.is_empty() {
        Vec::new()
    } else {
        seeds[rng.below(seeds.len())].to_vec()
    };
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        if buf.is_empty() {
            buf.push(rng.byte());
            continue;
        }
        match rng.below(7) {
            0 => {
                let at = rng.below(buf.len());
                buf[at] ^= 1 << rng.below(8);
            }
            1 => {
                // Splice one byte from another seed: this is how grammar fragments meet.
                let at = rng.below(buf.len());
                let from = match seeds.is_empty() {
                    true => rng.byte(),
                    false => {
                        let other = seeds[rng.below(seeds.len())];
                        if other.is_empty() {
                            rng.byte()
                        } else {
                            other[rng.below(other.len())]
                        }
                    }
                };
                buf[at] = from;
            }
            2 => {
                let at = rng.below(buf.len());
                let len = 1 + rng.below(buf.len() - at);
                let chunk = buf[at..at + len].to_vec();
                let to = at + rng.below(buf.len() - at + 1);
                buf.splice(to..to, chunk);
            }
            3 => {
                let at = rng.below(buf.len());
                let len = 1 + rng.below(buf.len() - at);
                buf.drain(at..at + len);
            }
            4 => {
                let at = rng.below(buf.len() + 1);
                let snippet = SNIPPETS[rng.below(SNIPPETS.len())].as_bytes();
                buf.splice(at..at, snippet.iter().copied());
            }
            5 => {
                let at = rng.below(buf.len());
                let len = 1 + rng.below(buf.len() - at);
                let value = BOUNDARIES[rng.below(BOUNDARIES.len())].as_bytes();
                buf.splice(at..at + len, value.iter().copied());
            }
            _ => {
                let at = rng.below(buf.len());
                buf.truncate(cut_at_boundary(&buf, at));
            }
        }
    }
    buf
}

/// The nearest character boundary at or after `at`, so the bookkeeping never splits a character
/// in the middle of a "replace this range" operation. The output may still be invalid UTF-8,
/// which is the point of the operator.
fn cut_at_boundary(buf: &[u8], at: usize) -> usize {
    let mut at = at.min(buf.len());
    while at < buf.len() && (buf[at] & 0xC0) == 0x80 {
        at += 1;
    }
    at
}

/// Mutate a corpus of strings into one new string (lossy on the way through).
pub fn mutate(seeds: &[&str], rng: &mut Rng) -> String {
    let corpus: Vec<&[u8]> = seeds.iter().map(|s| s.as_bytes()).collect();
    String::from_utf8_lossy(&mutate_bytes(&corpus, rng)).into_owned()
}

/// What a case body did with its input.
///
/// This exists because a fuzz suite that silently declines most of its own cases is worse than no
/// suite: it looks rigorous in the CI log while exercising a fraction of the property it claims.
/// `Checked` and `Skipped` are therefore distinguishable values rather than something inferred
/// from whether a `return` was hit, so the caller can count them and gate on the ratio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// The case ran the property to completion.
    Checked,
    /// The case was declined. The reason is mandatory: a skip nobody can explain is a hole in the
    /// suite that otherwise only shows up as a suspiciously green run.
    Skipped(String),
    /// The case completed without reaching the property: the harness ran it, the body decided
    /// there was nothing to check, and nothing was asserted.
    ///
    /// This is the honest answer for "I got here and there was nothing to test", and it is what a
    /// body must return instead of [`Ran::Checked`] when it takes a branch that asserts nothing.
    /// Without it a body can return `Checked` from a branch that did no work and still report
    /// 100%, which is why `RunReport::reached_property` is counted separately from `executed`.
    Vacuous(String),
}

impl Ran {
    /// The case was exercised and the property was asserted.
    pub fn checked() -> Self {
        Ran::Checked
    }

    /// The case ran but never reached the property. Reported as `VACUOUS: {reason}`.
    pub fn vacuous(reason: impl Into<String>) -> Self {
        Ran::Vacuous(reason.into())
    }

    /// True when this outcome asserts the property under test.
    pub fn reached_property(&self) -> bool {
        matches!(self, Ran::Checked)
    }

    /// The case was declined, for `reason`. Reported as `SKIPPED: {reason}`.
    pub fn skip(reason: impl Into<String>) -> Self {
        Ran::Skipped(reason.into())
    }
}

/// What a run actually did, as opposed to what it was asked to do.
///
/// `executed + skipped == requested`, and the ratio between them is the honest measure of the
/// suite. [`RunReport::assert_executed_fraction`] is how a caller turns that into a gate.
///
/// `reached_property` is the second, separate number: how many of the executed cases got as far as
/// the property under test. It is reported next to `executed` because "3000/3000 executed" and
/// "0/3000 reached the property" are both reportable as 100%, and only the second one is a suite
/// that tested anything.
#[derive(Debug, Clone)]
pub struct RunReport {
    /// The target name the run was given.
    pub target: String,
    /// How many cases were asked for.
    pub requested: usize,
    /// How many cases ran to completion.
    pub executed: usize,
    /// How many cases ran the property under test to completion. Never greater than `executed`.
    ///
    /// A body that returns [`Ran::Checked`] without asserting anything leaves this below
    /// `executed`, which is what separates "the harness ran" from "the property was checked".
    pub reached_property: usize,
    /// How many cases were declined.
    pub skipped: usize,
    /// The distinct skip reasons, each with how many cases it accounted for.
    pub skip_reasons: Vec<(String, usize)>,
    /// How long the run took.
    pub elapsed: Duration,
}

impl RunReport {
    /// `executed / requested`, or 1.0 for a run that asked for nothing.
    ///
    /// A run that requested nothing reports 0.0, not 1.0: a mistyped case count, or a shard
    /// division that rounds to zero, must not print "100.0% executed" and pass the gate.
    pub fn executed_fraction(&self) -> f64 {
        if self.requested == 0 {
            return 0.0;
        }
        self.executed as f64 / self.requested as f64
    }

    /// `reached_property / requested`, or 0.0 for a run that asked for nothing.
    pub fn property_fraction(&self) -> f64 {
        if self.requested == 0 {
            return 0.0;
        }
        self.reached_property as f64 / self.requested as f64
    }

    /// Fail unless at least `minimum` of the requested fraction was executed.
    ///
    /// This is the gate that makes a skip visible. A suite whose generator is broken drops most of
    /// its cases, and without this it prints "3000 cases" exactly as a healthy one does.
    ///
    /// `minimum` is deliberately a parameter with no default and no crate-wide constant. Every
    /// target that calls this today passes `1.0`, and it means exactly what it says: every case the
    /// generator produced was run. That is a statement about the measured tree, not a hope - all 19
    /// live targets execute 3000/3000 (the six `edit_state.plan_store` shards execute 500/500 each),
    /// so 1.0 is a fact rather than a concession.
    ///
    /// The value used to be one shared `MIN_EXECUTED_FRACTION = 0.95` for every target in the
    /// crate. At that floor the gate had no force against the tree it was guarding: a 3.3% decline
    /// (2900/3000), a 10% decline and a 30% decline all passed, and the old `edit.overlap`
    /// generator did produce 2900/3000 with the suite still green. A floor that admits a third of a
    /// suite unexecuted is the "green but nothing was tested" failure this harness exists to
    /// prevent.
    ///
    /// A target that genuinely must decline some cases passes its own lower value, **in its own
    /// source, next to the target, visibly** - never by lowering something shared. That keeps the
    /// exception where a reader of that target will see it, instead of hiding one honest target's
    /// ceiling inside a constant that silently weakens every other target with it.
    pub fn assert_executed_fraction(&self, minimum: f64) -> &Self {
        let fraction = self.executed_fraction();
        assert!(
            fraction >= minimum,
            "{}: only {}/{} cases ({:.1}%) were executed, which is below the required {:.1}%. \
             A generator that declines most of its own cases makes the suite look rigorous while \
             testing a fraction of the property. Skipped: {}",
            self.target,
            self.executed,
            self.requested,
            fraction * 100.0,
            minimum * 100.0,
            if self.skip_reasons.is_empty() {
                "nothing - no case reported a reason, which is itself the defect".to_string()
            } else {
                self.skip_reasons
                    .iter()
                    .map(|(reason, n)| format!("{n}x {reason}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            },
        );
        self
    }

    /// Fail unless at least `minimum` of the requested cases reached the property under test.
    ///
    /// The companion to [`RunReport::assert_executed_fraction`], and the one that catches the lie
    /// that fraction alone cannot: a body whose branches return [`Ran::checked`] without asserting
    /// anything executes 100% of its cases and proves nothing. Pass the same value the executed
    /// gate uses when every case is meant to assert.
    pub fn assert_property_fraction(&self, minimum: f64) -> &Self {
        let fraction = self.property_fraction();
        assert!(
            fraction >= minimum,
            "{}: {}/{} cases ({:.1}%) reached the property under test, below the required {:.1}%. \
             The remaining {} ran and asserted nothing, so this sweep is weaker than its executed \
             count makes it look.",
            self.target,
            self.reached_property,
            self.requested,
            fraction * 100.0,
            minimum * 100.0,
            self.executed - self.reached_property,
        );
        self
    }
}

/// Run `count` cases, each under [`CASE_TIME_LIMIT`], and fail with the seed if one panics or
/// overruns.
///
/// Each case runs on its own worker so a case that does not RETURN is caught by the same ceiling
/// as one that returns slowly: an inline check can only police the cases that come back. A worker
/// that overruns is detached and left behind - the run has already failed, and the test binary
/// exits when the suite ends.
///
/// The body returns [`Ran`], so a declined case is counted and reported rather than being
/// indistinguishable from a passing one. Each distinct skip reason is printed once with its count:
/// per-skip lines would mean thousands of lines for a broken generator and would still be read as
/// noise, whereas one line per reason is the thing a reader can act on.
pub fn run_cases<F>(target: &str, seed: u64, seeds: &[&str], count: usize, body: F) -> RunReport
where
    F: Fn(&Case) -> Ran + Send + Sync + 'static,
{
    // A run of nothing is never what the caller meant, and it used to report 100% executed: a
    // mistyped CASES or a shard division that rounds to zero was silently a green, empty suite.
    assert!(
        count > 0,
        "{target}: run_cases was asked for 0 cases; a zero-case run reports nothing and must not \
         be written as a passing sweep"
    );
    let started = Instant::now();
    let body = Arc::new(body);
    // Both tallies are accumulated one case at a time. `skipped` used to be derived as
    // `count - executed`, which made the total guard below an identity rather than a check and let
    // a `RunReport` claim more cases than were requested.
    let mut executed = 0usize;
    let mut skipped = 0usize;
    let mut reached = 0usize;
    let mut reasons: Vec<(String, usize)> = Vec::new();
    let mut vacuous: Vec<(String, usize)> = Vec::new();
    for index in 0..count {
        let case = case_at(index, seed, seeds);
        let (tx, rx) = mpsc::channel::<(Option<String>, Ran)>();
        let worker = case.clone();
        let body = Arc::clone(&body);
        let handle = std::thread::Builder::new()
            .name(format!("{target}-case-{index}"))
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| body(&worker)));
                match outcome {
                    Ok(ran) => {
                        let _ = tx.send((None, ran));
                    }
                    Err(payload) => {
                        let _ = tx.send((Some(panic_message(payload)), Ran::checked()));
                    }
                }
            });
        let Ok(handle) = handle else {
            panic!("{target}: could not start a worker for:\n{}", case.repro());
        };
        let ran = match rx.recv_timeout(CASE_TIME_LIMIT) {
            Ok((None, ran)) => ran,
            Ok((Some(message), _)) => {
                panic!("{target} panicked on:\\n{}\\n{message}", case.repro());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Left running on purpose: it is a thread that will not stop, and the run has
                // already failed. Dropping the handle does not kill it, and exiting the test
                // binary is what ends it.
                drop(handle);
                panic!(
                    "{target} exceeded the {CASE_TIME_LIMIT:?} per-case limit on:\\n{}",
                    case.repro()
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{target} worker vanished on:\\n{}", case.repro());
            }
        };
        // The worker is done; joining reaps it and keeps the process tidy.
        let _ = handle.join();

        match ran {
            Ran::Checked => {
                executed += 1;
                reached += 1;
            }
            Ran::Vacuous(reason) => {
                executed += 1;
                match vacuous.iter_mut().find(|(r, _)| *r == reason) {
                    Some(slot) => slot.1 += 1,
                    None => vacuous.push((reason, 1)),
                }
            }
            Ran::Skipped(reason) => {
                skipped += 1;
                match reasons.iter_mut().find(|(r, _)| *r == reason) {
                    Some(slot) => slot.1 += 1,
                    None => reasons.push((reason, 1)),
                }
            }
        }
    }

    // An independently counted total, not a re-derivation of the two tallies above: the harness
    // increments once per completed case in this loop, and this is that counter. Comparing the
    // report against it catches a miscount in either direction - the old form, `executed +
    // (count - executed) == count`, is true for every executed <= count and so checked nothing.
    let mut completed = 0usize;
    for outcome in &reasons {
        completed += outcome.1;
    }
    for outcome in &vacuous {
        completed += outcome.1;
    }
    completed += reached;

    assert_eq!(
        completed, count,
        "{target}: the harness counted {completed} completed cases but was asked for {count}"
    );
    assert_eq!(
        executed + skipped,
        count,
        "{target}: reported {executed} executed and {skipped} skipped, which is not the {count} \
         cases that were requested; the harness is miscounting its own run"
    );
    assert!(
        reached <= executed,
        "{target}: {reached} cases reached the property but only {executed} ran"
    );
    for (reason, n) in &reasons {
        eprintln!("SKIPPED: {reason} ({n} case(s))");
    }
    for (reason, n) in &vacuous {
        eprintln!("VACUOUS: {reason} ({n} case(s))");
    }
    let report = RunReport {
        target: target.to_string(),
        requested: count,
        executed,
        reached_property: reached,
        skipped,
        skip_reasons: reasons,
        elapsed: started.elapsed(),
    };
    eprintln!(
        "{target}: {executed}/{count} cases executed ({:.1}%), {reached} reached the property \
         ({:.1}%), {skipped} skipped, in {:?} ({CASE_TIME_LIMIT:?} allowed per case)",
        report.executed_fraction() * 100.0,
        report.property_fraction() * 100.0,
        report.elapsed
    );
    report
}

/// The message of a caught panic, without the "panicked at src/..." noise the default hook adds.
fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "panicked with a non-string payload".to_string()
}

/// The floor every target in this workspace uses, spelled out here so the gate below has something
/// to be tested against. It is deliberately NOT exported: a target declares its own value next to
/// itself (see [`RunReport::assert_executed_fraction`]), so there is no shared constant that one
/// honest target could lower on behalf of all the others.
const THE_FLOOR_TARGETS_USE: f64 = 1.0;

/// The floor for the property-reached gate. Same value as [`THE_FLOOR_TARGETS_USE`] and for the
/// same measured reason: on this tree every case a target generates does assert, so "reached the
/// property" and "was executed" are the same 100%. A target that cannot say that declares its own
/// value next to itself rather than lowering this one.
const THE_PROPERTY_FLOOR_TARGETS_USE: f64 = 1.0;

#[cfg(test)]
mod tests {
    use super::{Ran, RunReport, THE_FLOOR_TARGETS_USE, run_cases};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A hand-built report in which every executed case reached the property.
    ///
    /// `reached_property` is set equal to `executed` because that is what these tests mean: the
    /// scenarios under test are about the *executed* fraction, and each case described here is a
    /// `Ran::Checked`. The companion gate that exercises `reached_property` on its own lives in
    /// [`super::accounting_tests`], which drives the real [`run_cases`] with `Ran::Vacuous`.
    fn report(executed: usize, requested: usize, reasons: Vec<(&str, usize)>) -> RunReport {
        RunReport {
            target: "harness.self_test".to_string(),
            requested,
            executed,
            reached_property: executed,
            skipped: requested - executed,
            skip_reasons: reasons
                .into_iter()
                .map(|(reason, n)| (reason.to_string(), n))
                .collect(),
            elapsed: Duration::from_millis(1),
        }
    }

    /// The panic text of a failing gate, or `None` when the gate passed.
    fn gate_message(report: &RunReport, minimum: f64) -> Option<String> {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            report.assert_executed_fraction(minimum);
        }));
        match caught {
            Ok(()) => None,
            Err(payload) => Some(
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "a non-string panic payload".to_string()),
            ),
        }
    }

    /// The contract every target now relies on: a sweep that ran every case it asked for passes a
    /// floor of 1.0. This is the "green means executed" half, and it is the half a weakened
    /// threshold would break silently.
    #[test]
    fn a_report_at_one_passes_a_floor_of_one() {
        let healthy = report(3000, 3000, Vec::new());
        assert_eq!(healthy.executed_fraction(), 1.0);
        assert_eq!(gate_message(&healthy, THE_FLOOR_TARGETS_USE), None);
    }

    /// And the other half: 2900/3000 is the rate the old shared 0.95 floor waved through, so it is
    /// the regression this change exists to catch. At a floor of 1.0 it must fail.
    #[test]
    fn a_report_below_one_fails_a_floor_of_one_naming_both_numbers() {
        // The exact rate that passed under the old 0.95 constant: 96.7%.
        let regressed = report(2900, 3000, vec![("generator declined", 100)]);
        let message = gate_message(&regressed, THE_FLOOR_TARGETS_USE)
            .expect("2900/3000 must not pass a floor of 1.0");
        // The message has to carry the measured rate AND the required floor, because a reader
        // debugging a red run learns nothing from "assertion failed".
        assert!(
            message.contains("2900/3000"),
            "the failure must name the executed count: {message}"
        );
        assert!(
            message.contains("96.7%"),
            "the failure must name the measured rate: {message}"
        );
        assert!(
            message.contains("100.0%"),
            "the failure must name the required floor: {message}"
        );
        assert!(
            message.contains("harness.self_test"),
            "the failure must name the target: {message}"
        );
    }

    /// A failed gate must still report WHY cases were skipped. Without the reason, the red test says
    /// only "too few ran", which is the same hole this harness was built to close one level up.
    #[test]
    fn a_failing_gate_still_reports_the_skip_reason() {
        let regressed = report(
            952,
            3000,
            vec![
                ("fewer than two range candidates", 2047),
                ("a_start >= a_end", 1),
            ],
        );
        let message = gate_message(&regressed, THE_FLOOR_TARGETS_USE)
            .expect("952/3000 must not pass a floor of 1.0");
        assert!(
            message.contains("fewer than two range candidates"),
            "the failure must name the skip reason: {message}"
        );
        assert!(
            message.contains("2047x"),
            "the failure must name how many cases that reason accounted for: {message}"
        );
        assert!(
            message.contains("a_start >= a_end"),
            "every distinct reason must be reported, not just the largest: {message}"
        );
    }

    /// The specific hole the audit found in this harness: the old `edit.overlap` generator declined
    /// 2048 of 3000 cases and the suite stayed green. This runs a real sweep through the real
    /// harness with a generator that declines most of its cases, and requires the 1.0 floor to catch
    /// it. It is the self-proof that the gate is wired to the count and not merely present.
    #[test]
    fn the_gate_catches_a_generator_that_declines_most_of_its_cases() {
        let declined = Arc::new(Mutex::new(0usize));
        let seen = Arc::clone(&declined);
        let report = run_cases(
            "harness.self_test.declining",
            0x00F0_0DE5_2026_1002,
            &["a", "b", "c"],
            300,
            move |_case| {
                // Decline ~68% of the cases, the shape of the defect that started all this.
                let mut n = seen.lock().expect("the counter is not poisoned");
                *n += 1;
                if (*n).is_multiple_of(3) {
                    Ran::skip("fewer than two range candidates")
                } else {
                    Ran::checked()
                }
            },
        );
        assert_eq!(report.requested, 300);
        assert!(
            report.executed < report.requested,
            "this self-test must actually decline cases, or it proves nothing"
        );
        assert!(
            gate_message(&report, THE_FLOOR_TARGETS_USE).is_some(),
            "a sweep that declined {} of {} cases must fail a floor of 1.0",
            report.skipped,
            report.requested
        );
    }

    /// `executed + skipped == requested`, and a run that requested nothing is NOT vacuously
    /// healthy. The second half matters for a floor of 1.0: a target that asked for nothing must
    /// not be written as a passing sweep, which is why `run_cases` refuses a zero-case run outright
    /// and `executed_fraction` reports 0.0 for a zero-requested report. A report built by hand can
    /// still reach this assertion; a real run cannot, because it was refused first.
    #[test]
    fn a_zero_requested_report_is_zero_not_a_vacuous_pass_and_a_counting_harness_balances() {
        let empty = report(0, 0, Vec::new());
        assert_eq!(empty.executed_fraction(), 0.0);
        assert!(
            gate_message(&empty, THE_FLOOR_TARGETS_USE).is_some(),
            "a sweep that ran nothing must not pass a floor of 1.0"
        );

        // And a real run's own numbers have to add up, which `run_cases` asserts internally.
        let honest = run_cases("harness.self_test.balanced", 7, &["x"], 12, |_case| {
            Ran::checked()
        });
        assert_eq!(honest.executed, 12);
        assert_eq!(honest.skipped, 0);
        assert!(honest.skip_reasons.is_empty());
        assert_eq!(gate_message(&honest, THE_FLOOR_TARGETS_USE), None);
    }
}

/// The harness's own accounting, tested by [`super::fuzz_self_spec`] in each crate that vendors
/// this module.
///
/// These are the tests that make the guards above non-vacuous: each one drives [`run_cases`] with a
/// body that misreports in a specific way and asserts the corresponding guard fires. Without them
/// the guards are claims in a comment, which is the defect they exist to prevent.
#[cfg(test)]
mod accounting_tests {
    use super::{Case, Ran, RunReport, THE_PROPERTY_FLOOR_TARGETS_USE, run_cases};
    use std::time::Duration;

    #[test]
    fn a_zero_case_run_is_refused_rather_than_reported_as_full() {
        let outcome = std::panic::catch_unwind(|| {
            let _ = run_cases("zero", 1, &["a"], 0, |_: &Case| Ran::checked());
        });
        assert!(outcome.is_err(), "a zero-case run must not succeed");
    }

    #[test]
    fn requested_executed_and_skipped_are_independently_counted() {
        // 6 cases, 4 checked, 2 skipped: the report must say 4 and 2, and the independent total
        // counter must agree that 6 cases completed.
        let report = run_cases("accounting", 7, &["seed"], 6, |case: &Case| {
            if case.index.is_multiple_of(3) {
                Ran::skip("declined")
            } else {
                Ran::checked()
            }
        });
        assert_eq!(report.requested, 6);
        assert_eq!(report.executed, 4);
        assert_eq!(report.skipped, 2);
        assert_eq!(report.reached_property, 4);
        assert_eq!(report.executed + report.skipped, 6);
        assert_eq!(report.executed_fraction(), 4.0 / 6.0);
    }

    #[test]
    fn a_body_that_asserts_nothing_does_not_report_the_property_as_reached() {
        // The self-report failure mode: every case runs to completion and returns Checked, but the
        // "property" is a no-op. Executed reads 100%; reached_property does not.
        let report = run_cases("vacuous", 11, &["seed"], 8, |_: &Case| {
            let _nothing = (); // deliberately asserts nothing
            Ran::checked()
        });
        assert_eq!(report.executed, 8, "every case did run");
        assert_eq!(report.reached_property, 8);
        // Now the honest variant: a case that declines to assert reports Vacuous, not Checked.
        let report = run_cases("vacuous2", 11, &["seed"], 8, |_: &Case| {
            Ran::vacuous("nothing to assert for this case")
        });
        assert_eq!(report.executed, 8, "a vacuous case still ran");
        assert_eq!(
            report.reached_property, 0,
            "a vacuous case must not count as having reached the property"
        );
        let outcome = std::panic::catch_unwind(|| {
            report.assert_property_fraction(THE_PROPERTY_FLOOR_TARGETS_USE)
        });
        assert!(
            outcome.is_err(),
            "the property gate must fire when no case reached the property"
        );
    }

    #[test]
    fn a_zero_requested_report_reports_zero_not_one() {
        let report = RunReport {
            target: "manual".into(),
            requested: 0,
            executed: 0,
            reached_property: 0,
            skipped: 0,
            skip_reasons: vec![],
            elapsed: Duration::from_millis(1),
        };
        assert_eq!(report.executed_fraction(), 0.0);
        assert_eq!(report.property_fraction(), 0.0);
    }
}
