// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LOOP: one vendor child at a time, the harness process never exiting until the
//! session really ends.
//!
//! - It waits on signals and the child's state changes ([`crate::signals`],
//!   [`crate::child`]); a stopped child is followed, a continued harness continues it.
//! - It watches the store's `current` link. When atpkg has installed a newer vendor build
//!   it looks for a safe point ([`crate::gate`]), proves the new build starts
//!   ([`crate::probe`], as aterm's quiet restart does before its signal), and only then
//!   sends the child `SIGTERM` (as aterm's own Muse Code quiet restart does,
//!   `upgrade_muse_live_drive.rs`). When the child has ended it relaunches a proved build
//!   with Muse Code's resume argv on the same session
//!   ([`crate::muse::argv::rewrite_argv`]). A child that does not end within `stop_wait`
//!   is left alone: never killed, never signalled again.
//! - A child that crashed (aterm's Muse Code exit table: not clean, not a startup
//!   refusal) is relaunched on its session the same way, at most `crash_budget` times per
//!   `crash_window` (aterm's crash relaunch, `upgrade_muse_drive.rs` `after_exit_onto`),
//!   on the newest build when it is proved to start, else on the build it ran. Under an
//!   aterm claim the relaunch waits on aterm's word, held up to `reclaim_hold` while a claim
//!   that lapsed is made again ([`Harness::relaunch_word`]).
//! - Any other end — a clean exit, a refusal, a crash it cannot resume, `SIGHUP` or
//!   `SIGTERM` to the harness — ends the harness with the child's real status: the same
//!   exit code, or death by the same signal.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::child::{self, End, Status};
use crate::env::{self, Env};
use crate::gate::{self, Terminal, Timing};
use crate::journal::Journal;
use crate::link::{Duty, Link};
use crate::muse::{self, ExitKind, PACKAGE, PRODUCT, PROGRAM, argv, session};
use crate::probe::{self, Probe};
use crate::store::{self, Build, Layout};
use crate::tty::{self, Mode, Tty};
use crate::version::{Version, moves_onto};
use crate::{HARNESS, aterm, procs, say, signals};

/// The loop's clocks and budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The safe point's clocks.
    pub timing: Timing,
    /// How long one wait for a signal lasts.
    pub tick: Duration,
    /// How often the store and the safe point are looked at.
    pub look_every: Duration,
    /// How often a missing session witness is searched for again.
    pub scan_every: Duration,
    /// How long a child gets to end after the harness's `SIGTERM`.
    pub stop_wait: Duration,
    /// How many crash relaunches a window allows.
    pub crash_budget: usize,
    /// The crash window.
    pub crash_window: Duration,
    /// How long a build probe may run ([`crate::probe`]).
    pub probe_wait: Duration,
    /// How long after a failed probe the same build is probed again.
    pub probe_gap: Duration,
    /// How long a crash relaunch waits for aterm to prove this harness's claim again after it
    /// lapsed ([`Harness::relaunch_word`]).
    pub reclaim_hold: Duration,
}

impl Config {
    /// The production clocks.
    pub const PRODUCTION: Config = Config {
        timing: Timing::PRODUCTION,
        tick: Duration::from_millis(500),
        look_every: Duration::from_secs(2),
        scan_every: Duration::from_secs(30),
        stop_wait: Duration::from_secs(15),
        crash_budget: 3,
        crash_window: Duration::from_secs(600),
        probe_wait: probe::PROBE_WAIT,
        probe_gap: probe::PROBE_GAP,
        // aterm's adopted-claim grace: two of its 20 s claim renewals and 5 s, the time aterm
        // itself gives a claimant to come back after a seamless update or a restart.
        reclaim_hold: Duration::from_secs(45),
    };

    /// The clocks this build runs with: production, except that a DEBUG build reads
    /// `MUSE_ALAB_TEST_TIMING=<tick>,<look>,<person grace>,<settle>,<stop wait>[,<reclaim
    /// hold>]` (ms) so the integration tests run in seconds. A release build reads no such
    /// variable.
    #[must_use]
    pub fn for_this_build() -> Self {
        #[cfg(debug_assertions)]
        if let Some(c) = std::env::var("MUSE_ALAB_TEST_TIMING")
            .ok()
            .and_then(|v| Self::parse_test_timing(&v))
        {
            return c;
        }
        Self::PRODUCTION
    }

    /// The test timing's parse (see [`Config::for_this_build`]).
    #[must_use]
    pub fn parse_test_timing(v: &str) -> Option<Self> {
        let ms: Vec<u64> = v
            .split(',')
            .map(|p| p.trim().parse().ok())
            .collect::<Option<_>>()?;
        let (tick, look, grace, settle, stop, hold) = match ms.as_slice() {
            [tick, look, grace, settle, stop] => (tick, look, grace, settle, stop, None),
            [tick, look, grace, settle, stop, hold] => {
                (tick, look, grace, settle, stop, Some(*hold))
            }
            _ => return None,
        };
        Some(Config {
            timing: Timing {
                person_grace_us: grace * 1000,
                settle_us: settle * 1000,
            },
            tick: Duration::from_millis(*tick),
            look_every: Duration::from_millis(*look),
            scan_every: Duration::from_millis(*look),
            stop_wait: Duration::from_millis(*stop),
            reclaim_hold: hold.map_or(Self::PRODUCTION.reclaim_hold, Duration::from_millis),
            ..Self::PRODUCTION
        })
    }
}

/// How long after a move's relaunch an abnormal or refused exit counts as the new build
/// failing to start the resumed session (falling back onto the previous build).
const STARTUP_GRACE: Duration = Duration::from_secs(15);

/// The running child.
#[derive(Debug)]
struct Run {
    pid: u32,
    build: Build,
    /// Its arguments, without `argv[0]`.
    args: Vec<OsString>,
    /// When it was started (ms since the epoch, read just before the spawn).
    started_ms: u64,
    /// The session it holds and its lock, once witnessed.
    lock: Option<(String, PathBuf)>,
    /// When a missing witness was last searched for.
    scanned: Option<Instant>,
    /// The harness's `SIGTERM` at a safe point, awaiting the child's end.
    stop: Option<Stop>,
    /// The previous build and session when this child was just started by a live move.
    moved: Option<(Build, String, Instant)>,
    /// The child outlived a `SIGTERM`: it is never signalled for an update again.
    term_ignored: bool,
}

#[derive(Debug)]
struct Stop {
    from: Build,
    to: Build,
    session: String,
    sent: Instant,
}

/// What the harness knows of whether one build starts ([`crate::probe`]).
#[derive(Clone, Copy, Debug)]
struct Proof {
    found: Probe,
    at: Instant,
    /// A move onto this build failed after a probe proved it: no later probe can re-prove it.
    blind: bool,
}

/// The harness: its environment, its store, its journal and its clocks.
#[derive(Debug)]
pub struct Harness {
    env: Env,
    layout: Layout,
    journal: Journal,
    cfg: Config,
    tty: Option<Tty>,
    /// The terminal's mode when the harness started, put back before a relaunch and
    /// after a child that a signal ended (a crashed TUI leaves raw mode behind, and the
    /// next one would take raw mode for the mode to restore at its exit).
    mode: Option<Mode>,
    me: Option<PathBuf>,
    crashes: Vec<Instant>,
    /// This harness's nesting depth ([`crate::env::nesting_depth`]); every process it
    /// starts as the vendor carries `depth + 1`.
    depth: u32,
    /// The builds probed, and what was found.
    proofs: BTreeMap<Version, Proof>,
    /// The harness's standing with aterm ([`crate::link`]): every act waits on its word.
    link: Link,
}

/// Whether `exe` is this harness, never to be run: named like it, the same file (a
/// symlink or hard link to it), or a byte-for-byte copy of it. `me` is this process's
/// executable, read at start; the bytes are compared only when the sizes are equal.
#[must_use]
pub fn is_self(exe: &Path, me: Option<&Path>) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    if exe.file_name().and_then(|n| n.to_str()) == Some(HARNESS) {
        return true;
    }
    let Some(me) = me else {
        return false;
    };
    let (Ok(a), Ok(b)) = (std::fs::metadata(exe), std::fs::metadata(me)) else {
        return false;
    };
    if !a.is_file() || !b.is_file() {
        return false;
    }
    (a.dev() == b.dev() && a.ino() == b.ino()) || (a.len() == b.len() && same_bytes(exe, me))
}

fn open_regular_nonblocking(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

/// Whether two files hold the same bytes (an unreadable one does not).
fn same_bytes(a: &Path, b: &Path) -> bool {
    use std::io::Read as _;
    /// How much of each file one comparison step reads.
    const CHUNK: u64 = 64 * 1024;
    let (Some(fa), Some(fb)) = (open_regular_nonblocking(a), open_regular_nonblocking(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (Vec::new(), Vec::new());
    loop {
        ba.clear();
        bb.clear();
        // Up to the next CHUNK bytes of each file, or fewer only at its end (std's
        // `read_to_end` over a `take`, which retries short reads and interruptions).
        let (Ok(na), Ok(_)) = (
            (&fa).take(CHUNK).read_to_end(&mut ba),
            (&fb).take(CHUNK).read_to_end(&mut bb),
        ) else {
            return false;
        };
        if ba != bb {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

/// The vendor's argv as its grammar reads it (`argv[0]` first), when every word is UTF-8.
fn grammar_argv(build: &Build, args: &[OsString]) -> Option<Vec<String>> {
    std::iter::once(build.exe.clone().into_os_string())
        .chain(args.iter().cloned())
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok()
}

fn now_ms() -> u64 {
    tty::now_us() / 1000
}

fn signal_name(sig: libc::c_int) -> String {
    match sig {
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGTERM => "SIGTERM".into(),
        other => format!("signal {other}"),
    }
}

fn describe(end: End) -> String {
    match end {
        End::Code(c) => format!("exit {c}"),
        End::Signal(s) => signal_name(s),
    }
}

impl Harness {
    /// A harness over `layout`, nested `depth` deep.
    #[must_use]
    pub fn new(env: Env, layout: Layout, cfg: Config, depth: u32) -> Self {
        let journal = Journal::open(&env);
        let me = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::canonicalize(p).ok());
        let tty = Tty::find();
        Self {
            env,
            layout,
            journal,
            cfg,
            mode: tty.and_then(Tty::mode),
            tty,
            me,
            crashes: Vec::new(),
            depth,
            proofs: BTreeMap::new(),
            link: Link::Off,
        }
    }

    /// The harness's journal.
    #[must_use]
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// The marks every process this harness starts as the vendor carries
    /// ([`crate::env::child_marks`]).
    #[must_use]
    pub fn child_marks(&self) -> Vec<(&'static str, String)> {
        env::child_marks(self.depth)
    }

    /// The build's shim environment followed by [`Self::child_marks`].
    fn build_envs<'a>(&self, build: &'a Build) -> Vec<(&'a str, String)> {
        let mut envs: Vec<(&'a str, String)> = build
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        envs.extend(self.child_marks());
        envs
    }

    /// Whether `build` is proved to start ([`crate::probe`]): `(starts, probed_now)`. It
    /// is probed when nothing is known of it, or when its last probe failed at least
    /// `probe_gap` ago; a build that started once stays proved.
    fn prove(&mut self, build: &Build) -> (bool, bool) {
        if let Some(p) = self.proofs.get(&build.version) {
            if p.found == Probe::Starts {
                return (true, false);
            }
            if p.blind || p.at.elapsed() < self.cfg.probe_gap {
                return (false, false);
            }
        }
        let envs = self.build_envs(build);
        let found = probe::probe_within(&build.exe, &envs, self.cfg.probe_wait);
        self.journal.note(
            "PROBE",
            &format!(
                "{PROGRAM} {}: `{} {}` {}",
                build.version,
                build.exe.display(),
                probe::PROBE_ARG,
                found.word()
            ),
        );
        self.proofs.insert(
            build.version,
            Proof {
                found,
                at: Instant::now(),
                blind: false,
            },
        );
        (found == Probe::Starts, true)
    }

    /// The build a relaunch starts: the newest one when it is `known` (the build proved
    /// for the update, or the build the crashed child ran) or is proved to start now;
    /// else `known` itself (or `fallback` when `known` is gone), looked at again, while
    /// the store still holds it.
    fn relaunch_build(&mut self, known: &Build, fallback: &Build) -> Result<Build, String> {
        let newest = self.vendor();
        if let Ok(newest) = &newest
            && (newest.version == known.version
                || (moves_onto(fallback.version, newest.version) && self.prove(newest).0))
        {
            return Ok(newest.clone());
        }
        let check = |b: &Build| -> Result<Build, String> {
            let again = store::build_at(&self.layout, PACKAGE, PROGRAM, &b.dir)
                .map_err(|e| format!("its build {} cannot be run again: {e}", b.version))?;
            if is_self(&again.exe, self.me.as_deref()) {
                return Err(format!(
                    "its build {} is this harness itself; refusing to run it",
                    b.version
                ));
            }
            Ok(again)
        };
        let again = match check(known) {
            Ok(again) => again,
            Err(err) if fallback.version != known.version => {
                self.proofs.remove(&known.version);
                check(fallback).map_err(|_| err)?
            }
            Err(err) => {
                self.proofs.remove(&known.version);
                return Err(err);
            }
        };
        let why = match &newest {
            Ok(n) => format!("the newest build {} is not proved to start", n.version),
            Err(e) => e.clone(),
        };
        self.journal.note(
            "FALLBACK",
            &format!("{why}; relaunching on {} {}", PROGRAM, again.version),
        );
        Ok(again)
    }

    /// The newest runnable vendor build in the store, never this harness.
    ///
    /// # Errors
    /// Why there is none.
    pub fn vendor(&self) -> Result<Build, String> {
        let build = store::current(&self.layout, PACKAGE, PROGRAM)
            .map_err(|e| format!("{PRODUCT} cannot be run from the atpkg store: {e}"))?;
        if is_self(&build.exe, self.me.as_deref()) {
            return Err(format!(
                "the atpkg store's {PROGRAM} is this harness itself ({}); refusing to run it",
                build.exe.display()
            ));
        }
        Ok(build)
    }

    fn fatal(&self, code: i32, msg: &str) -> ! {
        say(&format!("{HARNESS}: {msg}"));
        self.journal.note("FATAL", msg);
        std::process::exit(code)
    }

    /// Put the terminal's starting mode back, when this job is in its foreground.
    fn restore_mode(&self) {
        if let (Some(tty), Some(mode)) = (self.tty, &self.mode)
            && tty.foreground() == Some(true)
            && !tty.set_mode(mode)
        {
            self.journal
                .note("MODE", "could not restore the terminal's mode");
        }
    }

    fn finish(&self, end: End) -> ! {
        if matches!(end, End::Signal(_)) {
            self.restore_mode();
        }
        self.journal.note("END", &describe(end));
        match end {
            End::Code(c) => std::process::exit(c),
            End::Signal(s) => signals::die_like(s),
        }
    }

    fn start(&self, build: Build, args: Vec<OsString>) -> Result<Run, String> {
        let started_ms = now_ms();
        let envs = self.build_envs(&build);
        let pid = child::spawn(&build.exe, &args, &envs)
            .map_err(|e| format!("cannot start {}: {e}", build.exe.display()))?;
        self.journal.note(
            "START",
            &format!(
                "child={pid} {PROGRAM}={} args={}",
                build.version,
                args.len()
            ),
        );
        Ok(Run {
            pid,
            build,
            args,
            started_ms,
            lock: None,
            scanned: None,
            stop: None,
            moved: None,
            term_ignored: false,
        })
    }

    /// The person grace: no move while a person typed more recently than this.
    fn grace(&self) -> Duration {
        Duration::from_micros(self.cfg.timing.person_grace_us)
    }

    /// Run `first` with `args` and supervise it until the session really ends, under `link`'s
    /// word (a proven aterm claim, or none outside aterm).
    pub fn run(mut self, first: Build, args: Vec<OsString>, link: Link) -> ! {
        self.link = link;
        let signals = match signals::install() {
            Ok(s) => s,
            Err(e) => self.fatal(70, &format!("cannot install its signal handlers: {e}")),
        };
        let mut run = match self.start(first, args) {
            Ok(r) => r,
            Err(e) => self.fatal(126, &e),
        };
        let mut ending: Option<libc::c_int> = None;
        let mut last_look: Option<Instant> = None;
        let mut last_word = String::new();
        loop {
            let p = signals.wait(self.cfg.tick);
            if p.hup {
                ending.get_or_insert(libc::SIGHUP);
                signals::send(run.pid, libc::SIGHUP);
                self.journal
                    .note("SIGNAL", "SIGHUP passed on; the session is ending");
            }
            if p.term {
                ending.get_or_insert(libc::SIGTERM);
                signals::send(run.pid, libc::SIGTERM);
                self.journal
                    .note("SIGNAL", "SIGTERM passed on; the session is ending");
            }
            if p.cont {
                signals::send(run.pid, libc::SIGCONT);
            }
            loop {
                match child::poll(run.pid) {
                    Ok(None) => break,
                    Ok(Some(Status::Stopped(sig))) => {
                        // Follow a stop that still holds (a report from before this
                        // process was continued is stale).
                        if procs::is_stopped(run.pid).unwrap_or(!p.cont) {
                            signals::stop_self(sig);
                        }
                    }
                    Ok(Some(Status::Continued)) => {}
                    Ok(Some(Status::Ended(end))) => {
                        run = self.after_end(run, end, ending, &signals);
                        last_look = None;
                        last_word.clear();
                        break;
                    }
                    Err(e) => self.fatal(70, &format!("lost its child {}: {e}", run.pid)),
                }
            }
            if let Some(stop) = &run.stop
                && stop.sent.elapsed() >= self.cfg.stop_wait
            {
                self.journal.note(
                    "STOP-UNHONOURED",
                    &format!(
                        "child={} did not end within {:?} of SIGTERM; it runs on {} and is not signalled again",
                        run.pid, self.cfg.stop_wait, run.build.version
                    ),
                );
                run.stop = None;
                run.term_ignored = true;
            }
            self.link
                .tick(&run.build.version.to_string(), &self.journal);
            if ending.is_none()
                && run.stop.is_none()
                && !run.term_ignored
                && last_look.is_none_or(|t| t.elapsed() >= self.cfg.look_every)
            {
                last_look = Some(Instant::now());
                self.look(&mut run, &mut last_word);
            }
        }
    }

    /// One look: a newer build installed, the safe point reached and the new build proved
    /// to start, stops the child.
    fn look(&mut self, run: &mut Run, last_word: &mut String) {
        let Ok(target) = self.vendor() else {
            return;
        };
        if !moves_onto(run.build.version, target.version) {
            return;
        }
        let failed_lately = self.proofs.get(&target.version).is_some_and(|p| {
            p.found != Probe::Starts && (p.blind || p.at.elapsed() < self.cfg.probe_gap)
        });
        // A build whose probe failed lately holds before the safe point is read at all.
        let checked = if failed_lately {
            Err("build-unproved")
        } else {
            self.safe_point(run)
        };
        let mut detail = None;
        let word = match checked {
            Err(word) => word,
            Ok((session, terminal)) => match self.prove(&target) {
                (false, _) => "build-unproved",
                // Proved just now: the probe took time, so the next look reads the whole
                // safe point again before any signal.
                (true, true) => "build-proved",
                (true, false) => {
                    // The last re-check, just before the signal: no input since the look,
                    // still the foreground, still no tool running.
                    let now = self.terminal();
                    let grace = self.grace();
                    let vendor = run.build.version.to_string();
                    if now != Some(terminal) || procs::descendants(run.pid) != Some(0) {
                        "changed-before-signal"
                    } else if let Err((word, why)) =
                        self.link
                            .may_act(Duty::Upgrade, grace, &vendor, &self.journal)
                    {
                        detail = Some(why);
                        word
                    } else if signals::send(run.pid, libc::SIGTERM) {
                        self.journal.note(
                            "STOP",
                            &format!(
                                "child={} SIGTERM at a safe point: {} -> {}, session {session}",
                                run.pid, run.build.version, target.version
                            ),
                        );
                        run.stop = Some(Stop {
                            from: run.build.clone(),
                            to: target,
                            session,
                            sent: Instant::now(),
                        });
                        return;
                    } else {
                        "signal-refused"
                    }
                }
            },
        };
        if *last_word != word {
            self.journal.note(
                "WAIT",
                &format!(
                    "child={} update {} -> {} waits: {word}{}",
                    run.pid,
                    run.build.version,
                    target.version,
                    detail.map_or_else(String::new, |d| format!(" ({d})"))
                ),
            );
            *last_word = word.to_string();
        }
    }

    fn terminal(&self) -> Option<Terminal> {
        self.tty.map(|t| Terminal {
            last_input_us: t.last_input_us(),
            foreground: t.foreground(),
            pending_input: t.pending_input(),
        })
    }

    /// The safe point, read cheapest first ([`gate::gate`]'s order): the session and the
    /// terminal reading it was decided on, or the hold.
    fn safe_point(&self, run: &mut Run) -> Result<(String, Terminal), &'static str> {
        let resumable = grammar_argv(&run.build, &run.args)
            .ok_or_else(|| "non-utf8".to_string())
            .and_then(|a| argv::rewrite_argv(&a, None).map_err(|e| e.to_string()))
            .map(|_| ());
        gate::check_argv(&resumable)?;
        let terminal = self.terminal();
        let now = tty::now_us();
        gate::check_terminal(terminal, now, &self.cfg.timing)?;
        gate::check_children(procs::descendants(run.pid))?;
        let witnessed = self.witness(run);
        gate::check_session(witnessed.as_ref().map(|(s, _)| s.as_str()))?;
        let Some((session, lock)) = witnessed else {
            return Err("no-session");
        };
        let log = session::log_look(&lock, run.pid, run.started_ms);
        let terminal = terminal.ok_or("no-tty")?;
        gate::check_log(&log, terminal.last_input_us, now, &self.cfg.timing)?;
        Ok((session, terminal))
    }

    /// The session the child holds: the cached lock while it still names the child, else
    /// a search of the sessions root (at most once per `scan_every`).
    fn witness(&self, run: &mut Run) -> Option<(String, PathBuf)> {
        if let Some((s, lock)) = &run.lock
            && session::lock_names(lock, run.pid)
        {
            return Some((s.clone(), lock.clone()));
        }
        if run
            .scanned
            .is_some_and(|t| t.elapsed() < self.cfg.scan_every)
        {
            return None;
        }
        run.scanned = Some(Instant::now());
        let home = self.env.home()?;
        let data = session::data_home(self.env.xdg_data_home.as_deref(), home)?;
        run.lock = session::witness(&session::sessions_root(&data), run.pid, run.started_ms);
        run.lock.clone()
    }

    /// The child ended: relaunch it, or end the harness as it ended. A crash relaunch may wait
    /// on aterm's word ([`Harness::relaunch_word`]), watching `signals` meanwhile.
    fn after_end(
        &mut self,
        mut run: Run,
        end: End,
        ending: Option<libc::c_int>,
        signals: &signals::Signals,
    ) -> Run {
        let kind = muse::exit_kind(end.shell_status());
        self.journal.note(
            "EXIT",
            &format!("child={} {} ({})", run.pid, describe(end), kind.word()),
        );
        if ending.is_some() {
            self.finish(end);
        }
        if let Some(stop) = run.stop.take() {
            let prev = stop.from;
            let from = prev.version;
            let session = stop.session.clone();
            let moved = match self.relaunch_plan(&run, &session, &stop.to, &run.build) {
                Ok((build, args)) => {
                    let msg = format!(
                        "moving {PRODUCT} {from} -> {}, resuming session {session}",
                        build.version
                    );
                    self.relaunch_start(build, &run.build, args, &msg)
                }
                Err(why) => Err(why),
            };
            match moved {
                Ok(mut next) => {
                    if next.build.version != from {
                        next.moved = Some((prev, session, Instant::now()));
                    }
                    return next;
                }
                Err(why) => {
                    say(&format!(
                        "{HARNESS}: {PRODUCT} stopped for its update but cannot be resumed: {why}"
                    ));
                    self.journal.note("NO-RELAUNCH", &why);
                    self.finish(end);
                }
            }
        }
        if let Some((prev, session, at)) = run.moved.take()
            && at.elapsed() <= STARTUP_GRACE
            && refused_start(end)
        {
            run.scanned = None;
            let clean_log = self
                .witness(&mut run)
                .is_some_and(|(_, lock)| session::ended_clean(&lock, run.pid, run.started_ms));
            if !clean_log {
                self.proofs.insert(
                    run.build.version,
                    Proof {
                        found: Probe::Exited,
                        at: Instant::now(),
                        blind: true,
                    },
                );
                let msg = format!(
                    "relaunch on {PRODUCT} {} ended ({}) on startup; falling back to {}, resuming session {session}",
                    run.build.version,
                    describe(end),
                    prev.version
                );
                let fallback = self
                    .relaunch_plan(&run, &session, &prev, &prev)
                    .and_then(|(build, args)| self.relaunch_start(build, &prev, args, &msg));
                match fallback {
                    Ok(mut next) => {
                        if next.build.version != prev.version {
                            next.moved = Some((prev, session, Instant::now()));
                        }
                        return next;
                    }
                    Err(why) => {
                        say(&format!("{HARNESS}: {PRODUCT} cannot be resumed: {why}"));
                        self.journal.note("NO-RELAUNCH", &why);
                        self.finish(end);
                    }
                }
            }
        }
        if kind != ExitKind::Crashed {
            self.finish(end);
        }
        run.scanned = None;
        let Some((session, lock)) = self.witness(&mut run) else {
            self.journal
                .note("NO-RELAUNCH", "the crashed child held no witnessed session");
            self.finish(end);
        };
        if session::ended_clean(&lock, run.pid, run.started_ms) {
            self.journal.note(
                "NO-RELAUNCH",
                &format!(
                    "{} is not a clean exit status, but the child's session log ends clean",
                    describe(end)
                ),
            );
            self.finish(end);
        }
        let now = Instant::now();
        let window = self.cfg.crash_window;
        self.crashes.retain(|t| now.duration_since(*t) < window);
        if self.crashes.len() >= self.cfg.crash_budget {
            let why = format!(
                "{PRODUCT} ended abnormally ({}) {} times within {} s; not relaunching it again",
                describe(end),
                self.crashes.len() + 1,
                window.as_secs()
            );
            say(&format!("{HARNESS}: {why}"));
            self.journal.note("NO-RELAUNCH", &why);
            self.finish(end);
        }
        let vendor = run.build.version.to_string();
        if let Err(why) = self.relaunch_word(signals, &vendor) {
            self.journal.note("NO-RELAUNCH", &why);
            self.finish(end);
        }
        let (ran, nth, budget) = (
            run.build.version,
            self.crashes.len() + 1,
            self.cfg.crash_budget,
        );
        let secs = window.as_secs();
        let known = run.build.clone();
        let resumed = match self.relaunch_plan(&run, &session, &known, &run.build) {
            Ok((build, args)) => {
                let on = build.version;
                let onto = if on == ran {
                    String::new()
                } else {
                    format!(" on {on}")
                };
                let msg = format!(
                    "{PRODUCT} ended abnormally ({}); resuming session {session}{onto} (relaunch {nth} of {budget} within {secs} s)",
                    describe(end)
                );
                self.relaunch_start(build, &run.build, args, &msg)
            }
            Err(why) => Err(why),
        };
        match resumed {
            Ok(mut next) => {
                if next.build.version != ran {
                    next.moved = Some((known, session, Instant::now()));
                }
                self.crashes.push(now);
                next
            }
            Err(why) => {
                say(&format!("{HARNESS}: {PRODUCT} cannot be resumed: {why}"));
                self.journal.note("NO-RELAUNCH", &why);
                self.finish(end);
            }
        }
    }

    /// ATERM'S WORD ON A CRASH RELAUNCH ([`Link::may_act`]), waited for while the claim is away.
    /// A claim that lapsed (aterm closed its connection, as a restart or a seamless update does,
    /// or no longer shows it proven) or a read aterm did not answer holds the relaunch up to
    /// `reclaim_hold`, the claim made again every [`crate::aterm::RECLAIM_EVERY`] and the
    /// relaunch judged again whenever aterm proves it: in a tab this harness leads, no built-in
    /// lane of aterm's brings the child back (aterm sees no exit, and its relaunch lane knows no
    /// conversation for the harness's pid), so a crash passed back at once would leave the
    /// session to nobody. A policy found off or a `[harness] relaunch` that is off passes the
    /// crash back at once; so does the hold running out, or a `SIGHUP` or `SIGTERM` reaching the
    /// harness meanwhile.
    ///
    /// # Errors
    /// Why the crash is passed back.
    fn relaunch_word(&mut self, signals: &signals::Signals, vendor: &str) -> Result<(), String> {
        let grace = self.grace();
        let hold = self.cfg.reclaim_hold;
        let deadline = Instant::now() + hold;
        let mut said = false;
        loop {
            let (word, why) = match self
                .link
                .may_act(Duty::Relaunch, grace, vendor, &self.journal)
            {
                Ok(()) => return Ok(()),
                Err(held) => held,
            };
            if !matches!(word, "no-claim" | "aterm-unread") {
                return Err(format!("aterm's word holds it: {why}"));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "aterm's word holds it: {why}; aterm did not prove this harness's claim again within {} s",
                    hold.as_secs()
                ));
            }
            if !said {
                self.journal.note(
                    "HOLD",
                    &format!(
                        "the crash relaunch waits up to {} s for aterm to prove this harness's claim again: {why}",
                        hold.as_secs()
                    ),
                );
                said = true;
            }
            // Until the next judgement: the claim proven again, else one reclaim period.
            let next = (Instant::now() + aterm::RECLAIM_EVERY).min(deadline);
            let was_held = self.link.is_held();
            while Instant::now() < next {
                let p = signals.wait(self.cfg.tick);
                if p.hup || p.term {
                    return Err(format!(
                        "{} reached the harness while the crash relaunch waited for aterm",
                        if p.hup { "SIGHUP" } else { "SIGTERM" }
                    ));
                }
                self.link.tick(vendor, &self.journal);
                if !was_held && self.link.is_held() {
                    break;
                }
            }
        }
    }

    /// What a relaunch on `session` starts: a proved build ([`Harness::relaunch_build`]
    /// over `known` with `fallback`) and the launch's resume argv.
    fn relaunch_plan(
        &mut self,
        run: &Run,
        session: &str,
        known: &Build,
        fallback: &Build,
    ) -> Result<(Build, Vec<String>), String> {
        let launch = grammar_argv(&run.build, &run.args)
            .ok_or("its launch has an argument that is not UTF-8")?;
        let args = argv::rewrite_argv(&launch, Some(session))
            .map_err(|e| format!("its launch cannot be resumed ({e})"))?;
        let build = self.relaunch_build(known, fallback)?;
        Ok((build, args))
    }

    /// Start the relaunch [`Harness::relaunch_plan`] chose, saying `msg` first, and
    /// falling back to `fallback` if spawning a newer `build` fails.
    fn relaunch_start(
        &mut self,
        build: Build,
        fallback: &Build,
        args: Vec<String>,
        msg: &str,
    ) -> Result<Run, String> {
        self.restore_mode();
        say(&format!("{HARNESS}: {msg}"));
        self.journal.note(
            "RELAUNCH",
            &format!(
                "{msg} ({PROGRAM}={} resume-args={})",
                build.version,
                args.len()
            ),
        );
        let os_args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
        let target_version = build.version;
        let started = match self.start(build, os_args.clone()) {
            Ok(next) => Ok(next),
            Err(err) if fallback.version != target_version => {
                self.proofs.insert(
                    target_version,
                    Proof {
                        found: Probe::NoExec,
                        at: Instant::now(),
                        blind: true,
                    },
                );
                let again = store::build_at(&self.layout, PACKAGE, PROGRAM, &fallback.dir)
                    .ok()
                    .filter(|b| !is_self(&b.exe, self.me.as_deref()))
                    .ok_or(err)?;
                self.journal.note(
                    "FALLBACK",
                    &format!(
                        "relaunch on {PROGRAM} {target_version} failed to spawn; falling back to {}",
                        again.version
                    ),
                );
                self.start(again, os_args)
            }
            Err(err) => Err(err),
        };
        // A held claim names the build the session now runs (`vendor=`).
        if let Ok(next) = &started {
            self.link
                .announce(&next.build.version.to_string(), &self.journal);
        }
        started
    }
}

/// Whether `end`, soon after a move started a new build, is that build failing to start:
/// any non-zero exit or signal death that is not a terminal or job signal's (`129` `SIGHUP`,
/// `130` `SIGINT`, `131` `SIGQUIT`, `143` `SIGTERM`).
fn refused_start(end: End) -> bool {
    !matches!(end.shell_status(), 0 | 129 | 130 | 131 | 143)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_start_excludes_clean_and_user_signal_exits() {
        assert!(!refused_start(End::Code(0)));
        for code in [129, 130, 131, 143] {
            assert!(!refused_start(End::Code(code)), "{code}");
        }
        for code in [1, 2, 126, 127] {
            assert!(refused_start(End::Code(code)), "{code}");
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
            assert!(!refused_start(End::Signal(sig)), "{sig}");
        }
        for sig in [libc::SIGABRT, libc::SIGILL, libc::SIGSEGV, libc::SIGKILL] {
            assert!(refused_start(End::Signal(sig)), "{sig}");
        }
    }

    #[test]
    fn the_test_timing_parses_only_whole() {
        let c = Config::parse_test_timing("50,100,300,200,2000").expect("five numbers");
        assert_eq!(c.tick, Duration::from_millis(50));
        assert_eq!(c.timing.person_grace_us, 300_000);
        assert_eq!(c.timing.settle_us, 200_000);
        assert_eq!(c.stop_wait, Duration::from_secs(2));
        assert_eq!(c.crash_budget, Config::PRODUCTION.crash_budget);
        assert_eq!(c.reclaim_hold, Config::PRODUCTION.reclaim_hold);
        let held = Config::parse_test_timing("50,100,300,200,2000,1500").expect("six numbers");
        assert_eq!(held.reclaim_hold, Duration::from_millis(1500));
        assert_eq!(Config::parse_test_timing("1,2,3"), None);
        assert_eq!(Config::parse_test_timing("1,2,3,4,5,6,7"), None);
        assert_eq!(Config::parse_test_timing("1,2,3,4,x"), None);
    }

    #[test]
    fn the_harness_never_runs_itself() {
        assert!(is_self(Path::new("/x/bin/muse-alab"), None));
        assert!(!is_self(Path::new("/x/bin/muse"), None));
        let me = std::fs::canonicalize(std::env::current_exe().expect("exe")).expect("canon");
        assert!(is_self(&me, Some(&me)));
        let dir = std::env::temp_dir().join(format!("muse-alab-is-self-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let copy = dir.join("muse");
        std::fs::copy(&me, &copy).expect("copy");
        assert!(is_self(&copy, Some(&me)), "a byte-for-byte copy");
        let other = dir.join("other");
        std::fs::write(&other, b"#!/bin/sh\n").expect("write");
        assert!(!is_self(&other, Some(&me)));
        assert!(!is_self(&dir, Some(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
