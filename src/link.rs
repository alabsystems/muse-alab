// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HARNESS'S STANDING WITH ATERM, held by the loop ([`crate::supervise`]) for the
//! harness's whole life: no aterm at all, a proven contract 3 claim, a claim given back while
//! aterm's policy is off, or a claim whose connection failed.
//!
//! Under a claim every act (a crash relaunch, a move's `SIGTERM`) waits on ONE FRESH read of
//! aterm, never the policy the claim was made under ([`judge`]): the claim must still be this
//! harness's and proven, the policy on for the session with the duty's own switch
//! (`[harness] relaunch`, `upgrade`) on, and, for a move, no person may have typed within the
//! person grace by aterm's `human_ms`. A claim aterm no longer shows is made again; a policy
//! found off gives the claim back, and it is made again once the policy is back on (read on a
//! clock, [`crate::aterm::RECLAIM_EVERY`]). A move re-announces the claim with the new build's
//! `vendor=`.

use std::time::{Duration, Instant};

use crate::aterm::{self, Claimed, Locator, Policy, Screen};
use crate::journal::Journal;

/// The duty an act takes from aterm's built-in engine (the claim's scope words).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duty {
    /// A crash relaunch on the same session.
    Relaunch,
    /// A move onto a newer build at a safe point.
    Upgrade,
}

/// What one fresh read of aterm says of an act.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Act.
    Act,
    /// Hold, with the look's word and why.
    Hold(&'static str, String),
    /// The claim is not aterm's proven one any more: lose it and make it again.
    Lose(String),
    /// The policy is off for the session: give the claim back.
    Release(String),
}

/// Judge `duty` on one fresh read of aterm (`read`: the policy and `status`) for `holder`, with
/// the person grace `grace`. Anything unread holds.
#[must_use]
pub fn judge(
    read: &Result<(Policy, Screen), String>,
    holder: &str,
    duty: Duty,
    grace: Duration,
) -> Verdict {
    let (policy, screen) = match read {
        Ok(r) => r,
        Err(why) => return Verdict::Hold("aterm-unread", format!("no fresh read of aterm: {why}")),
    };
    if !screen.held_by(holder) {
        return Verdict::Lose(format!(
            "aterm does not show this harness's proven claim (supervisor={} supervisor_proven={})",
            screen.supervisor.as_deref().unwrap_or("-"),
            u8::from(screen.proven)
        ));
    }
    if !policy.on() {
        return Verdict::Release(if policy.serves(aterm::REQUIRED_CONTRACT) {
            format!(
                "aterm's harness policy is off for this session (why={})",
                policy.why
            )
        } else {
            format!(
                "aterm no longer serves harness contract {}",
                aterm::REQUIRED_CONTRACT
            )
        });
    }
    let (switch, word) = match duty {
        Duty::Relaunch => (policy.relaunch, "relaunch"),
        Duty::Upgrade => (policy.upgrade, "upgrade"),
    };
    match switch {
        Some(true) => {}
        Some(false) => return Verdict::Hold("duty-off", format!("[harness] {word} is off")),
        None => {
            return Verdict::Hold("duty-off", format!("the policy reports no {word} switch"));
        }
    }
    if duty == Duty::Upgrade && screen.human.within(grace) {
        return Verdict::Hold(
            "person-typing",
            format!(
                "aterm saw a person type within {} ms (human_ms)",
                grace.as_millis()
            ),
        );
    }
    Verdict::Act
}

/// The harness's standing with aterm.
#[derive(Debug)]
pub enum Link {
    /// Not in an aterm tab: the harness supervises with no claim, as it always has.
    Off,
    /// A proven claim, and when the policy was last read.
    Held(Box<Claimed>, Instant),
    /// The claim, given back while aterm's policy is off, and when the policy was last read.
    Released(Box<Claimed>, Instant),
    /// The claim's connection failed: made again at once, then every
    /// [`aterm::RECLAIM_EVERY`].
    Lost {
        /// Where the socket is found again.
        locator: Locator,
        /// When it was last tried.
        tried: Option<Instant>,
        /// The last reason it was not made, said once.
        why: Option<String>,
    },
}

impl Link {
    /// The standing a start sequence left: a proven claim, or none outside aterm.
    #[must_use]
    pub fn new(claim: Option<Claimed>) -> Link {
        claim.map_or(Link::Off, |c| Link::Held(Box::new(c), Instant::now()))
    }

    fn take(&mut self) -> Link {
        std::mem::replace(self, Link::Off)
    }

    fn lose(&mut self, why: &str, journal: &Journal) {
        let locator = match self {
            Link::Held(c, _) | Link::Released(c, _) => c.locator.clone(),
            Link::Off | Link::Lost { .. } => return,
        };
        journal.note(
            "LOST",
            &format!(
                "the claim's connection to aterm failed ({why}); it is made again now, then every {} s",
                aterm::RECLAIM_EVERY.as_secs()
            ),
        );
        *self = Link::Lost {
            locator,
            tried: None,
            why: None,
        };
    }

    /// The loop's watch over aterm, every tick: a lost claim is made again; a standing one
    /// notices a closed connection (aterm restarted, or a seamless update dropped it) now, and
    /// follows the policy, re-read every [`aterm::RECLAIM_EVERY`].
    pub fn tick(&mut self, vendor: &str, journal: &Journal) {
        match self {
            Link::Off => {}
            Link::Lost { .. } => self.retry(vendor, journal),
            Link::Held(c, read) | Link::Released(c, read) => {
                if c.conn.closed() {
                    self.lose("aterm closed the claim's connection", journal);
                } else if read.elapsed() >= aterm::RECLAIM_EVERY {
                    self.follow_policy(vendor, journal);
                }
            }
        }
    }

    /// Read `harness policy` again and follow it: a held claim is given back when the policy is
    /// off for the session, and a given-back one made again when it is back on.
    fn follow_policy(&mut self, vendor: &str, journal: &Journal) {
        let read = match self {
            Link::Held(c, read) | Link::Released(c, read) => {
                *read = Instant::now();
                c.refresh_policy().cloned()
            }
            Link::Off | Link::Lost { .. } => return,
        };
        let policy = match read {
            Ok(p) => p,
            Err(why) if why.contains("answered") => return,
            Err(why) => {
                self.lose(&why, journal);
                return;
            }
        };
        match (self.take(), policy.on()) {
            (Link::Held(c, read), false) => {
                let why = format!(
                    "aterm's harness policy is off for this session (why={})",
                    policy.why
                );
                self.release(c, read, &why, journal);
            }
            (Link::Released(mut c, read), true) => {
                match c.claim_again(std::process::id(), crate::HARNESS_VERSION, Some(vendor)) {
                    Ok(()) => {
                        journal.note(
                            "CLAIMED",
                            &format!(
                                "{} holds the session again under contract 3, scope={}: aterm's harness policy is back on",
                                c.holder,
                                aterm::SCOPE
                            ),
                        );
                        *self = Link::Held(c, read);
                    }
                    Err(why) => {
                        journal.note("UNCLAIMED", &format!("the claim was not made again: {why}"));
                        *self = Link::Released(c, read);
                    }
                }
            }
            (other, _) => *self = other,
        }
    }

    fn release(&mut self, mut c: Box<Claimed>, read: Instant, why: &str, journal: &Journal) {
        match c.release() {
            Ok(()) => {
                journal.note(
                    "RELEASED",
                    &format!(
                        "{} gave its claim back: {why}; nothing is relaunched or moved until it is back on",
                        c.holder
                    ),
                );
                *self = Link::Released(c, read);
            }
            Err(e) => {
                *self = Link::Held(c, read);
                self.lose(&format!("the claim could not be given back: {e}"), journal);
            }
        }
    }

    /// Make a lost claim again: at once after the loss, then every [`aterm::RECLAIM_EVERY`].
    fn retry(&mut self, vendor: &str, journal: &Journal) {
        let Link::Lost {
            locator,
            tried,
            why: last,
        } = self
        else {
            return;
        };
        if tried.is_some_and(|t| t.elapsed() < aterm::RECLAIM_EVERY) {
            return;
        }
        *tried = Some(Instant::now());
        match aterm::claim(
            locator,
            std::process::id(),
            crate::HARNESS_VERSION,
            Some(vendor),
            aterm::TIMEOUT,
        ) {
            Ok(c) => {
                journal.note(
                    "CLAIMED",
                    &format!(
                        "{} holds the session again under contract 3, scope={}",
                        c.holder,
                        aterm::SCOPE
                    ),
                );
                *self = Link::Held(Box::new(c), Instant::now());
            }
            // Each new reason once: a retry every few seconds must not fill the journal.
            Err(why) if last.as_deref() != Some(why.as_str()) => {
                journal.note("UNCLAIMED", &format!("the claim was not made again: {why}"));
                *last = Some(why);
            }
            Err(_) => {}
        }
    }

    /// Whether this harness holds a claim aterm proved (and has not given it back).
    #[must_use]
    pub fn is_held(&self) -> bool {
        matches!(self, Link::Held(..))
    }

    /// Whether `duty` may be done now, on one fresh read of aterm ([`judge`]); outside aterm
    /// always. A claim aterm no longer shows is made again at once and, when aterm proves it
    /// again, the act is judged once more on a fresh read under the new claim; a policy found off
    /// gives it back.
    ///
    /// # Errors
    /// The look's word and why the act waits.
    pub fn may_act(
        &mut self,
        duty: Duty,
        grace: Duration,
        vendor: &str,
        journal: &Journal,
    ) -> Result<(), (&'static str, String)> {
        let mut rejudge = true;
        loop {
            match self.judge_now(duty, grace, journal) {
                Verdict::Act => return Ok(()),
                Verdict::Hold(word, why) => return Err((word, why)),
                Verdict::Lose(why) => {
                    self.lose(&why, journal);
                    self.retry(vendor, journal);
                    if rejudge && self.is_held() {
                        rejudge = false;
                        continue;
                    }
                    return Err(("no-claim", why));
                }
                Verdict::Release(why) => {
                    if let Link::Held(c, read) = self.take() {
                        self.release(c, read, &why, journal);
                    }
                    return Err(("policy-off", why));
                }
            }
        }
    }

    /// One fresh read of aterm judged for `duty` ([`judge`]); a standing that holds no claim
    /// (outside aterm, lost, given back) answers without a read, and a connection that failed is
    /// lost here.
    fn judge_now(&mut self, duty: Duty, grace: Duration, journal: &Journal) -> Verdict {
        let (fresh, holder) = match self {
            Link::Off => return Verdict::Act,
            Link::Lost { .. } => {
                return Verdict::Hold(
                    "no-claim",
                    "this harness holds no claim now (its connection to aterm failed)".to_string(),
                );
            }
            Link::Released(..) => {
                return Verdict::Hold(
                    "policy-off",
                    "the claim was given back: aterm's harness policy is off for this session"
                        .to_string(),
                );
            }
            Link::Held(c, read) => {
                *read = Instant::now();
                let fresh = c
                    .refresh_policy()
                    .cloned()
                    .and_then(|p| c.screen().map(|s| (p, s)));
                (fresh, c.holder.clone())
            }
        };
        // A connection that failed is a lost claim; a reply aterm refused is an unread one.
        if let Err(why) = &fresh
            && !why.contains("answered")
        {
            let why = why.clone();
            self.lose(&why, journal);
            return Verdict::Hold("no-claim", format!("no fresh read of aterm: {why}"));
        }
        judge(&fresh, &holder, duty, grace)
    }

    /// Re-announce a held claim after a move onto `vendor` (its new `vendor=`).
    pub fn announce(&mut self, vendor: &str, journal: &Journal) {
        let Link::Held(c, _) = self else {
            return;
        };
        if let Err(why) = c.reclaim(std::process::id(), crate::HARNESS_VERSION, Some(vendor)) {
            journal.note(
                "NOTE",
                &format!("the claim was not re-announced for muse {vendor}: {why}"),
            );
            self.lose(&why, journal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aterm::Human;

    const HOLDER: &str = "muse-alab@412";
    const GRACE: Duration = Duration::from_secs(120);

    fn policy(reply: &str) -> Policy {
        Policy::parse(reply).expect("a policy")
    }

    fn on() -> Policy {
        policy("OK gen=1 effective=on why=enabled contract=2,3 relaunch=true upgrade=true")
    }

    fn held(human: Human) -> Screen {
        Screen {
            supervisor: Some(HOLDER.to_string()),
            proven: true,
            human,
        }
    }

    /// THE ACT GATE: a held, proven claim under a policy that is on, with the duty's switch on,
    /// acts; every other read holds, gives the claim back or makes it again, and a person's
    /// typing by aterm's clock holds a move but never a crash relaunch.
    #[test]
    fn an_act_waits_on_one_fresh_read_of_aterm() {
        for duty in [Duty::Relaunch, Duty::Upgrade] {
            assert_eq!(
                judge(&Ok((on(), held(Human::Never))), HOLDER, duty, GRACE),
                Verdict::Act,
                "{duty:?}"
            );
            assert!(matches!(
                judge(&Err("timed out".into()), HOLDER, duty, GRACE),
                Verdict::Hold("aterm-unread", _)
            ));
            let foreign = Screen {
                supervisor: Some("muse-alab@9".into()),
                ..held(Human::Never)
            };
            assert!(matches!(
                judge(&Ok((on(), foreign)), HOLDER, duty, GRACE),
                Verdict::Lose(_)
            ));
            let unproven = Screen {
                proven: false,
                ..held(Human::Never)
            };
            assert!(matches!(
                judge(&Ok((on(), unproven)), HOLDER, duty, GRACE),
                Verdict::Lose(_)
            ));
            let off = policy(
                "OK gen=1 effective=off why=autopilot-off contract=2,3 relaunch=true upgrade=true",
            );
            assert!(matches!(
                judge(&Ok((off, held(Human::Never))), HOLDER, duty, GRACE),
                Verdict::Release(_)
            ));
            let old = policy("OK gen=1 effective=on why=enabled relaunch=true upgrade=true");
            assert!(matches!(
                judge(&Ok((old, held(Human::Never))), HOLDER, duty, GRACE),
                Verdict::Release(_)
            ));
        }
        let no_relaunch = policy("OK gen=1 effective=on contract=3 relaunch=false upgrade=true");
        assert!(matches!(
            judge(
                &Ok((no_relaunch.clone(), held(Human::Never))),
                HOLDER,
                Duty::Relaunch,
                GRACE
            ),
            Verdict::Hold("duty-off", _)
        ));
        assert_eq!(
            judge(
                &Ok((no_relaunch, held(Human::Never))),
                HOLDER,
                Duty::Upgrade,
                GRACE
            ),
            Verdict::Act
        );
        let unsaid = policy("OK gen=1 effective=on contract=3");
        assert!(matches!(
            judge(
                &Ok((unsaid, held(Human::Never))),
                HOLDER,
                Duty::Upgrade,
                GRACE
            ),
            Verdict::Hold("duty-off", _)
        ));
        let typing = held(Human::Ago(Duration::from_secs(3)));
        assert!(matches!(
            judge(&Ok((on(), typing.clone())), HOLDER, Duty::Upgrade, GRACE),
            Verdict::Hold("person-typing", _)
        ));
        assert_eq!(
            judge(&Ok((on(), typing)), HOLDER, Duty::Relaunch, GRACE),
            Verdict::Act,
            "a crashed child is relaunched whoever typed"
        );
        assert!(matches!(
            judge(
                &Ok((on(), held(Human::Unknown))),
                HOLDER,
                Duty::Upgrade,
                GRACE
            ),
            Verdict::Hold("person-typing", _)
        ));
        assert_eq!(
            judge(
                &Ok((on(), held(Human::Ago(GRACE)))),
                HOLDER,
                Duty::Upgrade,
                GRACE
            ),
            Verdict::Act
        );
    }

    #[test]
    fn outside_aterm_every_act_goes_ahead() {
        let journal = Journal::open(&crate::env::Env::default());
        let mut link = Link::new(None);
        assert!(matches!(link, Link::Off));
        assert_eq!(link.may_act(Duty::Relaunch, GRACE, "1", &journal), Ok(()));
        assert_eq!(link.may_act(Duty::Upgrade, GRACE, "1", &journal), Ok(()));
    }
}
