//! The human gate: "may I change these files?", and the [`Confirmer`] that decides where the
//! answer comes from.
//!
//! A non-interactive environment with no `--yes` must refuse to apply. Stated here as the
//! crate's own acceptance criterion, with no ticket reference in crate source:
//!
//! and the invariant it protects is T-30 ([`docs/SECURITY-MODEL.md`]): an agent that can write
//! would rather write than ask. So the gate is not politeness — it is the only thing standing
//! between a script and an unattended `apply`.
//!
//! # One home for the decision
//!
//! [`authorize`] is the only function in this crate that turns a confirmation into a yes or a no.
//! `edit apply`, `edit undo` and `edit recover` all reach it, and none of them re-implements any
//! part of it: they resolve what would change, hand it over, and do nothing at all unless the
//! return value is [`Decision::Proceed`]. A gate that three commands each carry a copy of is
//! three gates, and the copy that drifts is the one nobody reviews.
//!
//! # Why the answer is data and not a `stdin` read
//!
//! Interactivity is a **parameter**, never a fact observed at the point of use.
//! [`Interaction::detect`] is the one function that looks at a terminal, and it is called once, by
//! the shell, outside the tests. A test that called `is_terminal()` on its own stdin would be
//! asserting a property of the machine it happened to run on: it passes under a developer's
//! terminal and fails in CI, where stdin is `/dev/null` — and the only way to make it pass in CI
//! is to stop testing the branch. So the whole matrix — interactive/non-interactive ×
//! confirmed/declined/`--yes` — becomes values a table can enumerate, identical on every machine.
//!
//! [`Interaction`] is therefore not a second gate beside [`Confirmer::may_decide`]: it is the
//! answer to one question ("is somebody there?"), and its only job is to decide what
//! [`Confirmer::may_decide`] a caller ends up with ([`Interaction::may_decide`]). There is exactly
//! one condition that refuses, and it is written once, in [`authorize`].
//!
//! # `may_decide` and `confirm` are separate questions
//!
//! [`Confirmer::may_decide`] asks whether a decision can be obtained here at all; [`Confirmer::confirm`]
//! asks the question and reports the answer. Conflating them is the bug [`WavesItThrough`] exists
//! to catch: if the gate consulted only the answer, a confirmer that answers `Yes` with nobody
//! present would authorise a write. [`WavesItThrough`] is exactly that confirmer, and
//! `cli2_the_refusal_survives_a_confirmer_that_would_say_yes` pins the refusal against it.
//!
//! # Where the refusal is spelled
//!
//! Both refusals are [`ErrorCode::InvalidArgs`] and leave through the shared [`exit::exit_code_for`],
//! so the CLI cannot invent a private exit bucket for its own answer. A new [`ErrorCode`] variant
//! would be a change to L0's public taxonomy, which this work does not own, and `invalid_args` is
//! already documented as "missing, mistyped or out-of-range argument", whose next step is "the
//! valid form". Here the valid form is `--yes`.
//!
//! # Zero writes
//!
//! Nothing in this module touches the filesystem, opens a store or names a lock. It runs before the
//! write handlers are called at all, so a refusal has nothing to roll back — which is a stronger
//! statement than "we undo what we did", and it is why the refusal path returns rather than
//! attempting a rollback that has nothing to roll back.
//!
//! [T-30]: ../../SECURITY-MODEL.md

use crate::exit;
use crate::out::Out;
use opencrayast_core::ErrorCode;
use opencrayast_core::error::ToolError;
use std::collections::VecDeque;
use std::fmt;
use std::io::{BufRead, IsTerminal};

/// Can a person answer a question here?
///
/// A parameter rather than a fact, for the reason at the top of this file. It carries no gate of
/// its own: it is the environment half of the one condition [`authorize`] checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interaction {
    /// A person can be asked, and will be.
    Interactive,
    /// Nothing on the other end can answer: a pipe, a cron job, a CI step, `nohup`.
    ///
    /// A script cannot be asked a question, so an unanswered question is not a slow question — it
    /// is a no.
    NonInteractive,
}

impl Interaction {
    /// Look at this process's stdin and decide.
    ///
    /// The **only** place interactivity is observed, called by the shell that owns stdin and
    /// deliberately not by any test.
    pub fn detect() -> Interaction {
        if std::io::stdin().is_terminal() {
            Interaction::Interactive
        } else {
            Interaction::NonInteractive
        }
    }

    /// Whether a person can be asked.
    pub fn is_interactive(self) -> bool {
        matches!(self, Interaction::Interactive)
    }

    /// What this answers for a [`Confirmer`]: may the decision be obtained here?
    ///
    /// The single bridge between the two halves of the gate, so the two cannot disagree.
    pub fn may_decide(self) -> bool {
        self.is_interactive()
    }

    /// `interactive` or `non-interactive`, for the refusal message and the docs.
    pub fn as_str(self) -> &'static str {
        match self {
            Interaction::Interactive => "interactive",
            Interaction::NonInteractive => "non-interactive",
        }
    }
}

/// What a person said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Go ahead.
    Yes,
    /// Do not go ahead, and somebody was there to say it.
    No,
    /// There is nobody to ask: no terminal, or the input ended.
    Refused,
}

impl Answer {
    /// The words this CLI accepts, lowercased. `y`, `yes` and an empty line mean yes; anything
    /// else means no. Deliberately a small set: a confirmation prompt that accepts fourteen
    /// spellings of yes is a prompt nobody reads.
    pub fn from_word(word: &str) -> Answer {
        match word.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => Answer::Yes,
            _ => Answer::No,
        }
    }

    /// `true` only for an answer that authorises the write.
    ///
    /// `No` and `Refused` are the same event from the workspace's point of view — nothing was
    /// confirmed — which is what lets one refusal message cover both.
    pub fn granted(self) -> bool {
        self == Answer::Yes
    }
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Answer::Yes => "yes",
            Answer::No => "no",
            Answer::Refused => "refused",
        })
    }
}

/// Someone who can authorise a change to the workspace, or say there is nobody to ask.
pub trait Confirmer {
    /// Can the decision be obtained here at all? `false` is what makes a write refuse without
    /// `--yes`.
    fn may_decide(&self) -> bool;

    /// Will a question actually be put to somebody?
    ///
    /// `true` for every confirmer that asks: the terminal, and an injected answer. `false` for
    /// [`AlwaysYes`], because the decision was made on the command line before the program started
    /// and re-asking it would be theatre. It matters because the command prints the question it
    /// asks — printing a `(y/N)` under `--yes` would tell a script log that somebody was asked when
    /// nobody was.
    fn will_ask(&self) -> bool {
        self.may_decide()
    }

    /// Answer the question that was just printed.
    ///
    /// Only called when [`Confirmer::may_decide`] is `true`. A refuser answers
    /// [`Answer::Refused`], which is the same as `No` to every caller.
    fn confirm(&mut self, question: &str) -> Answer;
}

/// The production confirmer: a real terminal on stdin.
///
/// [`Confirmer::may_decide`] is [`Interaction::detect`] read through [`Interaction::may_decide`],
/// so the one place that looks at a terminal stays in one place.
///
/// # The test seam, and what it costs
///
/// `confirm` reaches for two process globals — [`Interaction::detect`] and the real stdin — and
/// neither can be replaced from a test. The module's own rule ("interactivity is a parameter,
/// never a fact observed at the point of use") is what makes that a problem: a test that called
/// [`Interaction::detect`] would be asserting a property of the machine it ran on, and the only
/// way to make that pass in CI is to delete the test.
///
/// So [`Stdin`] carries a `#[cfg(test)]` seam that stands in for **both** globals. The second half
/// is the part that matters: a seam for the interaction alone would not prove anything, because
/// when the guard is removed the code falls through to `read_line` on the *test binary's* stdin,
/// which is `/dev/null`, which yields `Ok(0)` — also `Refused`. The mutation would stay green
/// against a test that fed the seam a line saying `y`.
///
/// With the read also injected, the guard becomes observable in two independent ways: the returned
/// `Answer` **and** a counter proving the line was never read at all. The cost is a duplicated
/// parse line under `cfg(test)` and no coverage of the real [`Interaction::detect`] — the same
/// trade every other test in this crate makes, and the reason the live `Stdin` has no test at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stdin {
    _private: (),
    /// Test-only stand-in for the two process globals `confirm` reads. `None` in production.
    #[cfg(test)]
    seam: Option<TestSeam>,
}

/// What [`Stdin`] reads instead of the process's terminal, under `cfg(test)`.
///
/// `reads` is shared so a test can assert the guard returned **without consulting the answer
/// source**, which is the property the guard exists for and which the returned `Answer` alone
/// cannot establish.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct TestSeam {
    /// Stands in for [`Interaction::detect`].
    interaction: Interaction,
    /// Stands in for the line a terminal would have produced.
    line: &'static str,
    /// Incremented every time `line` is actually consulted.
    reads: &'static std::sync::atomic::AtomicUsize,
}

impl Stdin {
    /// A confirmer that asks a person on the terminal.
    pub fn new() -> Stdin {
        Stdin {
            _private: (),
            #[cfg(test)]
            seam: None,
        }
    }
}

impl Confirmer for Stdin {
    fn may_decide(&self) -> bool {
        #[cfg(test)]
        if let Some(seam) = self.seam {
            return seam.interaction.may_decide();
        }
        Interaction::detect().may_decide()
    }

    fn confirm(&mut self, _question: &str) -> Answer {
        if !self.may_decide() {
            // Not asked at all. Reading a redirected stdin here would consume a script's data and
            // then apply a change nobody confirmed, which is the failure this module exists to
            // prevent.
            return Answer::Refused;
        }
        #[cfg(test)]
        if let Some(seam) = self.seam {
            seam.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Answer::from_word(seam.line);
        }
        let mut line = String::new();
        match std::io::stdin().lock().read_line(&mut line) {
            // EOF (0 bytes read) is not "yes": it is nobody there.
            Ok(0) | Err(_) => Answer::Refused,
            Ok(_) => Answer::from_word(&line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads() -> &'static std::sync::atomic::AtomicUsize {
        Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(0)))
    }

    /// A `Stdin` whose terminal is `interaction` and whose line is `line`.
    fn stdin(interaction: Interaction, line: &'static str) -> (Stdin, &'static AtomicUsize) {
        let reads = reads();
        (
            Stdin {
                _private: (),
                seam: Some(TestSeam {
                    interaction,
                    line,
                    reads,
                }),
            },
            reads,
        )
    }

    use std::sync::atomic::AtomicUsize;

    /// The re-check in [`Confirmer::confirm`] that the outer gate in `authorize` already covers.
    ///
    /// Defence in depth: `authorize` refuses before a confirmer is ever asked, so this branch is
    /// unreachable through the real command path. That is the point — it is the layer that holds
    /// if a future caller reaches for a confirmer directly. It is also, therefore, exactly the
    /// layer nobody tests, and it could be deleted with the suite still green.
    ///
    /// The seam's line is `y`. If the guard is ever removed, that line is read and parsed into
    /// [`Answer::Yes`] — so this test goes red on the mutation, which is what makes it worth
    /// having. Asserting only `Refused` would not: falling through to a real `read_line` on the
    /// test binary's stdin gives `Ok(0)`, which is also `Refused`.
    #[test]
    fn confirm_refuses_without_reading_when_nobody_is_there() {
        let (mut c, reads) = stdin(Interaction::NonInteractive, "y");

        assert!(!c.may_decide(), "this seam has nobody to ask");
        assert_eq!(
            c.confirm("Proceed?"),
            Answer::Refused,
            "a confirmer that may not decide must not answer, even with a yes sitting on stdin"
        );
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the answer source was consulted: the guard exists precisely so a redirected stdin \
             is not consumed for an apply nobody confirmed"
        );
        assert!(!c.confirm("Proceed?").granted());
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "asking again must not read either"
        );
    }

    /// The complement: with somebody there, the answer source *is* consulted.
    ///
    /// Without this the guard test above would also pass if `confirm` returned `Refused`
    /// unconditionally — a confirmer that can never say yes is not a gate, it is a wall.
    #[test]
    fn confirm_reads_the_answer_when_somebody_is_there() {
        for (line, expected) in [
            ("y", Answer::Yes),
            ("yes", Answer::Yes),
            ("n", Answer::No),
            ("maybe", Answer::No),
        ] {
            let (mut c, reads) = stdin(Interaction::Interactive, line);
            assert!(c.may_decide());
            assert_eq!(c.confirm("Proceed?"), expected, "for line {line:?}");
            assert_eq!(
                reads.load(std::sync::atomic::Ordering::Relaxed),
                1,
                "an interactive confirmer must consult its answer source exactly once"
            );
        }
    }

    /// `Stdin::may_decide` is the interaction's, not its own opinion.
    ///
    /// The production path builds this struct with no seam, so this pins the one place the two
    /// halves of the gate are bridged and they cannot be swapped independently.
    #[test]
    fn may_decide_is_the_interactions_answer() {
        let (c, _) = stdin(Interaction::Interactive, "y");
        assert!(c.may_decide());
        assert!(!stdin(Interaction::NonInteractive, "y").0.may_decide());
    }
}

/// `--yes`: the decision was already made on the command line.
///
/// `may_decide` is `true` and `will_ask` is `false`: the operator said so before the program
/// started, so a piped stdin, a cron job and a terminal all behave the same, which is what makes
/// `--yes` usable from a script.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysYes {
    _private: (),
}

impl AlwaysYes {
    /// A confirmer that always says yes.
    pub fn new() -> AlwaysYes {
        AlwaysYes { _private: () }
    }
}

impl Confirmer for AlwaysYes {
    fn may_decide(&self) -> bool {
        true
    }

    fn will_ask(&self) -> bool {
        false
    }

    fn confirm(&mut self, _question: &str) -> Answer {
        Answer::Yes
    }
}

/// Nobody to ask: no terminal, or a test standing in for one.
///
/// The single confirmer that answers **without** `--yes` and without a terminal, which is what makes
/// "refuse when nobody can answer" a value rather than a property of the machine running the test.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOne {
    _private: (),
}

impl NoOne {
    /// A confirmer that refuses, having asked nobody.
    pub fn new() -> NoOne {
        NoOne { _private: () }
    }
}

impl Confirmer for NoOne {
    fn may_decide(&self) -> bool {
        false
    }

    fn confirm(&mut self, _question: &str) -> Answer {
        // Unreachable through `authorize`, and answering `Refused` rather than `Yes` means a
        // future caller that forgets to check `may_decide` still refuses.
        Answer::Refused
    }
}

/// A hostile double: **may not decide, but would say yes anyway**.
///
/// [`Confirmer::may_decide`] is `false` — there is no terminal and nobody to ask — while
/// [`Confirmer::confirm`] answers `Yes` regardless. No shipped confirmer behaves this way: [`Stdin`]
/// and [`NoOne`] both answer `Refused`, and [`AlwaysYes`] is the one that says yes, with `--yes`
/// behind it. It exists to pin one property: **the refusal is the CLI's decision**, made from
/// `may_decide`, and not something the confirmer happens to return.
///
/// Without it a test double that fails closed would make the `may_decide` gate removable without
/// any test going red — which is exactly the kind of test that passes for the wrong reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct WavesItThrough {
    _private: (),
}

impl WavesItThrough {
    /// The double: says yes to everything, even with nobody there to ask.
    pub fn new() -> WavesItThrough {
        WavesItThrough { _private: () }
    }
}

impl Confirmer for WavesItThrough {
    fn may_decide(&self) -> bool {
        false
    }

    fn confirm(&mut self, _question: &str) -> Answer {
        Answer::Yes
    }
}

/// A scripted answer from somebody who is there.
///
/// Fails closed in two separate ways, both of which the specs rely on: an **exhausted** script
/// answers no rather than panicking, and a script built for a [`Interaction::NonInteractive`] run
/// answers [`may_decide`] `false`. The earlier version of this type panicked when the script ran
/// out, which put a `panic!` in non-test code — forbidden by this crate's lints, and wrong here: for
/// the question "did a person consent", a fail-closed default is the correct shape even when the
/// spec that scripted it is the thing that is broken. The mistake is still caught, by
/// [`ScriptedConfirmer::asked`], which the specs compare against the number of answers they
/// scripted: too few *or* too many questions both fail the assertion, and neither fails by
/// silently proceeding.
#[derive(Debug, Default)]
pub struct ScriptedConfirmer {
    answers: VecDeque<bool>,
    asked: Vec<String>,
    may_decide: bool,
}

impl ScriptedConfirmer {
    /// Answer `answers` in the order they are asked for, from somebody who is there.
    pub fn new(answers: impl IntoIterator<Item = bool>) -> ScriptedConfirmer {
        ScriptedConfirmer::attended(Interaction::Interactive, answers)
    }

    /// The same, for a stated environment.
    ///
    /// `interaction` is the spec's statement about *this run*: it becomes the answer to
    /// "can anybody be asked", which is the condition the gate refuses on. It is data, exactly as
    /// the scripted answers are.
    pub fn attended(
        interaction: Interaction,
        answers: impl IntoIterator<Item = bool>,
    ) -> ScriptedConfirmer {
        ScriptedConfirmer {
            answers: answers.into_iter().collect(),
            asked: Vec::new(),
            may_decide: interaction.may_decide(),
        }
    }

    /// The same script, for an environment where nobody can be asked.
    ///
    /// The scripted answers are kept so a spec can still say "this question would have been answered
    /// yes", while `may_decide` is what actually decides — which is the property
    /// `cli2_c03` and `cli2_the_refusal_survives_a_confirmer_that_would_say_yes` rest on.
    pub fn unattended(interaction: Interaction) -> ScriptedConfirmer {
        ScriptedConfirmer {
            answers: VecDeque::new(),
            asked: Vec::new(),
            may_decide: interaction.may_decide(),
        }
    }

    /// Whether this script claims somebody can be asked.
    pub fn may_decide(&self) -> bool {
        self.may_decide
    }

    /// How many questions are left, so a spec can assert a question was **not** asked.
    pub fn remaining(&self) -> usize {
        self.answers.len()
    }

    /// How many questions were actually put — scripted and unscripted alike.
    ///
    /// The check that catches a command asking more questions than the spec anticipated: it
    /// answers the extra one "no" and this count comes back larger than the script.
    pub fn asked(&self) -> usize {
        self.asked.len()
    }

    /// The exact questions that were put, in order.
    ///
    /// Kept so a spec can assert *what* was asked and not only how often — "the person was asked
    /// something" is a much weaker claim than "the person was asked whether to apply this plan".
    pub fn prompts(&self) -> &[String] {
        &self.asked
    }
}

impl Confirmer for ScriptedConfirmer {
    fn may_decide(&self) -> bool {
        self.may_decide
    }

    fn confirm(&mut self, prompt: &str) -> Answer {
        self.asked.push(prompt.to_string());
        // Unscripted: no. See the type's docs.
        match self.answers.pop_front() {
            Some(true) => Answer::Yes,
            Some(false) => Answer::No,
            None => Answer::Refused,
        }
    }
}

/// A [`Confirmer`] that answers the same thing to every question, and remembers what it was asked.
///
/// [`ScriptedConfirmer`] for one write; this one for "ask once, and prove you were asked once".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    answer: Answer,
    question: Option<String>,
    times: u32,
}

impl Answered {
    /// A confirmer that answers `answer` and remembers what it was asked.
    pub fn new(answer: Answer) -> Answered {
        Answered {
            answer,
            question: None,
            times: 0,
        }
    }

    /// How many times this was asked.
    pub fn times_asked(&self) -> u32 {
        self.times
    }

    /// The question it was asked, which is the prompt a person would have seen.
    pub fn last_question(&self) -> Option<&str> {
        self.question.as_deref()
    }
}

impl Confirmer for Answered {
    fn may_decide(&self) -> bool {
        true
    }

    fn confirm(&mut self, question: &str) -> Answer {
        self.times += 1;
        self.question = Some(question.to_string());
        self.answer
    }
}

/// The question `apply` asks, without the plan id. Pinned so the docs, the help text and the specs
/// cannot drift.
pub const APPLY_PROMPT: &str = "Apply this plan to the files listed above?";

/// The question `undo` asks, without the plan id.
pub const UNDO_PROMPT: &str = "Undo this plan, restoring the files listed above?";

/// The question `recover` asks. It names no plan, because there is no single plan to name.
pub const RECOVER_PROMPT: &str = "Converge every half-applied plan in this workspace?";

/// The question for `plan_id`, as a person sees it.
///
/// The id is in the prompt because a person answering "yes" has to know *which* plan they said yes
/// to, and a terminal can hold more than one workspace's plans in view. It goes through [`Out`]
/// like every other line, so an id that could carry an escape sequence is escaped before it is
/// printed.
pub fn question(base: &'static str, plan_id: Option<&str>) -> String {
    match plan_id {
        Some(id) => format!("{base} [{id}] (y/N) "),
        None => format!("{base} (y/N) "),
    }
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// A person said so, or said so in advance with `--yes`. The caller may now write.
    Proceed {
        /// Whether a question was actually put to a person. `false` when `--yes` answered it.
        asked: bool,
    },
    /// The gate said no. The caller must not write anything.
    Refused(Refusal),
}

/// Which write is asking, and for what.
///
/// A parameter rather than three separate entry points, so `apply` and `undo` cannot drift apart in
/// their confirmation: they differ in a verb and in where the file list came from, which the caller
/// has already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `edit apply`.
    Apply,
    /// `edit undo`.
    Undo,
    /// `edit recover`: no plan id, and no single file list.
    Recover,
}

impl Op {
    /// The verb a person reads in the prompt and in the refusal.
    pub fn subject(self) -> &'static str {
        match self {
            Op::Apply => "apply",
            Op::Undo => "undo",
            Op::Recover => "recover",
        }
    }

    /// The question, without the plan id.
    pub fn prompt(self) -> &'static str {
        match self {
            Op::Apply => APPLY_PROMPT,
            Op::Undo => UNDO_PROMPT,
            Op::Recover => RECOVER_PROMPT,
        }
    }
}

/// What a write would change, and whether `--yes` was already given.
///
/// Built by the command, which is the only thing that knows the plan id and the file list; consumed
/// by [`authorize`], which is the only thing that decides.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Which write this is.
    pub op: Op,
    /// The plan id, when there is one.
    pub plan_id: Option<&'a str>,
    /// The files that will change. Resolved **before** the question, from the same store the write
    /// will use, so the list on screen and the files touched are the same list read at the same
    /// moment.
    pub files: &'a [String],
    /// The `--yes` flag: consent given in advance, by a person who typed it.
    pub yes: bool,
}

impl<'a> Request<'a> {
    /// A write of a named plan over a known list of files.
    pub fn of(op: Op, plan_id: &'a str, files: &'a [String], yes: bool) -> Request<'a> {
        Request {
            op,
            plan_id: Some(plan_id),
            files,
            yes,
        }
    }

    /// `edit recover`: the one write with no plan and no single file list.
    pub fn recover(yes: bool) -> Request<'a> {
        Request {
            op: Op::Recover,
            plan_id: None,
            files: &[],
            yes,
        }
    }
}

/// A refusal, in the shape the rest of the CLI reports errors in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// Always [`ErrorCode::InvalidArgs`] — pinned by [`refusal_code`], not free-form.
    pub code: ErrorCode,
    /// What happened, in one sentence a person can read.
    pub message: String,
    /// The next step. This is the contract that makes the refusal useful rather than merely safe: a
    /// refusal that does not say what to do gets worked around with `--yes` by reflex.
    pub next: String,
}

impl Refusal {
    /// The refusal as a [`ToolError`], so one reporting path prints codes, messages and next steps.
    pub fn to_error(&self) -> ToolError {
        ToolError::new(self.code, self.message.clone(), self.next.clone())
    }

    /// The process exit code this refusal maps to: 1, a user error.
    ///
    /// Asked of the shared [`exit::exit_code_for`] rather than restated, so the CLI cannot have a
    /// refusal exit code that `--help` does not document.
    pub fn exit_code(&self) -> i32 {
        exit::exit_code_for(self.code)
    }
}

/// The code a refusal always carries.
///
/// A function rather than a bare constant so the specs can assert the code is `invalid_args` **and**
/// that it maps to exit 1 through the shared [`exit::exit_code_for`].
pub fn refusal_code() -> ErrorCode {
    ErrorCode::InvalidArgs
}

/// The next step every refusal gives. One string, so a refusal cannot word its own advice.
pub const REFUSAL_NEXT: &str =
    "run `opencrayast edit apply <plan-id> --yes` from a shell where you have read the diff";

/// Decide whether a write may happen.
///
/// Prints what will change and the question to `out` when it is going to ask one, then returns
/// [`Decision::Proceed`] or [`Decision::Refused`]. It performs no I/O beyond those lines and calls
/// nothing that writes.
///
/// | `may_decide` | `--yes` | What happens |
/// |---|---|---|
/// | yes | yes | proceeds, asking nobody |
/// | yes | no | the file list and the prompt are printed; the person's answer decides |
/// | no | yes | proceeds |
/// | no | no | **refuses**, asking nobody |
///
/// The last row is the acceptance criterion, and the refusal is taken from `may_decide` **without
/// consulting the confirmer** — which is what [`WavesItThrough`] pins.
pub fn authorize(out: &mut Out<'_>, req: &Request<'_>, confirmer: &mut dyn Confirmer) -> Decision {
    let subject = req.op.subject();

    // Consent given in advance. It answers the question in both environments, so it is checked
    // first: asking a person something they have already answered is the kind of thing that gets a
    // script pressing enter on a prompt it never reads.
    if req.yes {
        return Decision::Proceed { asked: false };
    }

    // What will change is printed in **both** branches, before anything is decided. It is the answer
    // to "what would this do to me?", and a person deciding — or a log being read afterwards — is
    // entitled to it whichever way the run ends. The *question* is printed only when somebody can
    // answer it.
    announce(out, req, confirmer);

    // Nobody to ask. This is the criterion named above, and it refuses from `may_decide`
    // **without consulting the confirmer** — asking it anyway would let a confirmer that answers
    // `Yes` with nobody present authorise a write. `WavesItThrough` is exactly that double.
    if !confirmer.may_decide() {
        return refuse(out, subject, RefusalKind::NobodyToAsk);
    }

    if confirmer
        .confirm(&req.op.prompt_question(req.plan_id))
        .granted()
    {
        return Decision::Proceed { asked: true };
    }
    refuse(out, subject, RefusalKind::Declined)
}

impl Op {
    /// The question as it is put to a person: the pinned sentence, then the plan id.
    fn prompt_question(self, plan_id: Option<&str>) -> String {
        question(self.prompt(), plan_id)
    }
}

/// Print exactly what is about to change, then the question — if somebody can be asked it.
///
/// Both go through [`Out::line`], so a path or a plan id containing ESC, a bidi override or a
/// zero-width character is escaped like every other line this CLI prints. The file list is the most
/// attacker-influenced text in the whole confirmation flow.
///
/// The **file list** is always printed: it is the answer to "what would this do to me?", and it is
/// needed whether or not the run ends in a write. The **question** is printed only when somebody
/// can answer it; under `--yes` the decision was made on the command line, and printing a `(y/N)`
/// that no terminal will ever read would misrepresent how the run is driven.
fn announce(out: &mut Out<'_>, req: &Request<'_>, confirmer: &dyn Confirmer) {
    out.line(&format!(
        "This {} changes {} file(s):",
        req.op.subject(),
        req.files.len()
    ));
    for f in req.files {
        out.line(&format!("  {f}"));
    }
    if confirmer.will_ask() {
        out.line(&req.op.prompt_question(req.plan_id));
    }
}

/// Which of the two refusals this is. They share a code and an exit status — they are the same kind
/// of event — but a person reading the terminal needs to know which happened, and "one of the two
/// noes" is not a usable message.
#[derive(Debug, Clone, Copy)]
enum RefusalKind {
    /// `--yes` was not given and there was nobody to ask.
    NobodyToAsk,
    /// Somebody was asked and said no.
    Declined,
}

impl RefusalKind {
    /// What happened. Both sentences contain `nothing confirmed it`, which is the one claim both
    /// make and the one a caller can grep for.
    fn message(self, subject: &str) -> String {
        match self {
            RefusalKind::NobodyToAsk => format!(
                "{subject} needs confirmation and nothing confirmed it: this is a non-interactive \
                 environment, there is nobody to ask, and --yes was not given."
            ),
            RefusalKind::Declined => format!(
                "{subject} was declined at the confirmation prompt, so nothing confirmed it."
            ),
        }
    }
}

/// Print a refusal in the CLI's shape — the literal code, what happened, what to do — and build it.
fn refuse(out: &mut Out<'_>, subject: &str, kind: RefusalKind) -> Decision {
    let message = kind.message(subject);
    out.diag(&format!("[{}] {message}", refusal_code().as_str()));
    out.diag(&format!(
        "Next: {REFUSAL_NEXT}. Nothing was written; the plan is still in the store, so it can be \
         reviewed and applied later."
    ));
    Decision::Refused(Refusal {
        code: refusal_code(),
        message,
        next: REFUSAL_NEXT.to_string(),
    })
}
