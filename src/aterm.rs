// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! muse-alab's side of its link to aterm. The link itself is the shared library `alab-wire`
//! (`vendor/alab-harness/crates/alab-wire`, the copy of alab-harness pinned in
//! `vendor/alab-harness.pin`): the wire, the claim with its guards (the scope read back), the
//! [`Link`] state machine with its bounds and its busy-not-gone rule, aterm's exit judgement and
//! the crash ladder. This module holds only what is this harness's own: who it is to aterm
//! ([`config`], declaring the one contract it claims under, [`CONTRACTS`]), the clock its link
//! runs on ([`now_ms`]), what it claims for a child ([`child`]), whether a move may be weighed at
//! all under the grant ([`upgrade_held`]), aterm's word at a move's act ([`move_unheld`], which
//! adds Muse Code's composer and person checks to `alab-wire`'s rule for a move a harness judges
//! by its own readings), and how the link's notes reach the journal ([`journal`]).

use std::ffi::OsString;
use std::time::Duration;

use alab_wire::claim::Human;
use alab_wire::{
    Child, ChildProcess, Config, Contract, Facts, Link, Reading, SafePoint, Scope, Warrant,
};

use crate::env::Env;
use crate::gate::{self, Composer, ComposerWitness};
use crate::journal::Journal;

/// This harness's id: its row in aterm's roster and registry, and the holder `<id>@<pid>`.
pub const HOLDER_ID: &str = crate::HARNESS;
/// The word of the vendor it runs.
pub const VENDOR_WORD: &str = "muse";
/// The duties this build performs and claims from aterm: bringing a crashed child back on its
/// session, and moving it onto another vendor build (or this harness onto another build of
/// itself). The typing scopes stay unclaimed: this harness types nothing.
pub const SCOPES: [Scope; 2] = [Scope::Relaunch, Scope::Upgrade];

/// The contracts this harness declares and claims under: 4 alone, the `contracts` of
/// `alab-harness.toml` (a unit test holds the two equal). Contract 3 is aterm's compiled seed
/// rows' alone; a record-bearing build never claims it, and a 3 in the record would keep aterm
/// serving it (the harness proofs' plan, D11). [`config`] declares them to `alab-wire`
/// (`Config::with_contracts`, alab-harness Decision 33), whose every claim (the launch, an
/// adoption, each one made again after a loss or a policy switched back on) keeps to them: an
/// aterm that serves none of them (`2,3`, or 2 alone) is `Failure::Unserved`, sent no claim and
/// read again every 5 s.
pub const CONTRACTS: [Contract; 1] = [Contract::Four];

/// Who this harness is to aterm, declaring [`CONTRACTS`]. It asks for no session stream
/// (`alab-wire` v0.4.0's, which a contract-4 grant opens by default): taking the stream is its
/// own package (HX-19), so a contract-4 claim stays on the v0.1 path, renewed with `meta set
/// supervisor` and woken by the event subscription.
///
/// # Panics
/// Never: the id and vendor word are fixed lowercase words, [`SCOPES`] and [`CONTRACTS`] are
/// not empty, and the build refuses a [`crate::HARNESS_VERSION`] that is not
/// `MAJOR.MINOR.0[+g<sha>]` (`crate::is_pack_version`), which is a claim's `version=`. The unit
/// test makes this config.
#[must_use]
pub fn config() -> Config {
    Config::new(HOLDER_ID, VENDOR_WORD, &SCOPES, crate::HARNESS_VERSION)
        .and_then(|config| config.with_contracts(&CONTRACTS))
        .expect("muse-alab's id, vendor, scopes, version and contracts make a config aterm reads")
        .with_stream(false)
}

/// `env` as the lookup `alab-wire` reads a session from.
pub fn lookup(env: &Env) -> impl Fn(&str) -> Option<OsString> + '_ {
    |name| env.aterm_var(name)
}

/// Whether this process runs in an aterm tab, by the marks `config` names.
#[must_use]
pub fn hosted(env: &Env, config: &Config) -> bool {
    config.env().hosted_by(lookup(env))
}

/// The link's clock: milliseconds since boot on a system clock that stops while the machine
/// sleeps, the clock `std::time::Instant` reads (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC`
/// on Linux). Every crash-ladder window and away budget runs on it, so a closed lid neither
/// spends the 45 s aterm has to come back nor counts as a healthy run that forgives a crash
/// streak; aterm's own grace runs on `Instant` too. It is a system clock rather than one a
/// process starts, so a time carried across a harness re-exec reads the same on both sides.
#[must_use]
pub fn now_ms() -> u64 {
    uptime_ns() / 1_000_000
}

/// The system clock that stops while the machine sleeps: macOS's `CLOCK_UPTIME_RAW`
/// (`CLOCK_MONOTONIC` there keeps counting through sleep), Linux's `CLOCK_MONOTONIC`.
#[cfg(target_os = "macos")]
const UPTIME_CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
/// The system clock that stops while the machine sleeps (see the macOS twin).
#[cfg(not(target_os = "macos"))]
const UPTIME_CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

/// Nanoseconds on [`UPTIME_CLOCK`].
fn uptime_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a writable timespec that outlives the call, which writes only it. The
    // clock is one every Rust on this platform reads for `Instant`, so the call does not fail;
    // were it to, `now` stays zero, which reads as no time passed.
    let _ = unsafe { libc::clock_gettime(UPTIME_CLOCK, &raw mut now) };
    u64::try_from(now.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or(0))
}

/// What the claim says about the child: its vendor build and, once it runs, its pid and kernel
/// start time (`child=` under contract 4). Muse Code runs on no account this harness chooses, so
/// no `login=` is ever claimed.
#[must_use]
pub fn child(vendor: Option<&str>, process: Option<ChildProcess>) -> Child<'_> {
    Child {
        vendor,
        login: None,
        process,
    }
}

/// The kernel start of the process `pid` as aterm checks `child=`, or none when it cannot be
/// read (left out of the claim rather than guessed).
#[must_use]
pub fn process_of(pid: u32) -> Option<ChildProcess> {
    ChildProcess::of(pid).ok()
}

/// Whether the standing claim was granted both duties this harness performs. A launch whose
/// claim was granted less gives it back and runs Muse Code in its place: a narrower grant would
/// leave a duty to aterm's engine and this harness at once.
#[must_use]
pub fn takes_both(link: &Link) -> bool {
    SCOPES.iter().all(|scope| link.takes(*scope))
}

/// The granted duties as one journal word.
#[must_use]
pub fn granted(link: &Link) -> String {
    link.grant()
        .map_or_else(|| "-".to_string(), alab_wire::Grant::words)
}

/// Whether the harness may move onto another build at all, by the link alone (no I/O). Outside
/// aterm (`hosted` false, so no link was ever made) the harness's own readings decide, as before
/// it had a client. In an aterm tab a move is aterm's `upgrade` duty, handed to this harness only
/// by a standing claim that holds it, so every other state holds the move:
/// - a link lost and being claimed again, or one aterm has not admitted yet (a launch aterm
///   answered busy, off for now or not serving a contract this harness claims at), made again
///   every 5 s (`no-claim`);
/// - a link aterm refused after such a launch, absent for the session (`no-claim`): aterm never
///   let this harness lead the tab;
/// - a claim given back while aterm's policy is off (`policy-off`);
/// - a grant without `upgrade` (a person's cap, or an aterm that does not know the word), which
///   leaves the duty to aterm (`upgrade-not-granted`).
///
/// A crash relaunch or an asked restart stays on the build the child ran under the same rule
/// ([`upgrade_granted`]).
///
/// # Errors
/// The wait word and why.
pub fn upgrade_held(link: &Link, hosted: bool) -> Result<(), (&'static str, String)> {
    if link.is_absent() {
        if hosted {
            Err((
                "no-claim",
                "aterm refused this harness's claim; the session's moves stay aterm's".to_string(),
            ))
        } else {
            Ok(())
        }
    } else if link.is_lost() {
        let why = if link.claims() {
            "aterm does not hold this harness's claim; it is made again every 5 s"
        } else {
            "aterm has not admitted this harness's claim yet; it is made again every 5 s"
        };
        Err(("no-claim", why.to_string()))
    } else if link.grant().is_none() {
        Err((
            "policy-off",
            "the claim was given back while aterm's harness policy is off".to_string(),
        ))
    } else if link.takes(Scope::Upgrade) {
        Ok(())
    } else {
        Err((
            "upgrade-not-granted",
            format!("aterm granted scope={}", granted(link)),
        ))
    }
}

/// Whether a relaunch may start a newer build than the one the child ran: the move is aterm's
/// `upgrade` duty, so only outside aterm or under a grant that takes it ([`upgrade_held`]).
#[must_use]
pub fn upgrade_granted(link: &Link, hosted: bool) -> bool {
    upgrade_held(link, hosted).is_ok()
}

/// aterm's word at a move's act, once the harness's own readings call the moment safe: one
/// fresh policy and status read ([`Link::read`]) judged by [`judge_move`]. Outside aterm the
/// composer is judged on the input clock `local` alone. In an aterm tab, a link holding no
/// standing claim of `upgrade`, before the read or after it, holds the move ([`upgrade_held`]);
/// so does any hold aterm states in place of facts.
///
/// # Errors
/// The wait word and why.
pub fn move_unheld(
    link: &mut Link,
    hosted: bool,
    grace: Duration,
    local: ComposerWitness,
    now_ms: u64,
) -> Result<(), (&'static str, String)> {
    upgrade_held(link, hosted)?;
    if link.is_absent() {
        // Outside aterm: no link, and the harness's own readings decide.
        return gate::check_composer(local).map_err(|word| (word, composer_why(word, None)));
    }
    let holder = link.config().holder(std::process::id());
    match link.read(now_ms) {
        Reading::Fresh(facts) => judge_move(&facts, &holder, grace, local),
        Reading::Hold(why) => Err((
            "aterm-hold",
            why.strip_prefix("wait ").unwrap_or(&why).to_string(),
        )),
        // The read lost the link, or released a claim the policy no longer admits.
        Reading::Local => upgrade_held(link, hosted).and(Err((
            "aterm-unread",
            "aterm gave no facts for the move".to_string(),
        ))),
    }
}

/// aterm's facts on a move the harness's own readings called safe. First `alab-wire`'s rule for
/// such a move ([`alab_wire::claim::judge_own_move`] with [`Warrant::Harness`]): the claim is
/// still `holder`'s, proven and takes `upgrade`; aterm reads no halt and no other driver's hand;
/// and the policy has upgrades on and asks nobody to say when. Then what aterm sees of Muse
/// Code that the harness cannot: a person's last key (`human_ms`, within the longer of aterm's
/// `human_grace_s` and the harness's own `grace`, an unknown one holding) and the composer
/// (`composer=`, aterm's screen reader; where aterm serves none, the input clock `local`).
///
/// # Errors
/// The wait word and why.
pub fn judge_move(
    facts: &Facts,
    holder: &str,
    grace: Duration,
    local: ComposerWitness,
) -> Result<(), (&'static str, String)> {
    if let SafePoint::Hold(hold) =
        alab_wire::claim::judge_own_move(&facts.policy, &facts.status, holder, Warrant::Harness)
    {
        return Err(("aterm-hold", format!("aterm {}={}", hold.field, hold.value)));
    }
    let grace = facts.policy.human_grace.max(grace);
    match facts.status.human {
        Human::Never => {}
        Human::Ago(ago) if ago >= grace => {}
        Human::Ago(ago) => {
            return Err((
                "person-typing",
                format!(
                    "aterm saw a person type {} ms ago (human_ms), within {} ms",
                    ago.as_millis(),
                    grace.as_millis()
                ),
            ));
        }
        Human::Unknown => {
            return Err((
                "person-typing",
                "aterm's human_ms is absent or unreadable".to_string(),
            ));
        }
    }
    let served = facts.status.composer.as_deref().map(Composer::parse);
    gate::check_composer(ComposerWitness::of(served, local))
        .map_err(|word| (word, composer_why(word, served)))
}

/// Why the composer witness held a move with `word`, aterm having served `served`.
fn composer_why(word: &str, served: Option<Composer>) -> String {
    match served {
        Some(Composer::Draft) => {
            "aterm reads a person's draft in the composer (composer=draft)".into()
        }
        Some(_) => "aterm does not read the composer as empty or a placeholder (composer=-)".into(),
        None if word == "no-input-clock" => {
            "no terminal input clock proves the composer empty, and aterm serves no composer="
                .into()
        }
        None => {
            "the terminal delivered input since the last submission, and aterm serves no composer="
                .into()
        }
    }
}

/// Write the link's notes to the journal, each `aterm <verb> <detail>` as a `<VERB>` event:
/// after every step of the link (its launch, an adoption, a tick, an event drain, a read).
pub fn journal(link: &mut Link, journal: &Journal) {
    for line in link.take_notes() {
        let rest = line.strip_prefix("aterm ").unwrap_or(&line);
        let (verb, detail) = rest.split_once(' ').unwrap_or((rest, ""));
        journal.note(&verb.to_ascii_uppercase(), detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config declares to `alab-wire` the contracts the record says (`alab-harness.toml`,
    /// which atpkg-publish copies into the signed manifest): 4 alone.
    #[test]
    fn the_record_and_the_claim_name_contract_4_alone() {
        let record = include_str!("../alab-harness.toml");
        let line = record
            .lines()
            .find(|line| line.split('=').next().map(str::trim) == Some("contracts"))
            .expect("alab-harness.toml says its contracts");
        let listed: Vec<u32> = line
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(list, _)| list)
            .expect("a list")
            .split(',')
            .map(|n| n.trim().parse().expect("a contract number"))
            .collect();
        assert_eq!(listed, CONTRACTS.map(Contract::number));
        assert_eq!(listed, [4]);
        assert_eq!(config().contracts(), CONTRACTS);
    }

    #[test]
    fn this_harness_is_muse_alab_claiming_relaunch_and_upgrade() {
        let config = config();
        assert_eq!(config.id(), "muse-alab");
        assert_eq!(config.vendor(), "muse");
        assert_eq!(config.scopes(), [Scope::Relaunch, Scope::Upgrade]);
        assert_eq!(config.holder(77), "muse-alab@77");
        assert_eq!(config.version(), crate::HARNESS_VERSION);
        assert!(!config.streams(), "the session stream is HX-19's");
    }

    #[test]
    fn the_session_is_read_from_the_harnesss_own_environment() {
        let config = config();
        let hosted_by = |pairs: &[(&str, &str)]| hosted(&Env::with_aterm_vars(pairs), &config);
        assert!(hosted_by(&[("TERM_PROGRAM", "aterm")]));
        assert!(hosted_by(&[("ATERM_CHILD", "1")]));
        assert!(hosted_by(&[("ATERM_SESSION_ID", "s-1")]));
        assert!(!hosted_by(&[("TERM_PROGRAM", "iTerm.app")]));
        assert!(!hosted_by(&[("ATERM_CHILD", "0")]));
        assert!(!hosted_by(&[]));
    }

    /// The link's clock leaves out the time the machine slept, as `Instant` does: on macOS it
    /// reads `CLOCK_UPTIME_RAW`, not `CLOCK_MONOTONIC`, which counts sleep. On a Mac that has
    /// slept since boot the two differ by that sleep, so a `CLOCK_MONOTONIC` reading falls
    /// outside the window this reads around it.
    #[test]
    fn the_clock_is_the_uptime_clock_that_stops_in_sleep() {
        let raw_ms = || {
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `now` is a writable timespec that outlives the call, which writes only it.
            let rc = unsafe { libc::clock_gettime(UPTIME_CLOCK, &raw mut now) };
            assert_eq!(rc, 0);
            u64::try_from(now.tv_sec).unwrap() * 1_000
                + u64::try_from(now.tv_nsec).unwrap() / 1_000_000
        };
        let before = raw_ms();
        let now = now_ms();
        let after = raw_ms();
        assert!(
            (before..=after).contains(&now),
            "{now} is not between {before} and {after}"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(UPTIME_CLOCK, libc::CLOCK_UPTIME_RAW);
    }

    /// The policy in effect, upgrades on, nobody asked to say when.
    const ON: &str = "effective=on why=enabled upgrade=true upgrade_ask=false";

    fn facts(policy: &str, status: &str) -> Facts {
        Facts {
            policy: alab_wire::Policy::parse(&format!(
                "OK gen=1 contract=2,3,4 relaunch=true human_grace_s=120 {policy}"
            ))
            .unwrap(),
            status: alab_wire::Status::parse(&format!(
                "OK schema=1 supervisor=muse-alab%4077 supervisor_proven=1 \
                 supervisor_scope=relaunch,upgrade {status}"
            ))
            .unwrap(),
        }
    }

    const HOLDER: &str = "muse-alab@77";
    const GRACE: Duration = Duration::from_secs(1);
    /// The input clock: nothing typed since the last submission.
    const QUIET: ComposerWitness = ComposerWitness::InputClock {
        last_input_us: Some(100),
        submitted_us: 200,
    };
    /// The input clock: input since the last submission.
    const TYPED: ComposerWitness = ComposerWitness::InputClock {
        last_input_us: Some(300),
        submitted_us: 200,
    };
    const FREE: &str = "hold=0 hand=- human_ms=- composer=empty";

    /// `alab-wire`'s rule for a move a harness judges by its own readings comes first: the claim,
    /// no halt, no other driver, and the policy's two upgrade switches.
    #[test]
    fn a_move_needs_the_claim_no_halt_no_driver_and_both_switches() {
        assert_eq!(judge_move(&facts(ON, FREE), HOLDER, GRACE, QUIET), Ok(()));
        for (policy, status, why) in [
            (
                ON,
                "hold=1 hand=- human_ms=- composer=empty",
                "aterm hold=1",
            ),
            (
                ON,
                "hold=0 hand=lease:driver%404 human_ms=- composer=empty",
                "aterm hand=lease:driver%25404",
            ),
            (
                "effective=on why=enabled upgrade=false upgrade_ask=false",
                FREE,
                "aterm upgrade=false",
            ),
            (
                "effective=on why=enabled upgrade=true upgrade_ask=true",
                FREE,
                "aterm upgrade_ask=true",
            ),
        ] {
            assert_eq!(
                judge_move(&facts(policy, status), HOLDER, GRACE, QUIET),
                Err(("aterm-hold", why.to_string())),
                "{policy} / {status}"
            );
        }
        assert_eq!(
            judge_move(&facts(ON, FREE), "muse-alab@78", GRACE, QUIET),
            Err(("aterm-hold", "aterm supervisor=muse-alab@77".to_string()))
        );
    }

    /// A person aterm saw type within the longer of its grace and the harness's holds the move,
    /// and so does a `human_ms` aterm could not say.
    #[test]
    fn a_move_waits_out_the_longer_person_grace() {
        let held = |human: &str, grace: Duration| {
            judge_move(
                &facts(ON, &format!("hold=0 hand=- composer=empty {human}")),
                HOLDER,
                grace,
                QUIET,
            )
            .map_err(|(word, _)| word)
        };
        assert_eq!(held("human_ms=119000", GRACE), Err("person-typing"));
        assert_eq!(held("human_ms=120000", GRACE), Ok(()));
        assert_eq!(
            held("human_ms=120000", Duration::from_secs(200)),
            Err("person-typing"),
            "the harness's grace can lengthen aterm's"
        );
        assert_eq!(held("human_ms=-", GRACE), Ok(()));
        assert_eq!(held("", GRACE), Err("person-typing"));
        assert_eq!(held("human_ms=soon", GRACE), Err("person-typing"));
    }

    /// THE COMPOSER IS aterm's WORD WHERE aterm SERVES ONE: `empty` and `placeholder` let a move
    /// go however the input clock reads, `draft` and `-` hold it; with no `composer=` served the
    /// input clock judges.
    #[test]
    fn the_composer_is_aterms_word_where_aterm_serves_one() {
        let with = |composer: &str, local| {
            judge_move(
                &facts(ON, &format!("hold=0 hand=- human_ms=- {composer}")),
                HOLDER,
                GRACE,
                local,
            )
            .map_err(|(word, _)| word)
        };
        assert_eq!(with("composer=empty", TYPED), Ok(()));
        assert_eq!(with("composer=placeholder", TYPED), Ok(()));
        assert_eq!(with("composer=draft", QUIET), Err("draft"));
        assert_eq!(with("composer=-", QUIET), Err("composer-unproven"));
        assert_eq!(with("", QUIET), Ok(()));
        assert_eq!(with("", TYPED), Err("draft-unproven"));
    }

    /// With no claim ever made (outside aterm) a move is the harness's own: nothing is read
    /// from aterm, the grant holds nothing, and the input clock judges the composer.
    #[test]
    fn outside_aterm_a_move_is_judged_on_the_input_clock_alone() {
        let mut link = Link::absent(config(), 77);
        assert_eq!(upgrade_held(&link, false), Ok(()));
        assert!(upgrade_granted(&link, false));
        assert_eq!(move_unheld(&mut link, false, GRACE, QUIET, 0), Ok(()));
        assert_eq!(
            move_unheld(&mut link, false, GRACE, TYPED, 0).map_err(|(word, _)| word),
            Err("draft-unproven")
        );
        assert!(link.take_notes().is_empty());
    }

    /// The move's word on `link` in an aterm tab, the session quiet: the wait word, or none.
    fn hosted_move(link: &mut Link) -> Result<(), &'static str> {
        move_unheld(link, true, GRACE, QUIET, now_ms()).map_err(|(word, _)| word)
    }

    /// IN AN aterm TAB A MOVE NEEDS A STANDING CLAIM: a launch aterm has not admitted yet (here
    /// Autopilot off, a link lost that never held a claim) and a link aterm refused after it
    /// (absent for the session) hold every move, however quiet the session, and read nothing of
    /// aterm, where outside aterm the same quiet session moves. A claim that stands with
    /// `upgrade` lets the move go on aterm's fresh read.
    #[test]
    fn in_an_aterm_tab_a_move_needs_a_standing_claim() {
        let aterm = alab_wire::fake::FakeAterm::for_harness("muse-alab-unit-unclaimed", &config());
        aterm.set_policy(&alab_wire::fake::POLICY_ON.replace(
            "effective=on why=enabled",
            "effective=off why=autopilot-off",
        ));
        let lookup = aterm.lookup(config().env());
        let me = std::process::id();
        let mut link = Link::launch_with(config(), me, child(Some("1.4.10"), None), 0, &lookup);
        assert!(link.is_lost() && !link.claims(), "{:?}", link.take_notes());
        let verbs = aterm.verbs().len();
        assert_eq!(
            upgrade_held(&link, true).map_err(|(w, _)| w),
            Err("no-claim")
        );
        assert!(!upgrade_granted(&link, true));
        assert_eq!(hosted_move(&mut link), Err("no-claim"));
        assert_eq!(aterm.verbs().len(), verbs, "nothing asked of aterm");

        // aterm refuses the claim made again (no registry row proves it): absent for good.
        aterm.set_registered(false);
        aterm.set_policy(alab_wire::fake::POLICY_ON);
        link.tick(child(Some("1.4.10"), None), now_ms() + 60_000);
        assert!(link.is_absent(), "{:?}", link.take_notes());
        assert_eq!(
            upgrade_held(&link, true).map_err(|(w, _)| w),
            Err("no-claim")
        );
        assert!(!upgrade_granted(&link, true));
        assert_eq!(hosted_move(&mut link), Err("no-claim"));
        assert_eq!(upgrade_held(&link, false), Ok(()), "outside aterm it moves");

        // A claim that stands, with `upgrade`, is asked at the act.
        aterm.set_registered(true);
        let mut held = Link::launch_with(config(), me, child(Some("1.4.10"), None), 0, &lookup);
        assert!(held.judges(), "{:?}", held.take_notes());
        assert_eq!(upgrade_held(&held, true), Ok(()));
        assert!(upgrade_granted(&held, true));
        let verbs = aterm.verbs().len();
        assert_eq!(hosted_move(&mut held), Ok(()));
        assert!(aterm.verbs().len() > verbs, "aterm read at the act");
    }

    #[test]
    fn the_links_notes_become_journal_events() {
        let dir =
            std::env::temp_dir().join(format!("muse-alab-aterm-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Journal::open(&Env {
            xdg_state_home: Some(dir.clone()),
            ..Env::default()
        });
        let env = Env::with_aterm_vars(&[("TERM_PROGRAM", "aterm")]);
        // Hosted, but no session to locate: the link is absent and says why.
        let mut link =
            Link::launch_with(config(), 77, child(Some("1.4.10"), None), 0, lookup(&env));
        assert!(link.is_absent());
        journal(&mut link, &log);
        let text = std::fs::read_to_string(log.path().unwrap()).unwrap();
        assert!(text.contains(" UNCLAIMED locator "), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Whether `verbs` (what the fake aterm was asked) hold a contract-3 claim, or the read-only
    /// roster check `alab-wire` makes before one alone (`native-act info`), or any claim not at 4.
    fn any_contract_3(verbs: &[String]) -> bool {
        verbs.iter().any(|verb| {
            verb == "native-act info"
                || (verb.starts_with("meta set supervisor ") && !verb.contains(" contract=4 "))
        })
    }

    /// NO CONTRACT-3 CLAIM IS EVER SENT ([`CONTRACTS`], declared to `alab-wire`). An aterm that
    /// serves `2,3` (aterm main before HX-06) or 2 alone is sent no claim and no contract-3
    /// roster read, at the launch, at an adoption or on the reclaim clock, and the launch's lost
    /// link says why (`Failure::Unserved`, on which the launch passes Muse Code through). Once
    /// aterm serves 4 the claim is made at 4, and a claim lost to an aterm restart into `2,3` is
    /// made again at 4 alone. NEGATIVE CONTROL: the same scene with `alab-wire`'s default
    /// contracts (3 and 4) claims at 3.
    #[test]
    fn no_contract_3_claim_is_ever_sent() {
        use alab_wire::Failure;
        use alab_wire::link::RECLAIM_EVERY_MS;

        let aterm = alab_wire::fake::FakeAterm::for_harness("muse-alab-unit-contracts", &config());
        let lookup = aterm.lookup(config().env());
        let me = std::process::id();
        let muse = || child(Some("1.4.10"), None);
        let mut ms = 0;
        for (served, why) in [
            (&[2, 3][..], "policy contract=2,3 declared=4"),
            (&[2][..], "policy contract=2 declared=4"),
        ] {
            aterm.serve_contracts(served);
            let mut link = Link::launch_with(config(), me, muse(), ms, &lookup);
            assert!(link.is_lost() && !link.claims(), "{why}");
            assert_eq!(link.unclaimed(), Some(&Failure::Unserved(why.to_string())));
            assert!(
                link.take_notes()
                    .contains(&format!("aterm unclaimed {why}")),
                "{why}"
            );
            for _ in 0..3 {
                ms += RECLAIM_EVERY_MS;
                link.tick(muse(), ms);
                assert!(link.is_lost() && link.grant().is_none(), "{why}");
            }
            let adopted = Link::adopt_with(config(), me, muse(), ms, &lookup);
            assert!(adopted.is_lost() && adopted.grant().is_none(), "{why}");
            assert_eq!(aterm.holder(), None, "{why}");
        }
        assert!(!any_contract_3(&aterm.verbs()), "{:?}", aterm.verbs());
        assert!(
            !aterm
                .verbs()
                .iter()
                .any(|verb| verb.starts_with("meta set supervisor ")),
            "no claim at all: {:?}",
            aterm.verbs()
        );

        // aterm serves 4: the lost launch is claimed at 4 on the clock.
        aterm.serve_contracts(&[2, 3]);
        let mut link = Link::launch_with(config(), me, muse(), ms, &lookup);
        aterm.serve_contracts(&[2, 3, 4]);
        ms += RECLAIM_EVERY_MS;
        link.tick(muse(), ms);
        assert_eq!(
            link.grant().map(|grant| grant.contract),
            Some(Contract::Four),
            "{:?}",
            link.take_notes()
        );
        assert_eq!(aterm.holder(), Some(format!("muse-alab@{me}")));

        // aterm restarts into one serving `2,3`: the claim is made again at 4 alone.
        aterm.restart();
        aterm.serve_contracts(&[2, 3]);
        let _ = link.relaunch_word(false, ms);
        assert!(link.is_lost() && link.claims(), "{:?}", link.take_notes());
        for _ in 0..3 {
            ms += RECLAIM_EVERY_MS;
            link.tick(muse(), ms);
            assert!(link.is_lost() && link.grant().is_none());
        }
        assert_eq!(
            link.unclaimed(),
            Some(&Failure::Unserved(
                "policy contract=2,3 declared=4".to_string()
            ))
        );
        assert_eq!(aterm.holder(), None);
        aterm.serve_contracts(&[2, 3, 4]);
        ms += RECLAIM_EVERY_MS;
        link.tick(muse(), ms);
        assert_eq!(
            link.grant().map(|grant| grant.contract),
            Some(Contract::Four),
            "{:?}",
            link.take_notes()
        );
        link.release(ms);
        let verbs = aterm.verbs();
        assert!(!any_contract_3(&verbs), "{verbs:?}");

        // NEGATIVE CONTROL: the default contracts claim at 3 from the same `2,3` aterm.
        aterm.serve_contracts(&[2, 3]);
        let both = Config::new(HOLDER_ID, VENDOR_WORD, &SCOPES, crate::HARNESS_VERSION)
            .unwrap()
            .with_stream(false);
        let mut three = Link::launch_with(both, me, muse(), ms, &lookup);
        assert_eq!(
            three.grant().map(|grant| grant.contract),
            Some(Contract::Three)
        );
        three.release(ms);
        assert!(any_contract_3(&aterm.verbs()[verbs.len()..]));
    }
}
