// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SAFE POINT: whether a running child that is behind a newer build may be stopped
//! now and resumed on that build. The user's ruling names the safe point: the turn ended,
//! the composer is empty, no person is typing. Every witness must be read and must agree;
//! anything unread or unknown holds (fail closed). A busy child is never stopped.
//!
//! | check | witness | holds with |
//! |---|---|---|
//! | the launch can be resumed | Muse Code's resume grammar ([`crate::muse::argv`]) | `not-resumable` |
//! | this job owns the terminal | `tcgetpgrp` ([`crate::tty`]) | `no-tty`, `not-foreground` |
//! | no person is typing | the terminal's input clock ([`crate::tty`]) is at least `person_grace` old | `person-typing`, `no-input-clock` |
//! | no typing waits unread | the terminal's input queue is empty (`FIONREAD`): keys a busy child has not read yet never moved the clock | `input-pending`, `no-input-queue` |
//! | no tool is running | the child has no descendant processes ([`crate::procs`]) | `child-process`, `process-table` |
//! | the session is known | exactly one session lock naming the child, written since it started ([`crate::muse::session::witness`]) | `no-session` |
//! | the turn ended | Muse Code's own session log: no open run, task or queued item in this process's part, nor in its subagents' logs ([`crate::muse::log`], aterm's G-M1/G-M2) | `turn-open`, `open-task`, `background-work`, `queued-work`, `log-unproved`, `log-unreadable` |
//! | the turn has settled | the last turn ended at least `settle` ago | `turn-settling` |
//! | the composer is empty | aterm's screen reader where aterm serves it (`status composer=`, read fresh at the act, [`crate::aterm::move_unheld`]), else no terminal input since the last prompt was submitted (the last turn's start, else the process's start) | `draft`, `composer-unproven`; `draft-unproven` |
//!
//! THE COMPOSER WITNESS ([`check_composer`]) is aterm's word where aterm serves it: its
//! reader of the identified program says `empty`, `placeholder` (a tip or a suggestion, no
//! person's text), `draft`, or `-` (no composer drawn, or its text not proven), and only the
//! first two let the move go. An aterm that does not serve `composer=` (one that predates it,
//! or none at all outside aterm) leaves the terminal's input clock, which needs no screen: a
//! draft can only come from terminal input, and the terminal's access time is never earlier
//! than the last read of input ([`crate::tty`]). That clock holds whenever it is later than
//! the last submission — a key, a paste, a mouse or focus report, a terminal query's answer,
//! or a submitting key whose read the kernel timed lazily (one in the same wall-clock second
//! as the read before it) — even when the composer is in fact empty; then the restart waits
//! for the next turn, or for the session's end (the next launch starts the newest build
//! anyway). A prompt that arrives in one read (a paste, or aterm typing it) is timed exactly.

use crate::muse::log::{Open, Quiet};

/// What aterm's screen reader says the session's composer holds (`status composer=`, contract
/// 3's §8.1 item 9 in aterm's harness design): read off the screen by the reader of the program
/// aterm identified, so the harness judges a safe point without a screen reader of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Composer {
    /// `composer=empty`: no typed or placeholder text.
    Empty,
    /// `composer=placeholder`: placeholder text (a tip or a suggestion), not a person's draft.
    Placeholder,
    /// `composer=draft`: a person's unsent draft.
    Draft,
    /// `composer=-`, or a word this harness does not know: no composer drawn, or its text not
    /// proven either way.
    Unproven,
}

impl Composer {
    /// The word `composer=` carries.
    #[must_use]
    pub fn parse(word: &str) -> Composer {
        match word {
            "empty" => Composer::Empty,
            "placeholder" => Composer::Placeholder,
            "draft" => Composer::Draft,
            _ => Composer::Unproven,
        }
    }
}

/// The safe point's clocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// How long the terminal must have been without input (aterm's `human_grace_s`
    /// default for its own quiet restart is 120 s).
    pub person_grace_us: u64,
    /// How long after the turn ended the restart waits (the person, or aterm's Autopilot,
    /// gets the first move after a turn).
    pub settle_us: u64,
}

impl Timing {
    /// The production clocks.
    pub const PRODUCTION: Timing = Timing {
        person_grace_us: 120_000_000,
        settle_us: 30_000_000,
    };
}

/// The terminal, read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    /// When it last delivered input (µs since the epoch; `None`: unreadable).
    pub last_input_us: Option<u64>,
    /// Whether this job is its foreground (`None`: unreadable).
    pub foreground: Option<bool>,
    /// How many bytes of input wait unread in its input queue (`None`: unreadable).
    pub pending_input: Option<usize>,
}

/// One look at a running child that is behind a newer build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    /// Its argv resumes (`Err`: the refusal).
    pub resumable: Result<(), String>,
    /// The session it holds, witnessed.
    pub session: Option<String>,
    /// The terminal (`None`: there is none).
    pub terminal: Option<Terminal>,
    /// How many descendants it has (`None`: unreadable).
    pub children: Option<usize>,
    /// What its session's logs say.
    pub log: Result<Quiet, Open>,
    /// aterm's `composer=`, where aterm serves it.
    pub composer: Option<Composer>,
    /// Now (µs since the epoch).
    pub now_us: u64,
}

/// Whether the launch's argv can be resumed.
///
/// # Errors
/// `not-resumable`.
pub fn check_argv(resumable: &Result<(), String>) -> Result<(), &'static str> {
    resumable.as_ref().map(|_| ()).map_err(|_| "not-resumable")
}

/// Whether the session the child holds is witnessed.
///
/// # Errors
/// `no-session`.
pub fn check_session(session: Option<&str>) -> Result<(), &'static str> {
    session.map(|_| ()).ok_or("no-session")
}

/// Whether the job owns the terminal and no person is typing.
///
/// # Errors
/// `no-tty`, `not-foreground`, `no-input-clock`, `person-typing`, `no-input-queue` or
/// `input-pending`.
pub fn check_terminal(
    t: Option<Terminal>,
    now_us: u64,
    timing: &Timing,
) -> Result<(), &'static str> {
    let t = t.ok_or("no-tty")?;
    if t.foreground != Some(true) {
        return Err("not-foreground");
    }
    let last = t.last_input_us.ok_or("no-input-clock")?;
    if now_us.saturating_sub(last) < timing.person_grace_us || last > now_us {
        return Err("person-typing");
    }
    match t.pending_input {
        None => Err("no-input-queue"),
        Some(0) => Ok(()),
        Some(_) => Err("input-pending"),
    }
}

/// Whether no tool runs under the child.
///
/// # Errors
/// `process-table` or `child-process`.
pub fn check_children(children: Option<usize>) -> Result<(), &'static str> {
    match children {
        None => Err("process-table"),
        Some(0) => Ok(()),
        Some(_) => Err("child-process"),
    }
}

/// Whether the turn ended and has settled: the last submission it read (µs since the epoch),
/// which the input clock's composer witness is judged against ([`ComposerWitness::InputClock`]).
///
/// # Errors
/// The log's word or `turn-settling`.
pub fn check_log(
    log: &Result<Quiet, Open>,
    now_us: u64,
    timing: &Timing,
) -> Result<u64, &'static str> {
    let quiet = log.as_ref().map_err(|o| o.word())?;
    if now_us.saturating_sub(quiet.ended_us) < timing.settle_us || quiet.ended_us > now_us {
        return Err("turn-settling");
    }
    Ok(quiet.submitted_us)
}

/// What says the composer holds no person's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerWitness {
    /// aterm's screen reader (`status composer=`), where aterm serves it.
    Aterm(Composer),
    /// The terminal's input clock against the last submission: no input since it, so nothing
    /// typed waits in the composer ([`crate::tty`]).
    InputClock {
        /// When the terminal last delivered input (`None`: unreadable).
        last_input_us: Option<u64>,
        /// When the last prompt was submitted (the last turn's start, else the process's).
        submitted_us: u64,
    },
}

impl ComposerWitness {
    /// aterm's word where aterm serves one (`served`), else the input clock `local`.
    #[must_use]
    pub fn of(served: Option<Composer>, local: ComposerWitness) -> ComposerWitness {
        served.map_or(local, ComposerWitness::Aterm)
    }
}

/// Whether the composer holds no person's text, by `witness`: aterm's `empty` or
/// `placeholder`, else no terminal input since the last submission.
///
/// # Errors
/// `draft` or `composer-unproven` (aterm's word), `no-input-clock` or `draft-unproven` (the
/// input clock).
pub fn check_composer(witness: ComposerWitness) -> Result<(), &'static str> {
    match witness {
        ComposerWitness::Aterm(Composer::Empty | Composer::Placeholder) => Ok(()),
        ComposerWitness::Aterm(Composer::Draft) => Err("draft"),
        ComposerWitness::Aterm(Composer::Unproven) => Err("composer-unproven"),
        ComposerWitness::InputClock {
            last_input_us,
            submitted_us,
        } => match last_input_us {
            None => Err("no-input-clock"),
            Some(last) if last > submitted_us => Err("draft-unproven"),
            Some(_) => Ok(()),
        },
    }
}

/// THE GATE: every check, in the order the live loop reads them (cheapest first).
///
/// # Errors
/// The first check's hold word.
pub fn gate(l: &Look, timing: &Timing) -> Result<(), &'static str> {
    check_argv(&l.resumable)?;
    check_terminal(l.terminal, l.now_us, timing)?;
    check_children(l.children)?;
    check_session(l.session.as_deref())?;
    let submitted_us = check_log(&l.log, l.now_us, timing)?;
    check_composer(ComposerWitness::of(
        l.composer,
        ComposerWitness::InputClock {
            last_input_us: l.terminal.and_then(|t| t.last_input_us),
            submitted_us,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_284_625_000_000;
    const T: Timing = Timing::PRODUCTION;

    fn quiet_look() -> Look {
        Look {
            resumable: Ok(()),
            session: Some("01a148f8-9301-7752-96db-fef456f88074".to_string()),
            terminal: Some(Terminal {
                last_input_us: Some(NOW - 600_000_000),
                foreground: Some(true),
                pending_input: Some(0),
            }),
            children: Some(0),
            log: Ok(Quiet {
                from_us: NOW - 900_000_000,
                submitted_us: NOW - 599_000_000,
                ended_us: NOW - 60_000_000,
            }),
            composer: None,
            now_us: NOW,
        }
    }

    #[test]
    fn a_quiet_point_goes() {
        assert_eq!(gate(&quiet_look(), &T), Ok(()));
    }

    #[test]
    fn the_gate_names_each_hold() {
        let mut cases: Vec<(Look, &str)> = Vec::new();
        let mut l = quiet_look();
        l.resumable = Err("not-resumable --workspace".to_string());
        cases.push((l, "not-resumable"));
        let mut l = quiet_look();
        l.session = None;
        cases.push((l, "no-session"));
        let mut l = quiet_look();
        l.terminal = None;
        cases.push((l, "no-tty"));
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: Some(NOW - 600_000_000),
            foreground: Some(false),
            pending_input: Some(0),
        });
        cases.push((l, "not-foreground"));
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: None,
            foreground: Some(true),
            pending_input: Some(0),
        });
        cases.push((l, "no-input-clock"));
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: Some(NOW - 5_000_000),
            foreground: Some(true),
            pending_input: Some(0),
        });
        cases.push((l, "person-typing"));
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: Some(NOW - 600_000_000),
            foreground: Some(true),
            pending_input: Some(2),
        });
        cases.push((l, "input-pending"));
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: Some(NOW - 600_000_000),
            foreground: Some(true),
            pending_input: None,
        });
        cases.push((l, "no-input-queue"));
        let mut l = quiet_look();
        l.children = Some(1);
        cases.push((l, "child-process"));
        let mut l = quiet_look();
        l.children = None;
        cases.push((l, "process-table"));
        for open in [
            Open::Turn,
            Open::Unproved,
            Open::Unreadable,
            Open::Queued,
            Open::Task { background: true },
            Open::Task { background: false },
        ] {
            let mut l = quiet_look();
            l.log = Err(open);
            cases.push((l, open.word()));
        }
        let mut l = quiet_look();
        l.log = Ok(Quiet {
            from_us: NOW - 900_000_000,
            submitted_us: NOW - 700_000_000,
            ended_us: NOW - 60_000_000,
        });
        cases.push((l, "draft-unproven"));
        let mut l = quiet_look();
        l.log = Ok(Quiet {
            from_us: NOW - 900_000_000,
            submitted_us: NOW - 599_000_000,
            ended_us: NOW - 10_000_000,
        });
        cases.push((l, "turn-settling"));
        for (word, composer) in [
            ("draft", Composer::Draft),
            ("composer-unproven", Composer::Unproven),
        ] {
            let mut l = quiet_look();
            l.composer = Some(composer);
            cases.push((l, word));
        }
        for (look, word) in cases {
            assert_eq!(gate(&look, &T), Err(word), "{look:?}");
        }
    }

    #[test]
    fn input_exactly_at_the_submission_is_the_submission() {
        let mut l = quiet_look();
        if let Ok(q) = &mut l.log {
            q.submitted_us = NOW - 600_000_000;
        }
        assert_eq!(gate(&l, &T), Ok(()));
    }

    #[test]
    fn a_clock_in_the_future_holds() {
        let mut l = quiet_look();
        l.terminal = Some(Terminal {
            last_input_us: Some(NOW + 1),
            foreground: Some(true),
            pending_input: Some(0),
        });
        assert_eq!(gate(&l, &T), Err("person-typing"));
        let mut l = quiet_look();
        if let Ok(q) = &mut l.log {
            q.ended_us = NOW + 1;
        }
        assert_eq!(gate(&l, &T), Err("turn-settling"));
    }

    /// THE COMPOSER IS aterm's WORD WHERE aterm SERVES ONE: its `empty` or `placeholder` lets a
    /// move go though the input clock is later than the last submission (input after it that
    /// typed no draft, which that clock cannot tell from one), and its `draft` or `-` holds one
    /// the input clock would let go. With no word served, the input clock judges.
    #[test]
    fn the_composer_is_aterms_word_where_aterm_serves_one() {
        let mut clock_holds = quiet_look();
        if let Ok(q) = &mut clock_holds.log {
            q.submitted_us = NOW - 700_000_000;
        }
        assert_eq!(gate(&clock_holds, &T), Err("draft-unproven"));
        for composer in [Composer::Empty, Composer::Placeholder] {
            let mut l = clock_holds.clone();
            l.composer = Some(composer);
            assert_eq!(gate(&l, &T), Ok(()), "{composer:?}");
        }
        for (composer, word) in [
            (Composer::Draft, "draft"),
            (Composer::Unproven, "composer-unproven"),
        ] {
            let mut l = quiet_look();
            l.composer = Some(composer);
            assert_eq!(gate(&l, &T), Err(word), "{composer:?}");
        }
        assert_eq!(
            check_composer(ComposerWitness::of(
                None,
                ComposerWitness::InputClock {
                    last_input_us: None,
                    submitted_us: NOW
                }
            )),
            Err("no-input-clock")
        );
    }
}
