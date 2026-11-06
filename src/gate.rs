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
//! | the composer is empty | no terminal input since the last prompt was submitted (the last turn's start, else the process's start): what was typed before it went into that prompt | `draft-unproven` |
//! | the turn has settled | the last turn ended at least `settle` ago | `turn-settling` |
//!
//! The composer witness needs no screen: a draft can only come from terminal input, and
//! the terminal's access time is never earlier than the last read of input
//! ([`crate::tty`]). It holds whenever that clock is later than the last submission — a
//! key, a paste, a mouse or focus report, a terminal query's answer, or a submitting key
//! whose read the kernel timed lazily (one in the same wall-clock second as the read
//! before it) — even when the composer is in fact empty; then the restart waits for the
//! next turn, or for the session's end (the next launch starts the newest build anyway).
//! A prompt that arrives in one read (a paste, or aterm typing it) is timed exactly. aterm's
//! screen reader (contract v3, [`crate::aterm`]) is the witness that will lift the rest.

use crate::muse::log::{Open, Quiet};

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

/// Whether the turn ended, the composer is empty and the turn has settled.
///
/// # Errors
/// The log's word, `draft-unproven` or `turn-settling`.
pub fn check_log(
    log: &Result<Quiet, Open>,
    last_input_us: Option<u64>,
    now_us: u64,
    timing: &Timing,
) -> Result<(), &'static str> {
    let quiet = log.as_ref().map_err(|o| o.word())?;
    let last = last_input_us.ok_or("no-input-clock")?;
    if last > quiet.submitted_us {
        return Err("draft-unproven");
    }
    if now_us.saturating_sub(quiet.ended_us) < timing.settle_us || quiet.ended_us > now_us {
        return Err("turn-settling");
    }
    Ok(())
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
    check_log(
        &l.log,
        l.terminal.and_then(|t| t.last_input_us),
        l.now_us,
        timing,
    )
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
}
