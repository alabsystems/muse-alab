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
//! - A child that crashed (aterm's Muse Code exit table: not clean, not a startup refusal;
//!   and abnormal by `alab_wire::relaunch::abnormal_exit`) is relaunched on its session the
//!   same way after aterm's back-off ladder's pause (1 s, 10 s, 60 s, then 10 minutes; a
//!   child that ran 10 minutes starts it over, [`alab_wire::relaunch::Ladder`]), on the
//!   newest build when it is proved to start and the claim takes `upgrade`, else on the build
//!   it ran. Under aterm the relaunch waits on aterm's word ([`Harness::crash_wait`]), up to
//!   aterm's 45 s while aterm is away or answers only later; a `SIGINT`, `SIGQUIT`, `SIGHUP`
//!   or `SIGTERM` meanwhile passes the crash on, and keys typed meanwhile are dropped. In a tab
//!   where aterm never admitted a claim of this harness's, or once aterm answered a claim made
//!   again that it serves no contract this harness declares, the crash is passed on at once.
//! - A restart aterm asks the claim holder for (`restart cause=asked|stall`, a person's
//!   `aterm ctl restart` or the stall's remedy) ends the child with `SIGTERM` (and `SIGKILL`
//!   if it does not end, as aterm's own restart does) and resumes the same session, on the
//!   model aterm names when it names one; it waits a bounded time for the session and for
//!   aterm's word, and one meant for an earlier child is passed over ([`Harness::restarts`]).
//!   A restart onto another account is refused: Muse Code takes no account home this
//!   harness can set. Every child start re-announces the claim with the new child
//!   ([`Harness::announce`]).
//! - Any other end — a clean exit, a refusal, a crash it cannot resume, `SIGHUP` or
//!   `SIGTERM` to the harness — ends the harness with the child's real status: the same
//!   exit code, or death by the same signal.
//! - It watches its own program's `current` too. When atpkg has installed a newer build of the
//!   harness itself, it waits for the same safe point, proves the new image speaks its adoption
//!   protocol, and `execve`s it in this process with the child and its episode carried across
//!   ([`crate::adopt`], [`Harness::adopt`]): the same pid, the same child, never signalled.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alab_wire::link::LATER_RETRY_MS;
use alab_wire::relaunch::{AWAY_BUDGET, AwayBudget, ChildEnd, Ladder, abnormal_exit};
use alab_wire::{Child, ChildProcess, Failure, Link, Relaunch, Restart};

use crate::adopt::{self, Carried};
use crate::child::{self, End, Status};
use crate::env::{self, Env};
use crate::gate::{self, ComposerWitness, Terminal, Timing};
use crate::journal::Journal;
use crate::muse::{self, ExitKind, PACKAGE, PRODUCT, PROGRAM, argv, session};
use crate::probe::{self, Probe};
use crate::store::{self, Build, Layout, OwnBuild};
use crate::tty::{self, Mode, Tty};
use crate::version::{Version, moves_onto};
use crate::{HARNESS, HARNESS_VERSION, aterm, procs, say, signals};

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
    /// How long a child gets to end after a move's `SIGTERM`.
    pub stop_wait: Duration,
    /// How long a child gets to end after a restart's `SIGTERM` before its `SIGKILL` (a stalled
    /// one gets at most [`STALL_KILL_AFTER`]), and after that `SIGKILL` before the restart is
    /// abandoned.
    pub term_grace: Duration,
    /// How long a build probe may run ([`crate::probe`]).
    pub probe_wait: Duration,
    /// How long after a failed probe the same build is probed again.
    pub probe_gap: Duration,
    /// How long one crash relaunch waits in all for aterm to answer again
    /// ([`AwayBudget`]), and how long a restart aterm asked for waits from its arrival for the
    /// session and aterm's word ([`Deferred`]): aterm's 45 s.
    pub away_budget: Duration,
}

impl Config {
    /// The production clocks.
    pub const PRODUCTION: Config = Config {
        timing: Timing::PRODUCTION,
        tick: Duration::from_millis(500),
        look_every: Duration::from_secs(2),
        scan_every: Duration::from_secs(30),
        stop_wait: Duration::from_secs(15),
        // aterm's restart act's deadline (`restart_act.rs`).
        term_grace: Duration::from_secs(30),
        probe_wait: probe::PROBE_WAIT,
        probe_gap: probe::PROBE_GAP,
        // aterm's grace for a claim whose connection went away (`harness_host.rs`).
        away_budget: AWAY_BUDGET,
    };

    /// The clocks this build runs with: production, except that a DEBUG build reads
    /// `MUSE_ALAB_TEST_TIMING=<tick>,<look>,<person grace>,<settle>,<stop wait>[,<away
    /// budget>]` (ms) so the integration tests run in seconds; the stop wait is a restart's
    /// term grace too. A release build reads no such variable, and the crash ladder's pauses
    /// are aterm's in every build.
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
        let (tick, look, grace, settle, stop, away) = match ms.as_slice() {
            [tick, look, grace, settle, stop] => (tick, look, grace, settle, stop, None),
            [tick, look, grace, settle, stop, away] => {
                (tick, look, grace, settle, stop, Some(*away))
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
            term_grace: Duration::from_millis(*stop),
            away_budget: away.map_or(Self::PRODUCTION.away_budget, Duration::from_millis),
            ..Self::PRODUCTION
        })
    }
}

/// How long after a move's relaunch an abnormal or refused exit counts as the new build
/// failing to start the resumed session (falling back onto the previous build).
const STARTUP_GRACE: Duration = Duration::from_secs(15);

/// aterm's grace before a stalled program's `SIGKILL` (`crates/aterm-gui/src/input_stall.rs`
/// `RESTART_GRACE`): a stalled program reads nothing, so it may never act on `SIGTERM`.
const STALL_KILL_AFTER: Duration = Duration::from_secs(5);

/// How soon aterm is asked again about a crash or an asked restart while it could not answer.
const ASK_AGAIN: Duration = Duration::from_millis(LATER_RETRY_MS);

/// The running child.
#[derive(Debug)]
struct Run {
    pid: u32,
    build: Build,
    /// Its build as the claim's `vendor=` names it.
    vendor: String,
    /// Its kernel start, as the claim's `child=` names it (`None`: unreadable).
    process: Option<ChildProcess>,
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

impl Run {
    /// What the claim says about this child.
    fn claim(&self) -> Child<'_> {
        aterm::child(Some(&self.vendor), self.process)
    }
}

#[derive(Debug)]
struct Stop {
    from: Build,
    to: Build,
    session: String,
    /// When the last signal was sent, on a clock that stops while the machine sleeps (std's
    /// `Instant` on macOS). Time the child spends stopped does not count either: a stopped child
    /// acts on no signal but `SIGKILL`, so [`Harness::continued`] starts the wait again whenever
    /// the child is continued.
    sent: Instant,
    /// For a restart aterm asked for (not a move), how it goes on.
    restart: Option<Escalation>,
}

/// A restart aterm asked for, under way: made whatever the child does with its `SIGTERM`, as
/// aterm's own restart act sends `SIGKILL` to a program still running after its grace
/// (`crates/aterm-gui/src/restart_act.rs` `escalate`), which under this harness's `relaunch`
/// claim aterm leaves to the holder. A move's `SIGTERM` is never followed by one: a child
/// that ignores it runs on.
#[derive(Debug)]
struct Escalation {
    /// aterm's cause.
    cause: String,
    /// The resume arguments, on aterm's model when it named one.
    args: Vec<String>,
    /// How long after the `SIGTERM` the `SIGKILL` follows.
    kill_after: Duration,
    /// Whether the `SIGKILL` was sent.
    killed: bool,
}

impl Stop {
    /// How long after [`Stop::sent`] the child must have ended.
    fn limit(&self, cfg: &Config) -> Duration {
        match &self.restart {
            None => cfg.stop_wait,
            Some(escalation) if !escalation.killed => escalation.kill_after,
            Some(_) => cfg.term_grace,
        }
    }
}

/// A restart aterm asked for that waits: for the child's session, or for aterm to answer. It
/// waits at most [`Config::away_budget`] from its arrival, whatever it waits for: a restart is
/// made for the moment aterm asked in, and one made much later would end the child mid-turn
/// for a cause long gone. aterm asks again while its cause stands (a stall's remedy fires
/// again).
#[derive(Debug)]
struct Deferred {
    event: Restart,
    /// When it is refused if it still waits.
    expires: Instant,
    /// When aterm is asked again.
    next_ask: Instant,
    /// Why it waits, journalled once.
    why: String,
}

/// What a restart aterm asked for came to.
enum Asked {
    /// The child was sent `SIGTERM`; the restart is under way.
    Made,
    /// It was refused, and journalled.
    Refused,
    /// It waits, and why.
    Waits(String),
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
    /// aterm's crash back-off ([`Ladder`]), on [`aterm::now_ms`].
    ladder: Ladder,
    /// This harness's nesting depth ([`crate::env::nesting_depth`]); every process it
    /// starts as the vendor carries `depth + 1`.
    depth: u32,
    /// The builds probed, and what was found.
    proofs: BTreeMap<Version, Proof>,
    /// The harness's link to aterm (`alab-wire`): every act under a claim waits on its word.
    link: Link,
    /// This process runs in an aterm tab ([`aterm::hosted`]).
    hosted: bool,
    /// A restart aterm asked for that waits ([`Harness::restarts`]).
    deferred: Option<Deferred>,
    /// The store build this image is ([`store::own_build_of`]); `None` outside the store.
    own: Option<u64>,
    /// The last probe of a newer build of the harness itself, by build number.
    own_proof: Option<(u64, Proof)>,
    /// The last word the look at the harness's own update said.
    own_word: String,
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

/// `end` as `alab-wire` judges an exit.
fn child_end(end: End) -> ChildEnd {
    match end {
        End::Code(c) => ChildEnd::Exited(c),
        End::Signal(s) => ChildEnd::Signaled(s),
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
        let own = me
            .as_deref()
            .and_then(|me| store::own_build_of(&layout, HARNESS, HARNESS, me));
        let config = aterm::config();
        let hosted = aterm::hosted(&env, &config);
        Self {
            env,
            layout,
            journal,
            cfg,
            mode: tty.and_then(Tty::mode),
            tty,
            me,
            ladder: Ladder::new(),
            depth,
            proofs: BTreeMap::new(),
            link: Link::absent(config, std::process::id()),
            hosted,
            deferred: None,
            own,
            own_proof: None,
            own_word: String::new(),
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
    /// for the update, or the build the crashed child ran) or is proved to start now and the
    /// claim takes `upgrade` ([`aterm::upgrade_granted`]: a newer build is a move); else
    /// `known` itself (or `fallback` when `known` is gone), looked at again, while the store
    /// still holds it.
    fn relaunch_build(&mut self, known: &Build, fallback: &Build) -> Result<Build, String> {
        let newest = self.vendor();
        let may_move = aterm::upgrade_granted(&self.link, self.hosted);
        if let Ok(newest) = &newest
            && (newest.version == known.version
                || (may_move
                    && moves_onto(fallback.version, newest.version)
                    && self.prove(newest).0))
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
            Ok(n) if !may_move && moves_onto(fallback.version, n.version) => format!(
                "aterm's claim does not take upgrade (scope={}), so the newest build {} waits",
                aterm::granted(&self.link),
                n.version
            ),
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
            vendor: build.version.to_string(),
            process: aterm::process_of(pid),
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
    /// word (an aterm claim, held or being made, or none outside aterm). The first start
    /// re-announces the claim with the child it now names ([`Harness::announce`]).
    pub fn run(mut self, first: Build, args: Vec<OsString>, link: Link) -> ! {
        self.link = link;
        let signals = match signals::install() {
            Ok(s) => s,
            Err(e) => self.fatal(70, &format!("cannot install its signal handlers: {e}")),
        };
        let run = match self.start(first, args) {
            Ok(r) => r,
            Err(e) => self.fatal(126, &e),
        };
        self.announce(&run);
        self.supervise(&signals, run, signals::Pending::default())
    }

    /// THE NEW IMAGE after the harness's own re-exec ([`crate::adopt`]): take over the child the
    /// old image ran, with its episode, and supervise it as the old image would have; the
    /// signals posted across the exec are acted on first, and a claim the old image held is made
    /// again at once under this image's version.
    pub fn adopt(mut self, carried: Carried) -> ! {
        let signals = match signals::install() {
            Ok(s) => s,
            Err(e) => self.fatal(70, &format!("cannot install its signal handlers: {e}")),
        };
        let arrived = signals::take_blocked();
        let now = Instant::now();
        let ago = |age: Duration| now.checked_sub(age).unwrap_or(now);
        if carried.mode.is_some() {
            self.mode = carried.mode;
        }
        let (streak, since) = carried.ladder;
        self.ladder = Ladder::carried(streak, since, aterm::now_ms());
        self.proofs = carried
            .proofs
            .iter()
            .map(|(v, found, age, blind)| {
                (
                    *v,
                    Proof {
                        found: *found,
                        at: ago(*age),
                        blind: *blind,
                    },
                )
            })
            .collect();
        let run = Run {
            pid: carried.child,
            vendor: carried.build.version.to_string(),
            process: aterm::process_of(carried.child),
            build: carried.build,
            args: carried.args,
            started_ms: carried.started_ms,
            lock: carried.lock,
            scanned: None,
            stop: None,
            moved: carried
                .moved
                .map(|(prev, session, age)| (prev, session, ago(age))),
            term_ignored: carried.term_ignored,
        };
        self.journal.note(
            "ADOPTED",
            &format!(
                "{HARNESS} {HARNESS_VERSION} (build {}) took over child={} {PROGRAM}={} from {HARNESS} {}",
                self.own.map_or_else(|| "-".to_string(), |n| n.to_string()),
                run.pid,
                run.build.version,
                carried.from
            ),
        );
        // The old image's claim ended with its connections at the exec, its duty towards the
        // child did not: claimed again at once under this image's version, with the child.
        let config = aterm::config();
        self.link = if carried.linked {
            Link::adopt_with(
                config,
                std::process::id(),
                run.claim(),
                aterm::now_ms(),
                aterm::lookup(&self.env),
            )
        } else {
            Link::absent(config, std::process::id())
        };
        aterm::journal(&mut self.link, &self.journal);
        self.supervise(&signals, run, arrived)
    }

    /// THE LOOP over `run` until the session ends, `arrived` acted on first.
    fn supervise(
        mut self,
        signals: &signals::Signals,
        mut run: Run,
        arrived: signals::Pending,
    ) -> ! {
        let mut ending: Option<libc::c_int> = None;
        let mut last_look: Option<Instant> = None;
        let mut last_word = String::new();
        let mut carried = arrived;
        loop {
            let p = if carried == signals::Pending::default() {
                signals.wait(self.cfg.tick)
            } else {
                signals
                    .wait(Duration::ZERO)
                    .or(std::mem::take(&mut carried))
            };
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
            // The harness continued (after it followed the child's stop, or a whole-job `^Z`
            // stopped both): it continues the child too.
            let mut continued = p.cont;
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
                    // A continue the harness may never have seen a stop for: a whole-job `^Z`
                    // and the shell's `fg` stop and continue both, and only the continue is
                    // reported once the harness runs again.
                    Ok(Some(Status::Continued)) => continued = true,
                    Ok(Some(Status::Ended(end))) => {
                        run = self.after_end(run, end, ending, p.int, signals);
                        last_look = None;
                        last_word.clear();
                        break;
                    }
                    Err(e) => self.fatal(70, &format!("lost its child {}: {e}", run.pid)),
                }
            }
            if continued {
                self.continued(&mut run);
            }
            // A child still stopped is not judged: the harness continued alone after following
            // its stop (`kill -CONT` on the harness's pid) has spent the stop on the clock, and
            // continues the child on its next pass, which starts the wait again.
            if let Some(stop) = &run.stop
                && stop.sent.elapsed() >= stop.limit(&self.cfg)
                && procs::is_stopped(run.pid) != Some(true)
            {
                self.unhonoured(&mut run);
            }
            let now = aterm::now_ms();
            self.link.tick(run.claim(), now);
            let drained = self.link.events(run.claim(), now);
            aterm::journal(&mut self.link, &self.journal);
            self.restarts(&mut run, drained.restart, ending);
            if ending.is_none()
                && run.stop.is_none()
                && self.deferred.is_none()
                && last_look.is_none_or(|t| t.elapsed() >= self.cfg.look_every)
            {
                last_look = Some(Instant::now());
                if let Some(own) = self.look(&mut run, &mut last_word) {
                    carried = self.reexec(&run, &own, signals);
                }
            }
        }
    }

    /// The child did not end within its stop's limit. A move's child runs on, never signalled
    /// again; a restart's gets `SIGKILL`, and the restart is abandoned if even that does not end
    /// it.
    fn unhonoured(&self, run: &mut Run) {
        let Some(stop) = run.stop.as_mut() else {
            return;
        };
        match stop.restart.as_mut() {
            None => {
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
            Some(escalation) if !escalation.killed => {
                self.journal.note(
                    "RESTART-KILL",
                    &format!(
                        "cause={} child={} did not end within {} ms of SIGTERM; SIGKILL",
                        escalation.cause,
                        run.pid,
                        escalation.kill_after.as_millis()
                    ),
                );
                signals::send(run.pid, libc::SIGKILL);
                escalation.killed = true;
                stop.sent = Instant::now();
            }
            Some(escalation) => {
                self.journal.note(
                    "RESTART-ABANDONED",
                    &format!(
                        "cause={} child={} still runs {} ms after SIGKILL",
                        escalation.cause,
                        run.pid,
                        self.cfg.term_grace.as_millis()
                    ),
                );
                run.stop = None;
            }
        }
    }

    /// THE RESTARTS aterm asks for, on every pass of the loop while a child runs: a new request
    /// (`asked`, drained from aterm's event stream) replaces one still waiting, and the one
    /// waiting is tried when it is due ([`Harness::restart_now`]), each at most
    /// [`Config::away_budget`] after it arrived. While the session ends or the child is already
    /// being ended, a new request is passed over: aterm sent it for this child, which is going.
    fn restarts(&mut self, run: &mut Run, asked: Option<Restart>, ending: Option<libc::c_int>) {
        let now = Instant::now();
        let busy = ending.is_some() || run.stop.is_some();
        if let Some(event) = asked {
            if busy {
                self.journal.note(
                    "RESTART-PASSED-OVER",
                    &format!("cause={} the child is already being ended", event.cause),
                );
            } else if let Some(old) = self.deferred.replace(Deferred {
                event,
                expires: now + self.cfg.away_budget,
                next_ask: now,
                why: String::new(),
            }) {
                self.journal.note(
                    "RESTART-PASSED-OVER",
                    &format!("cause={} a newer restart replaced it", old.event.cause),
                );
            }
        }
        if busy {
            return;
        }
        let Some(mut waiting) = self.deferred.take() else {
            return;
        };
        // Its limit is judged before aterm is asked again, so a pass that comes late (the job
        // stopped with `^Z`, which `Instant` counts) refuses it rather than make it then.
        if now >= waiting.expires {
            self.journal.note(
                "RESTART-REFUSED",
                &format!(
                    "cause={} not made within {} ms: {}",
                    waiting.event.cause,
                    self.cfg.away_budget.as_millis(),
                    waiting.why
                ),
            );
            return;
        }
        if now < waiting.next_ask {
            self.deferred = Some(waiting);
            return;
        }
        match self.restart_now(run, &waiting.event) {
            Asked::Made | Asked::Refused => {}
            Asked::Waits(why) => {
                if waiting.why != why {
                    self.journal.note(
                        "RESTART-WAITS",
                        &format!("cause={} {why}", waiting.event.cause),
                    );
                    waiting.why = why;
                }
                waiting.next_ask = now + ASK_AGAIN;
                self.deferred = Some(waiting);
            }
        }
    }

    /// aterm asked the claim holder to restart its child (`restart cause=<cause>`): end the
    /// child with `SIGTERM` now (and `SIGKILL` if it does not end, [`Escalation`]), and resume
    /// the same session when it has ended. The request is aterm's, or a person's through aterm,
    /// so no safe point is waited for, and it is no crash: the ladder is not touched.
    ///
    /// It is made only when the child's session is witnessed, the resume argv can be written
    /// (on the model aterm names, Muse Code's `--model`), the restart moves no account (Muse
    /// Code takes no account home this harness can set: refused, and the child runs on), and
    /// one fresh policy and status read permits it (`Link::relaunch_word` with `asked`: the
    /// claim is this harness's, proven and takes `relaunch`, and nobody holds the session). A
    /// request with no session yet, or one aterm could not answer, waits.
    fn restart_now(&mut self, run: &mut Run, event: &Restart) -> Asked {
        let cause = &event.cause;
        if let Some(account) = &event.account {
            self.journal.note(
                "RESTART-REFUSED",
                &format!(
                    "cause={cause} to={}: {PRODUCT} takes no account home {HARNESS} can set; the child runs on",
                    account.label
                ),
            );
            return Asked::Refused;
        }
        let Some((session, _)) = self.witness(run) else {
            return Asked::Waits("no session lock names the child's session yet".to_string());
        };
        let resume = grammar_argv(&run.build, &run.args)
            .ok_or_else(|| "its launch has an argument that is not UTF-8".to_string())
            .and_then(|launch| {
                argv::rewrite_argv(&launch, Some(&session))
                    .map_err(|e| format!("its launch cannot be resumed ({e})"))
            })
            .and_then(|args| match &event.model {
                Some(model) => argv::with_model(&args, model)
                    .map_err(|e| format!("model={model} cannot be named ({e})")),
                None => Ok(args),
            });
        let args = match resume {
            Ok(args) => args,
            Err(why) => {
                self.journal
                    .note("RESTART-REFUSED", &format!("cause={cause} {why}"));
                return Asked::Refused;
            }
        };
        let word = self.link.relaunch_word(true, aterm::now_ms());
        aterm::journal(&mut self.link, &self.journal);
        match word {
            Relaunch::Permitted => {}
            Relaunch::Denied(why) => {
                self.journal
                    .note("RESTART-REFUSED", &format!("cause={cause} {why}"));
                return Asked::Refused;
            }
            Relaunch::Away(why) | Relaunch::Later(why) => {
                return Asked::Waits(format!("aterm did not answer: {why}"));
            }
        }
        if !signals::send(run.pid, libc::SIGTERM) {
            self.journal.note(
                "RESTART-REFUSED",
                &format!("cause={cause} child={} took no SIGTERM", run.pid),
            );
            return Asked::Refused;
        }
        let model = event
            .model
            .as_ref()
            .map_or_else(String::new, |m| format!(" model={m}"));
        self.journal.note(
            "RESTART",
            &format!(
                "cause={cause}{model}: child={} SIGTERM, resuming session {session}",
                run.pid
            ),
        );
        let kill_after = if cause == "stall" {
            STALL_KILL_AFTER.min(self.cfg.term_grace)
        } else {
            self.cfg.term_grace
        };
        run.stop = Some(Stop {
            from: run.build.clone(),
            to: run.build.clone(),
            session,
            sent: Instant::now(),
            restart: Some(Escalation {
                cause: cause.clone(),
                args,
                kill_after,
                killed: false,
            }),
        });
        Asked::Made
    }

    /// A child was started: a restart aterm asked for until now was meant for the child before
    /// it, so it is passed over and never applied to this one (one still waiting, and one aterm
    /// sent as that child ended and not yet drained); then the claim is announced again with
    /// the new child (`vendor=`, and `child=<pid>:<start>` under contract 4).
    fn announce(&mut self, run: &Run) {
        if let Some(waiting) = self.deferred.take() {
            self.journal.note(
                "RESTART-PASSED-OVER",
                &format!(
                    "cause={} it waited for the child before this one",
                    waiting.event.cause
                ),
            );
        }
        let now = aterm::now_ms();
        self.link.tick(run.claim(), now);
        let drained = self.link.events(run.claim(), now);
        aterm::journal(&mut self.link, &self.journal);
        if let Some(event) = drained.restart {
            self.journal.note(
                "RESTART-PASSED-OVER",
                &format!(
                    "cause={} it came before this child was announced",
                    event.cause
                ),
            );
        }
        self.link.reannounce(run.claim(), aterm::now_ms());
        aterm::journal(&mut self.link, &self.journal);
    }

    /// TIME STOPPED IS NOT GRACE: the child was continued, so the wait of a `SIGTERM` the harness
    /// sent starts again (a stopped child could not act on it).
    fn continued(&self, run: &mut Run) {
        if let Some(stop) = run.stop.as_mut() {
            stop.sent = Instant::now();
            self.journal.note(
                "CONTINUED",
                &format!(
                    "child={} was continued; the {:?} it has to end after SIGTERM starts again",
                    run.pid, self.cfg.stop_wait
                ),
            );
        }
    }

    /// One look: a vendor move when a newer vendor build is installed (and the child was never
    /// found to outlive a `SIGTERM`), else the harness's own update, which this returns when it
    /// may go ahead now.
    fn look(&mut self, run: &mut Run, last_word: &mut String) -> Option<OwnBuild> {
        if !run.term_ignored && self.vendor_look(run, last_word) {
            return None;
        }
        self.own_look(run)
    }

    /// The vendor's look: a newer build installed, the safe point reached and the new build
    /// proved to start, stops the child. Whether a vendor move holds this look: a newer build
    /// is installed and the move waits on anything but its own build's probe (a build that does
    /// not start leaves the look to the harness's own update).
    fn vendor_look(&mut self, run: &mut Run, last_word: &mut String) -> bool {
        let Ok(target) = self.vendor() else {
            return false;
        };
        if !moves_onto(run.build.version, target.version) {
            return false;
        }
        let failed_lately = self.proofs.get(&target.version).is_some_and(|p| {
            p.found != Probe::Starts && (p.blind || p.at.elapsed() < self.cfg.probe_gap)
        });
        let mut detail = None;
        // A claim that does not hand this harness the move, and a build whose probe failed
        // lately, hold before the safe point is read at all.
        let checked = if let Err((word, why)) = aterm::upgrade_held(&self.link, self.hosted) {
            detail = Some(why);
            Err(word)
        } else if failed_lately {
            Err("build-unproved")
        } else {
            self.safe_point(run)
        };
        let word = match checked {
            Err(word) => word,
            Ok((session, terminal, submitted_us)) => match self.prove(&target) {
                (false, _) => "build-unproved",
                // Proved just now: the probe took time, so the next look reads the whole
                // safe point again before any signal.
                (true, true) => "build-proved",
                (true, false) => {
                    // The last re-check, just before the signal: no input since the look,
                    // still the foreground, still no tool running.
                    let now = self.terminal();
                    let composer = ComposerWitness::InputClock {
                        last_input_us: terminal.last_input_us,
                        submitted_us,
                    };
                    if now != Some(terminal) || procs::descendants(run.pid) != Some(0) {
                        "changed-before-signal"
                    } else if let Err((word, why)) = self.move_unheld(composer) {
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
                            restart: None,
                        });
                        return true;
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
        word != "build-unproved"
    }

    /// The harness's own look ([`crate::adopt`]): a newer build of the harness in the store, the
    /// same safe point as a move's, and the new image proved to speak this one's adoption
    /// protocol; then the build to re-exec into. Each new word that holds it is journalled once.
    fn own_look(&mut self, run: &mut Run) -> Option<OwnBuild> {
        let running = self.own?;
        let target = store::own_current(&self.layout, HARNESS, HARNESS).ok()?;
        if target.id <= running {
            return None;
        }
        let failed_lately = self.own_proof.is_some_and(|(id, p)| {
            id == target.id
                && p.found != Probe::Starts
                && (p.blind || p.at.elapsed() < self.cfg.probe_gap)
        });
        let mut detail = None;
        let checked = if let Err((word, why)) = aterm::upgrade_held(&self.link, self.hosted) {
            detail = Some(why);
            Err(word)
        } else if failed_lately {
            Err("build-unproved")
        } else {
            self.safe_point(run)
        };
        let word = match checked {
            Err(word) => word,
            Ok((_, terminal, submitted_us)) => match self.prove_own(&target) {
                (false, _) => "build-unproved",
                (true, true) => "build-proved",
                (true, false) => {
                    let composer = ComposerWitness::InputClock {
                        last_input_us: terminal.last_input_us,
                        submitted_us,
                    };
                    if self.terminal() != Some(terminal) || procs::descendants(run.pid) != Some(0) {
                        "changed-before-exec"
                    } else if let Err((word, why)) = self.move_unheld(composer) {
                        detail = Some(why);
                        word
                    } else {
                        return Some(target);
                    }
                }
            },
        };
        if self.own_word != word {
            self.journal.note(
                "WAIT",
                &format!(
                    "{HARNESS} {HARNESS_VERSION} (build {running}) -> build {} waits: {word}{}",
                    target.id,
                    detail.map_or_else(String::new, |d| format!(" ({d})"))
                ),
            );
            self.own_word = word.to_string();
        }
        None
    }

    /// Whether the harness's own build `target` is proved to speak this image's adoption
    /// protocol ([`adopt::judge_probe`]): `(proved, probed_now)`, kept as [`Harness::prove`]
    /// keeps a vendor build's.
    fn prove_own(&mut self, target: &OwnBuild) -> (bool, bool) {
        if let Some((id, p)) = &self.own_proof
            && *id == target.id
        {
            if p.found == Probe::Starts {
                return (true, false);
            }
            if p.blind || p.at.elapsed() < self.cfg.probe_gap {
                return (false, false);
            }
        }
        let (found, said) =
            probe::said_within(&target.exe, adopt::PROBE_ARG, &[], self.cfg.probe_wait);
        let found = adopt::judge_probe(found, &said);
        self.journal.note(
            "PROBE",
            &format!(
                "{HARNESS} build {}: `{} {}` {}",
                target.id,
                target.exe.display(),
                adopt::PROBE_ARG,
                found.word()
            ),
        );
        self.own_proof = Some((
            target.id,
            Proof {
                found,
                at: Instant::now(),
                blind: false,
            },
        ));
        (found == Probe::Starts, true)
    }

    /// aterm's word at a move's act ([`aterm::move_unheld`]), its notes journalled.
    fn move_unheld(&mut self, composer: ComposerWitness) -> Result<(), (&'static str, String)> {
        let grace = self.grace();
        let word = aterm::move_unheld(
            &mut self.link,
            self.hosted,
            grace,
            composer,
            aterm::now_ms(),
        );
        aterm::journal(&mut self.link, &self.journal);
        word
    }

    /// What crosses the re-exec ([`Carried`]).
    fn carry(&self, run: &Run) -> Carried {
        Carried {
            from: HARNESS_VERSION.to_string(),
            prefix: self.layout.prefix.clone(),
            child: run.pid,
            build: run.build.clone(),
            args: run.args.clone(),
            started_ms: run.started_ms,
            lock: run.lock.clone(),
            moved: run
                .moved
                .as_ref()
                .map(|(prev, session, at)| (prev.clone(), session.clone(), at.elapsed())),
            term_ignored: run.term_ignored,
            ladder: (self.ladder.streak(), self.ladder.since(aterm::now_ms())),
            proofs: self
                .proofs
                .iter()
                .map(|(v, p)| (*v, p.found, p.at.elapsed(), p.blind))
                .collect(),
            mode: self.mode,
            linked: !self.link.is_absent(),
        }
    }

    /// RE-EXEC into the harness's own build `target` ([`crate::adopt`]), the caught signals
    /// blocked across it. Returns only when it did not happen: with what arrived meanwhile when
    /// a `SIGHUP`, `SIGTERM` or `SIGCONT` holds it (the loop acts on them first), else after the
    /// exec failed, which leaves that build unproved for good.
    fn reexec(
        &mut self,
        run: &Run,
        target: &OwnBuild,
        signals: &signals::Signals,
    ) -> signals::Pending {
        let running = self.own.map_or_else(|| "-".to_string(), |n| n.to_string());
        if let Err(e) = signals::block_caught() {
            self.journal
                .note("NO-REEXEC", &format!("cannot block its signals: {e}"));
            return signals::Pending::default();
        }
        let arrived = signals.wait(Duration::ZERO);
        if arrived.acts() {
            signals::unblock_caught();
            self.journal.note(
                "WAIT",
                &format!(
                    "{HARNESS} {HARNESS_VERSION} (build {running}) -> build {} waits: a signal arrived",
                    target.id
                ),
            );
            return arrived;
        }
        let carried = self.carry(run);
        self.journal.note(
            "REEXEC",
            &format!(
                "{HARNESS} {HARNESS_VERSION} (build {running}) -> build {} ({}): child={} and its session {} cross on a descriptor",
                target.id,
                target.exe.display(),
                run.pid,
                run.lock.as_ref().map_or("-", |(s, _)| s.as_str())
            ),
        );
        let why = adopt::exec_into(&target.exe, &carried);
        signals::unblock_caught();
        self.journal.note("NO-REEXEC", &why);
        self.own_proof = Some((
            target.id,
            Proof {
                found: Probe::NoExec,
                at: Instant::now(),
                blind: true,
            },
        ));
        arrived
    }

    fn terminal(&self) -> Option<Terminal> {
        self.tty.map(|t| Terminal {
            last_input_us: t.last_input_us(),
            foreground: t.foreground(),
            pending_input: t.pending_input(),
        })
    }

    /// The safe point, read cheapest first ([`gate::gate`]'s order): the session, the terminal
    /// reading it was decided on and the last submission, or the hold. The composer is judged at
    /// the act, on aterm's fresh word where aterm serves one, else on the input clock against
    /// that submission ([`aterm::move_unheld`]).
    fn safe_point(&self, run: &mut Run) -> Result<(String, Terminal, u64), &'static str> {
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
        let submitted_us = gate::check_log(&log, now, &self.cfg.timing)?;
        Ok((session, terminal, submitted_us))
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

    /// The child ended: relaunch it, or end the harness as it ended. A crash relaunch waits for
    /// the ladder's pause and aterm's word ([`Harness::crash_wait`]), watching `signals`
    /// meanwhile; `interrupted` says a `SIGINT` or `SIGQUIT` reached the harness in the pass
    /// that found the end.
    fn after_end(
        &mut self,
        mut run: Run,
        end: End,
        ending: Option<libc::c_int>,
        interrupted: bool,
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
            let moved = match stop.restart {
                // aterm's restart: the same session, on the build the child ran unless a newer
                // one is proved and the claim takes `upgrade`.
                Some(restart) => self.relaunch_build(&stop.to, &run.build).and_then(|build| {
                    let msg = format!(
                        "{PRODUCT} ended for aterm's restart (cause={}, {}); resuming session {session} on {}",
                        restart.cause,
                        describe(end),
                        build.version
                    );
                    self.relaunch_start(build, &run.build, restart.args, &msg)
                }),
                None => match self.relaunch_plan(&run, &session, &stop.to, &run.build) {
                    Ok((build, args)) => {
                        let msg = format!(
                            "moving {PRODUCT} {from} -> {}, resuming session {session}",
                            build.version
                        );
                        self.relaunch_start(build, &run.build, args, &msg)
                    }
                    Err(why) => Err(why),
                },
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
        // A crash is an end both rules call abnormal: Muse Code's exit table (not clean, not a
        // startup refusal) and aterm's (`abnormal_exit`: no job or terminal signal's).
        if kind != ExitKind::Crashed || !abnormal_exit(child_end(end)) {
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
        let known = run.build.clone();
        let resume = grammar_argv(&known, &run.args)
            .ok_or_else(|| "its launch has an argument that is not UTF-8".to_string())
            .and_then(|launch| {
                argv::rewrite_argv(&launch, Some(&session))
                    .map_err(|e| format!("its launch cannot be resumed ({e})"))
            });
        if let Err(why) = resume {
            self.journal.note("NO-RELAUNCH", &why);
            self.finish(end);
        }
        // In an aterm tab where this harness never held a claim (one aterm has not admitted yet,
        // busy, off for now or not serving its contract, or refused for good), the crash is not
        // this harness's to answer: aterm never let it lead the tab, and its answer stands now,
        // not after a wait for a claim nobody lost.
        if self.hosted && !self.link.claims() {
            let why = if self.link.is_absent() {
                "aterm refused this harness's claim for the session"
            } else {
                "aterm has not admitted this harness's claim"
            };
            self.journal
                .note("NO-RELAUNCH", &format!("no-claim: {why}"));
            self.finish(end);
        }
        // The crashed child left the terminal in its own mode: put the starting one back at once,
        // so a key reaches the harness as a signal for as long as no child runs.
        self.restore_mode();
        if let Some(waiting) = self.deferred.take() {
            self.journal.note(
                "RESTART-REFUSED",
                &format!("cause={} child-ended", waiting.event.cause),
            );
        }
        let pause = self.ladder.crashed(aterm::now_ms());
        self.journal.note(
            "CRASH",
            &format!(
                "child={} {}: session {session} streak={} pause-ms={}",
                run.pid,
                describe(end),
                self.ladder.streak(),
                pause.as_millis()
            ),
        );
        if let Err(why) = self.crash_wait(signals, pause, &run.vendor, interrupted) {
            say(&format!(
                "{HARNESS}: {PRODUCT} ended abnormally ({}); not relaunching it: {why}",
                describe(end)
            ));
            self.journal.note("NO-RELAUNCH", &why);
            self.discard_input();
            self.finish(end);
        }
        let ran = run.build.version;
        let resumed = match self.relaunch_plan(&run, &session, &known, &run.build) {
            Ok((build, args)) => {
                let on = build.version;
                let onto = if on == ran {
                    String::new()
                } else {
                    format!(" on {on}")
                };
                let msg = format!(
                    "{PRODUCT} ended abnormally ({}); resuming session {session}{onto} (crash relaunch {} after a {} ms pause)",
                    describe(end),
                    self.ladder.streak().saturating_add(1),
                    pause.as_millis()
                );
                self.discard_input();
                self.relaunch_start(build, &run.build, args, &msg)
            }
            Err(why) => Err(why),
        };
        match resumed {
            Ok(mut next) => {
                self.ladder.relaunched(aterm::now_ms());
                if next.build.version != ran {
                    next.moved = Some((known, session, Instant::now()));
                }
                next
            }
            Err(why) => {
                say(&format!("{HARNESS}: {PRODUCT} cannot be resumed: {why}"));
                self.journal.note("NO-RELAUNCH", &why);
                self.discard_input();
                self.finish(end);
            }
        }
    }

    /// KEYS TYPED AT A DEAD SCREEN ARE MEANT FOR NOBODY: drop what was typed and not read while a
    /// crash relaunch waited, before the relaunched child starts or the harness ends. The
    /// terminal is back in its starting (cooked, echoing) mode for that wait, so a line typed
    /// there would otherwise reach the resumed Muse Code as a prompt, or the parent shell as a
    /// command once the crash is passed on. Only while this job owns the terminal's foreground
    /// ([`Tty::discard_input`]).
    fn discard_input(&self) {
        if let Some(tty) = self.tty {
            let _ = tty.discard_input();
        }
    }

    /// THE CRASH WAIT: the ladder's `pause`, and under aterm aterm's word on the relaunch
    /// (`Link::relaunch_word` for a crash), asked at once and again once the pause is over, so
    /// the relaunch stands on a fresh read. aterm away (a lost link, made again every 5 s) or
    /// answering only later is asked again every second, within [`Config::away_budget`] of time
    /// actually spent waiting for it: in a tab this harness leads, no built-in lane of aterm's
    /// brings the child back, so a crash passed on at once would leave the session to nobody.
    /// A lost link whose claim aterm last answered serving no contract this harness declares
    /// (`Failure::Unserved` in `Link::unclaimed`, after an aterm restart into `2,3`) is not
    /// away: that answer is aterm's last word on the link, and it passes the crash on at once.
    /// Outside aterm only the pause is waited. Restarts aterm asks for meanwhile are refused (no
    /// child runs to restart), and a `SIGINT`, `SIGQUIT`, `SIGHUP` or `SIGTERM` (or
    /// `interrupted`) passes the crash on: whoever sends one wants the session over.
    ///
    /// # Errors
    /// Why the crash is passed on.
    fn crash_wait(
        &mut self,
        signals: &signals::Signals,
        pause: Duration,
        vendor: &str,
        interrupted: bool,
    ) -> Result<(), String> {
        let passed_on = |p: signals::Pending| {
            let which = if p.hup {
                "SIGHUP"
            } else if p.term {
                "SIGTERM"
            } else {
                "SIGINT or SIGQUIT"
            };
            format!("{which} reached the harness while the crash relaunch waited")
        };
        if interrupted {
            return Err(passed_on(signals::Pending {
                int: true,
                ..signals::Pending::default()
            }));
        }
        let due = Instant::now() + pause;
        // Outside aterm no claim was ever made and nothing is asked. In an aterm tab only a link
        // that held a claim gets this far ([`Harness::after_end`]).
        let asks = self.link.claims();
        let mut ask_at = asks.then(Instant::now);
        let mut budget = AwayBudget::within(self.cfg.away_budget);
        let mut away = String::new();
        loop {
            let now = Instant::now();
            if let Some(at) = ask_at
                && now >= at
            {
                let ms = aterm::now_ms();
                let word = self.link.relaunch_word(false, ms);
                aterm::journal(&mut self.link, &self.journal);
                match word {
                    Relaunch::Permitted => {
                        budget.answered(ms);
                        if now >= due {
                            return Ok(());
                        }
                        ask_at = Some(due);
                    }
                    Relaunch::Denied(why) => return Err(format!("aterm's word: {why}")),
                    Relaunch::Away(why) | Relaunch::Later(why) => {
                        // `relaunch_word` answers every lost link `Away`, but one whose claim
                        // made again aterm answered serving no contract this harness declares
                        // has aterm's last word, not its absence: no hold aterm stated stands
                        // over it, and no claim can be made until an aterm update, which comes
                        // as another restart. In the first 5 s after a loss nothing has been
                        // asked yet, and the wait's own tick makes the attempt.
                        if let Some(Failure::Unserved(word)) = self.link.unclaimed() {
                            return Err(format!("aterm's word: {word}"));
                        }
                        if budget.wait(ms) {
                            self.journal.note(
                                "CRASH-WAIT",
                                &format!(
                                    "aterm did not answer ({why}); asking again within the {} s budget",
                                    self.cfg.away_budget.as_secs_f32()
                                ),
                            );
                        }
                        away = why;
                        ask_at = Some(now + ASK_AGAIN);
                    }
                }
            }
            if budget.spent(aterm::now_ms()) {
                return Err(format!(
                    "claim-not-proven-again {away}: aterm did not answer within {} ms",
                    self.cfg.away_budget.as_millis()
                ));
            }
            if !asks && now >= due {
                return Ok(());
            }
            let p = signals.wait(crash_sleep(now, due, ask_at, self.cfg.tick));
            if p.hup || p.term || p.int {
                return Err(passed_on(p));
            }
            let child = aterm::child(Some(vendor), None);
            let ms = aterm::now_ms();
            self.link.tick(child, ms);
            let drained = self.link.events(child, ms);
            aterm::journal(&mut self.link, &self.journal);
            if let Some(event) = drained.restart {
                self.journal.note(
                    "RESTART-REFUSED",
                    &format!("cause={} crash-pending: no child runs", event.cause),
                );
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
        // The claim names the child the session now runs ([`Harness::announce`]).
        if let Ok(next) = &started {
            self.announce(next);
        }
        started
    }
}

/// How long one pass of the crash wait ([`Harness::crash_wait`]) waits for a signal at `now`: at
/// most a `tick`, and no later than aterm's next ask (`ask_at`; none outside aterm) or, while the
/// ladder's pause runs, its end (`due`). Once the pause is over only the next ask is due: a wait
/// still capped at a pause long over would be no wait at all, and the harness would spin on a
/// zero-timeout wait for as long as aterm stayed away (up to its 45 s).
fn crash_sleep(now: Instant, due: Instant, ask_at: Option<Instant>, tick: Duration) -> Duration {
    let next = match ask_at {
        Some(at) if now < due => at.min(due),
        Some(at) => at,
        None => due,
    };
    tick.min(next.saturating_duration_since(now))
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

    /// THE CRASH WAIT NEVER SPINS: while the ladder's pause runs it wakes for the earlier of the
    /// pause's end and aterm's next ask; once the pause is over, with aterm away and asked again
    /// a second on, it sleeps until that ask (at most a tick). NEGATIVE CONTROL: the wait capped
    /// at a pause already over was zero there, a busy loop until aterm came back.
    #[test]
    fn the_crash_wait_sleeps_until_the_next_ask_once_the_pause_is_over() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let tick = Duration::from_millis(500);
        let sleep = |now, due, ask: Option<u64>| {
            crash_sleep(at(now), at(due), ask.map(at), tick).as_millis()
        };
        // During the pause.
        assert_eq!(sleep(0, 1000, Some(300)), 300, "until the next ask");
        assert_eq!(sleep(0, 200, Some(1000)), 200, "until the pause's end");
        assert_eq!(sleep(0, 300, None), 300, "outside aterm: the pause alone");
        assert_eq!(sleep(0, 10_000, None), 500, "never past a tick");
        // The pause over, aterm away: the next ask is a second on.
        assert_eq!(sleep(2000, 1000, Some(3000)), 500);
        assert_eq!(sleep(2000, 1000, Some(2200)), 200);
        assert_eq!(sleep(2000, 1000, Some(2000)), 0, "an ask due now");
        assert_eq!(sleep(2000, 1000, Some(1900)), 0, "an ask overdue");
    }

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
        assert_eq!(
            c.term_grace,
            Duration::from_secs(2),
            "a restart's grace is the stop wait"
        );
        assert_eq!(c.away_budget, AWAY_BUDGET, "aterm's 45 s unless shortened");
        let away = Config::parse_test_timing("50,100,300,200,2000,1500").expect("six numbers");
        assert_eq!(away.away_budget, Duration::from_millis(1500));
        assert_eq!(Config::PRODUCTION.away_budget, Duration::from_secs(45));
        assert_eq!(Config::PRODUCTION.term_grace, Duration::from_secs(30));
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
