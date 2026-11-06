// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The harness end to end: the built `muse-alab` binary, run on a pseudo-terminal in its
//! own session (as a terminal runs a shell's job), over a sandboxed atpkg store whose
//! `muse` is a FAKE Muse Code.
//!
//! The fake is this very test binary: installed into the store as `<build>/bin/muse`, it
//! sees its own name and `FAKE_MUSE_CTL` and plays Muse Code instead of running the tests
//! (so this target has its own `main`, `harness = false`). The fake keeps a session the way
//! Muse Code does — a `.session.lock` naming its pid, a `session.jsonl` with a
//! `runtime.session.route_facts` record and run records — reads the terminal in raw mode
//! only when `poll` says it is readable (as crossterm does), and takes commands from a
//! control file. It reports what it saw (argv, pid, parent, input, signals) in an events
//! file the tests read.
//!
//! The harness's safe-point clocks are shortened with `MUSE_ALAB_TEST_TIMING`, which only
//! a debug build reads; under a release build, or where the platform keeps no terminal
//! input clock (`muse_alab::tty::INPUT_CLOCK`), the tests that need them are skipped and
//! say so.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::FromRawFd as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The harness under test.
const HARNESS: &str = env!("CARGO_BIN_EXE_muse-alab");
/// The harness's version.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The build the session starts on.
const OLD: &str = "1.4.10-R6000.0";
/// The build atpkg installs underneath it.
const NEW: &str = "1.4.13-R6996.1";
/// A build newer still.
const NEWER: &str = "1.4.14-R7000.0";
/// The session the fake creates when it is not resuming one.
const SESSION: &str = "01a148f8-9301-7752-96db-fef456f88074";
/// tick, look, person grace, settle, stop wait (ms).
const TIMING: &str = "50,100,300,200,3000";
/// How long a test waits for something that must happen.
const PATIENCE: Duration = Duration::from_secs(30);

fn main() {
    if fake::is_me() {
        fake::main();
    }
    runner::main();
}

// ---------------------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------------------

/// Every test: its name, whether it needs the debug-build clocks, and its body.
const TESTS: &[(&str, bool, fn())] = &[
    (
        "passes_arguments_and_the_terminal_through",
        false,
        passes_arguments_and_the_terminal_through,
    ),
    (
        "exits_with_the_childs_code",
        false,
        exits_with_the_childs_code,
    ),
    (
        "a_startup_refusal_is_not_relaunched",
        false,
        a_startup_refusal_is_not_relaunched,
    ),
    (
        "dies_by_the_childs_signal_when_it_cannot_resume",
        false,
        dies_by_the_childs_signal_when_it_cannot_resume,
    ),
    (
        "ignores_interrupts_meant_for_the_child",
        false,
        ignores_interrupts_meant_for_the_child,
    ),
    (
        "passes_terminate_on_and_ends_with_it",
        false,
        passes_terminate_on_and_ends_with_it,
    ),
    (
        "passes_hangup_on_and_ends_with_it",
        false,
        passes_hangup_on_and_ends_with_it,
    ),
    (
        "follows_the_child_when_it_stops",
        false,
        follows_the_child_when_it_stops,
    ),
    (
        "moves_onto_a_newer_build_at_a_safe_point",
        true,
        moves_onto_a_newer_build_at_a_safe_point,
    ),
    (
        "never_stops_a_child_with_a_turn_open",
        true,
        never_stops_a_child_with_a_turn_open,
    ),
    (
        "never_stops_a_child_running_a_tool",
        true,
        never_stops_a_child_running_a_tool,
    ),
    (
        "never_stops_a_child_with_a_draft",
        true,
        never_stops_a_child_with_a_draft,
    ),
    (
        "never_stops_a_launch_it_cannot_resume",
        true,
        never_stops_a_launch_it_cannot_resume,
    ),
    (
        "never_signals_again_a_child_that_outlived_its_stop",
        true,
        never_signals_again_a_child_that_outlived_its_stop,
    ),
    (
        "never_moves_onto_a_build_that_does_not_start",
        true,
        never_moves_onto_a_build_that_does_not_start,
    ),
    (
        "never_stops_a_child_with_typing_unread",
        true,
        never_stops_a_child_with_typing_unread,
    ),
    (
        "relaunches_a_crashed_child_within_its_budget",
        false,
        relaunches_a_crashed_child_within_its_budget,
    ),
    (
        "a_crash_comes_back_on_the_build_it_ran_when_the_newest_does_not_start",
        false,
        a_crash_comes_back_on_the_build_it_ran_when_the_newest_does_not_start,
    ),
    (
        "keeps_the_signals_its_caller_ignored",
        false,
        keeps_the_signals_its_caller_ignored,
    ),
    (
        "a_broken_stderr_never_loses_the_session",
        false,
        a_broken_stderr_never_loses_the_session,
    ),
    (
        "never_recurses_through_a_store_wrapper",
        false,
        never_recurses_through_a_store_wrapper,
    ),
    (
        "refuses_when_muse_is_not_installed",
        false,
        refuses_when_muse_is_not_installed,
    ),
    (
        "refuses_an_incomplete_build",
        false,
        refuses_an_incomplete_build,
    ),
    ("refuses_to_run_itself", false, refuses_to_run_itself),
    (
        "prints_its_version_and_the_installed_builds",
        false,
        prints_its_version_and_the_installed_builds,
    ),
    (
        "no_harness_runs_muse_in_its_place",
        false,
        no_harness_runs_muse_in_its_place,
    ),
    (
        "falls_back_when_a_moved_build_passes_help_but_refuses_to_start",
        true,
        falls_back_when_a_moved_build_passes_help_but_refuses_to_start,
    ),
    (
        "under_aterm_passes_through_where_no_claim_is_proven",
        false,
        under_aterm_passes_through_where_no_claim_is_proven,
    ),
    (
        "under_aterm_holds_the_session_by_a_proven_claim",
        false,
        under_aterm_holds_the_session_by_a_proven_claim,
    ),
    (
        "under_aterm_a_crash_while_the_claim_lapsed_waits_for_it_and_is_relaunched",
        false,
        under_aterm_a_crash_while_the_claim_lapsed_waits_for_it_and_is_relaunched,
    ),
    (
        "under_aterm_a_crash_is_passed_back_once_the_claim_stays_away_past_the_hold",
        false,
        under_aterm_a_crash_is_passed_back_once_the_claim_stays_away_past_the_hold,
    ),
];

fn passes_arguments_and_the_terminal_through() {
    let sb = Sandbox::new("passthrough");
    let old = sb.install(OLD);
    sb.point_current(&old);
    let args: Vec<OsString> = vec![
        "--trust-workspace".into(),
        "a b".into(),
        "".into(),
        "--".into(),
        "-x".into(),
        OsString::from_vec(b"\xffz".to_vec()),
    ];
    let mut h = sb.spawn(&args, &[]);
    let start = h.wait_start(&sb, 1);
    assert_eq!(
        field(&start, "argv"),
        r"[--trust-workspace][a b][][--][-x][\xffz]"
    );
    assert_eq!(
        field(&start, "tty"),
        "1",
        "the child is on the inherited terminal"
    );
    assert_eq!(
        field(&start, "ppid"),
        h.pid.to_string(),
        "the harness is its parent"
    );
    assert_eq!(
        field(&start, "exe"),
        old.join("bin").join("muse").display().to_string(),
        "the store's build, not PATH's muse"
    );
    h.type_bytes(b"hi");
    sb.wait_event(&h, "input", |e| e == "input hi");
    sb.send("say hello-from-the-fake");
    h.wait_output("hello-from-the-fake");
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
    assert!(!sb.decoy_ran(), "PATH's muse never runs");
    let j = sb.journal();
    assert!(
        j.contains(" START ") && j.contains(" EXIT ") && j.contains(" END exit 0"),
        "{j}"
    );
}

fn exits_with_the_childs_code() {
    // Exit 7 is not in Muse Code's clean exit table, but the session's log says it ended
    // clean: the harness ends with 7 and resumes nothing.
    let sb = Sandbox::new("exit-code");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.send("exit 7");
    assert_eq!(h.wait_exit(), Exit::Code(7));
    assert_eq!(sb.starts().len(), 1);
    assert!(
        sb.journal().contains("the child's session log ends clean"),
        "{}",
        sb.journal()
    );
    // And with no session at all, an abnormal code is passed on as it is.
    let sb = Sandbox::new("exit-code-no-session");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(
        &["--trust-workspace".into()],
        &[("FAKE_MUSE_NO_SESSION", "1")],
    );
    h.wait_start(&sb, 1);
    sb.send("crash-exit 42");
    assert_eq!(h.wait_exit(), Exit::Code(42));
    assert_eq!(sb.starts().len(), 1);
}

fn a_startup_refusal_is_not_relaunched() {
    // Exit 1 and 2 are Muse Code's startup refusals (aterm's exit table): the session
    // holds a witnessed session, and still nothing is relaunched.
    let sb = Sandbox::new("refusal");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.send("exit 2");
    assert_eq!(h.wait_exit(), Exit::Code(2));
    assert_eq!(sb.starts().len(), 1);
}

fn dies_by_the_childs_signal_when_it_cannot_resume() {
    // A child that crashes holding no session cannot be resumed: the harness ends as it
    // did, by the same signal.
    let sb = Sandbox::new("signal-death");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(
        &["--trust-workspace".into()],
        &[("FAKE_MUSE_NO_SESSION", "1")],
    );
    h.wait_start(&sb, 1);
    sb.send(&format!("crash {}", libc::SIGUSR1));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGUSR1));
    assert_eq!(sb.starts().len(), 1);
    assert!(
        sb.journal()
            .contains("NO-RELAUNCH the crashed child held no witnessed session")
    );
}

fn ignores_interrupts_meant_for_the_child() {
    let sb = Sandbox::new("interrupt");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    // To the harness alone: ignored (the terminal sends ^C to the whole job; the
    // child decides what it means).
    h.kill(libc::SIGINT);
    h.kill(libc::SIGQUIT);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        h.running(),
        "the harness outlives SIGINT and SIGQUIT sent to it"
    );
    sb.send("say still-here");
    h.wait_output("still-here");
    // To the job, as the terminal sends it: the child dies of it, and so does the
    // harness, without a relaunch (130 is a clean end in Muse Code's exit table).
    h.killpg(libc::SIGINT);
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGINT));
    assert_eq!(sb.starts().len(), 1);
}

fn passes_terminate_on_and_ends_with_it() {
    let sb = Sandbox::new("terminate");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    h.kill(libc::SIGTERM);
    sb.wait_event(&h, "term", |_| true);
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGTERM));
    assert_eq!(sb.starts().len(), 1, "an ending session is not relaunched");
}

fn passes_hangup_on_and_ends_with_it() {
    let sb = Sandbox::new("hangup");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    h.kill(libc::SIGHUP);
    sb.wait_event(&h, "hup", |_| true);
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGHUP));
    assert_eq!(sb.starts().len(), 1);
}

fn follows_the_child_when_it_stops() {
    let sb = Sandbox::new("stop");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.send("stop");
    assert_eq!(
        h.wait_stopped(),
        libc::SIGSTOP,
        "the harness stops with its child"
    );
    h.killpg(libc::SIGCONT);
    sb.wait_event(&h, "continued", |_| true);
    sb.send("exit 3");
    assert_eq!(h.wait_exit(), Exit::Code(3));
}

fn moves_onto_a_newer_build_at_a_safe_point() {
    let sb = Sandbox::new("update");
    let old = sb.install(OLD);
    sb.point_current(&old);
    let launch: Vec<OsString> = ["--trust-workspace", "--model", "m1", "hello prompt"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let mut h = sb.spawn(&launch, &[]);
    let first = h.wait_start(&sb, 1);
    assert_eq!(
        field(&first, "argv"),
        "[--trust-workspace][--model][m1][hello prompt]"
    );
    // A turn, then quiet: the safe point. The prompt arrives in one read, in a later
    // wall-clock second than the terminal's creation, so the kernel times it exactly
    // (src/tty.rs).
    next_second();
    h.type_bytes(b"go\r");
    sb.wait_event(&h, "submit", |e| e == "submit go");
    let new = sb.install(NEW);
    sb.point_current(&new);
    let second = h.wait_start(&sb, 2);
    let events = sb.events();
    let term = events.iter().position(|e| e == "term");
    assert!(term.is_some(), "stopped with SIGTERM: {events:?}");
    let probe = events.iter().position(|e| {
        e.starts_with("help ")
            && field(e, "exe") == new.join("bin").join("muse").display().to_string()
    });
    assert!(
        probe.is_some_and(|p| Some(p) < term),
        "the new build was proved to start before the stop: {events:?}"
    );
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]"),
        "Muse Code's resume argv: kept flags, dropped --model and the first prompt"
    );
    assert_eq!(
        field(&second, "exe"),
        new.join("bin").join("muse").display().to_string()
    );
    assert_eq!(
        field(&second, "ppid"),
        h.pid.to_string(),
        "the same harness process"
    );
    h.wait_output(&format!(
        "muse-alab: moving Muse Code {OLD} -> {NEW}, resuming session {SESSION}"
    ));
    let j = sb.journal();
    assert!(j.contains(" STOP ") && j.contains(" RELAUNCH "), "{j}");
    // The resumed session is the same session, in a new process's part of its log.
    let log = fs::read_to_string(sb.session_dir(SESSION).join("session.jsonl")).expect("log");
    assert_eq!(
        log.matches(r#""payload_type":"runtime.session.route_facts""#)
            .count(),
        2,
        "{log}"
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
    assert_eq!(sb.starts().len(), 2);
}

/// The update hold every busy test shares: a newer build is installed while the child is
/// busy; the harness waits with `word` and never signals; once `release` is done the
/// child is moved (the positive control: the same harness would have moved it).
fn holds_while_busy(
    name: &str,
    launch: &[&str],
    busy: impl FnOnce(&Sandbox, &Harness),
    words: &[&str],
    release: impl FnOnce(&Sandbox, &Harness),
) {
    let sb = Sandbox::new(name);
    sb.point_current(&sb.install(OLD));
    let launch: Vec<OsString> = launch.iter().map(OsString::from).collect();
    let mut h = sb.spawn(&launch, &[]);
    h.wait_start(&sb, 1);
    busy(&sb, &h);
    sb.point_current(&sb.install(NEW));
    for word in words {
        h.wait_until(&sb, &format!("the hold {word}"), |sb| {
            sb.journal().contains(&format!("waits: {word}"))
        });
    }
    std::thread::sleep(Duration::from_millis(1500));
    let events = sb.events();
    assert!(
        !events.iter().any(|e| e.starts_with("term")),
        "a busy child is never signalled: {events:?}"
    );
    assert_eq!(sb.starts().len(), 1);
    assert!(h.running());
    release(&sb, &h);
    let second = h.wait_start(&sb, 2);
    assert!(
        field(&second, "argv").ends_with(&format!("[resume][{SESSION}]")),
        "{second}"
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn never_stops_a_child_with_a_turn_open() {
    holds_while_busy(
        "busy-turn",
        &["--trust-workspace"],
        |sb, h| {
            sb.send("open-turn");
            sb.wait_event(h, "open-turn", |_| true);
        },
        &["turn-open"],
        |sb, _| sb.send("end-turn"),
    );
}

fn never_stops_a_child_running_a_tool() {
    holds_while_busy(
        "busy-tool",
        &["--trust-workspace"],
        |sb, h| {
            sb.send("tool");
            sb.wait_event(h, "tool", |_| true);
        },
        &["child-process"],
        |sb, h| {
            sb.send("untool");
            sb.wait_event(h, "untool", |_| true);
        },
    );
}

fn never_stops_a_child_with_a_draft() {
    holds_while_busy(
        "busy-draft",
        &["--trust-workspace"],
        |sb, h| {
            // A person types a draft, a key at a time, and leaves it in the composer.
            for key in [b"d", b"r", b"a"] {
                h.type_bytes(key);
                std::thread::sleep(Duration::from_millis(120));
            }
            sb.wait_event(h, "input", |e| e == "input a");
        },
        &["draft-unproven"],
        |sb, h| {
            h.type_bytes(b"\r");
            sb.wait_event(h, "submit", |e| e == "submit dra");
        },
    );
}

fn never_stops_a_launch_it_cannot_resume() {
    // `--workspace` cannot be carried into a resume (aterm's grammar refuses it): the
    // harness never stops such a session, however quiet.
    let sb = Sandbox::new("not-resumable");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--workspace".into(), "/w".into()], &[]);
    h.wait_start(&sb, 1);
    sb.point_current(&sb.install(NEW));
    h.wait_until(&sb, "the hold not-resumable", |sb| {
        sb.journal().contains("waits: not-resumable")
    });
    std::thread::sleep(Duration::from_millis(1000));
    assert!(!sb.events().iter().any(|e| e.starts_with("term")));
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
    assert_eq!(sb.starts().len(), 1);
}

fn never_signals_again_a_child_that_outlived_its_stop() {
    let sb = Sandbox::new("unhonoured");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.send("ignore-term");
    sb.wait_event(&h, "ignore-term", |_| true);
    sb.point_current(&sb.install(NEW));
    sb.wait_event(&h, "term-ignored", |_| true);
    h.wait_until(&sb, "STOP-UNHONOURED", |sb| {
        sb.journal().contains("STOP-UNHONOURED")
    });
    std::thread::sleep(Duration::from_millis(1500));
    let events = sb.events();
    assert_eq!(
        events.iter().filter(|e| e.starts_with("term")).count(),
        1,
        "one SIGTERM, never another, never SIGKILL: {events:?}"
    );
    assert_eq!(sb.starts().len(), 1);
    assert!(h.running());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn never_moves_onto_a_build_that_does_not_start() {
    // atpkg installs a build that cannot start (`--help` exits 3): the session is quiet,
    // and still it is never stopped, because nothing proves the build would bring it back.
    let sb = Sandbox::new("unproved");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.point_current(&sb.install_script(NEW, "exit 3"));
    h.wait_until(&sb, "the hold build-unproved", |sb| {
        sb.journal().contains("waits: build-unproved")
    });
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        !sb.events().iter().any(|e| e.starts_with("term")),
        "a session is never stopped for a build that does not start: {:?}",
        sb.events()
    );
    assert!(sb.journal().contains("--help` exited"), "{}", sb.journal());
    assert_eq!(sb.starts().len(), 1);
    assert!(h.running());
    // The positive control: a newer build that does start is moved onto.
    let newer = sb.install(NEWER);
    sb.point_current(&newer);
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "exe"),
        newer.join("bin").join("muse").display().to_string()
    );
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn falls_back_when_a_moved_build_passes_help_but_refuses_to_start() {
    let sb = Sandbox::new("moved-refused-start");
    let old = sb.install(OLD);
    sb.point_current(&old);
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    let broken_new = sb.install_script(NEW, "if [ \"$1\" = --help ]; then exit 0; fi\nexit 1");
    sb.point_current(&broken_new);
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "exe"),
        old.join("bin").join("muse").display().to_string(),
        "fell back onto OLD after NEW refused to start"
    );
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    h.wait_until(&sb, "the hold build-unproved after fallback", |sb| {
        sb.journal().contains("waits: build-unproved")
    });
    assert!(h.running());
    // Installing NEWER re-arms the update and moves cleanly.
    let newer = sb.install(NEWER);
    sb.point_current(&newer);
    let third = h.wait_start(&sb, 3);
    assert_eq!(
        field(&third, "exe"),
        newer.join("bin").join("muse").display().to_string()
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn never_stops_a_child_with_typing_unread() {
    // The child is busy and not reading the terminal; a person types, and the keys wait in
    // the terminal's queue. The input clock has not moved (nothing read them), so only the
    // queue shows the person.
    holds_while_busy(
        "busy-unread",
        &["--trust-workspace"],
        |sb, h| {
            sb.send("deaf");
            sb.wait_event(h, "deaf", |_| true);
            h.type_bytes(b"go\r");
        },
        &["input-pending"],
        |sb, h| {
            // Read in a later wall-clock second than anything before it, so the kernel
            // times the read exactly (src/tty.rs).
            next_second();
            sb.send("hear");
            sb.wait_event(h, "submit", |e| e == "submit go");
        },
    );
}

fn a_crash_comes_back_on_the_build_it_ran_when_the_newest_does_not_start() {
    let sb = Sandbox::new("crash-unproved");
    let old = sb.install(OLD);
    sb.point_current(&old);
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    sb.point_current(&sb.install_script(NEW, "exit 3"));
    sb.send("crash-exit 101");
    let next = h.wait_start(&sb, 2);
    assert_eq!(
        field(&next, "exe"),
        old.join("bin").join("muse").display().to_string(),
        "the build it ran, not the newest that does not start"
    );
    assert_eq!(
        field(&next, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    let j = sb.journal();
    assert!(
        j.contains(" FALLBACK ") && j.contains("is not proved to start"),
        "{j}"
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn keeps_the_signals_its_caller_ignored() {
    // Started as `nohup` starts it (SIGHUP ignored) and as a shell without job control
    // starts a `&` job (SIGINT ignored): the child inherits both ignores, as it would
    // without the harness, and the harness does not end on SIGHUP.
    let sb = Sandbox::new("ignored");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn_with(
        &["--trust-workspace".into()],
        &[],
        &[libc::SIGHUP, libc::SIGINT],
        None,
    );
    let start = h.wait_start(&sb, 1);
    assert_eq!(field(&start, "ign"), "INT,HUP", "{start}");
    h.kill(libc::SIGHUP);
    std::thread::sleep(Duration::from_millis(400));
    assert!(h.running(), "an ignored SIGHUP does not end the session");
    assert!(!sb.events().iter().any(|e| e.starts_with("hup")));
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
    // The control: nothing ignored, nothing inherited ignored.
    let sb = Sandbox::new("ignored-control");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    let start = h.wait_start(&sb, 1);
    assert_eq!(field(&start, "ign"), "-", "{start}");
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn a_broken_stderr_never_loses_the_session() {
    // stderr is a pipe nobody reads any more: the line the harness writes before a
    // relaunch fails (EPIPE), and the relaunch still happens.
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: pipe(2) returned these and nothing else owns them.
    let (read, write) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    // SAFETY: fcntl on a descriptor this test owns.
    unsafe { libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC) };
    drop(read);
    let sb = Sandbox::new("broken-stderr");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn_with(&["--trust-workspace".into()], &[], &[], Some(write));
    h.wait_start(&sb, 1);
    sb.send("crash-exit 101");
    let next = h.wait_start(&sb, 2);
    assert_eq!(
        field(&next, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    assert!(sb.journal().contains(" RELAUNCH "), "{}", sb.journal());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

fn never_recurses_through_a_store_wrapper() {
    // A store `muse` that runs the harness again, by `exec` (the parent check) or as a
    // child of a shell (the depth bound), and the same through `--no-harness` (an `exec`
    // loop in one process): each refuses at once instead of recursing without bound.
    for (name, body, env, says) in [
        (
            "wrapper-exec",
            format!("exec '{HARNESS}' \"$@\""),
            vec![],
            "refusing to run: started as Muse Code by muse-alab",
        ),
        (
            "wrapper-child",
            format!("'{HARNESS}' \"$@\"\nexit $?"),
            vec![],
            "is already nested 8 deep",
        ),
        (
            "wrapper-no-harness",
            format!("exec '{HARNESS}' \"$@\""),
            vec![("ATERM_NO_HARNESS", "1")],
            "refusing to run: started as Muse Code by muse-alab",
        ),
    ] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install_script(OLD, &body));
        let mut h = sb.spawn(&["--trust-workspace".into()], &env);
        assert_eq!(h.wait_exit(), Exit::Code(127), "{name}: {}", h.output());
        h.wait_output(says);
        assert!(sb.starts().is_empty(), "{name}");
    }
}

fn relaunches_a_crashed_child_within_its_budget() {
    let sb = Sandbox::new("crash");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(
        &["--trust-workspace".into(), "--effort".into(), "high".into()],
        &[],
    );
    h.wait_start(&sb, 1);
    // A panic's exit status (101) without a clean end record is a crash too.
    sb.send("crash-exit 101");
    let next = h.wait_start(&sb, 2);
    assert_eq!(
        field(&next, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    h.wait_output(&format!(
        "Muse Code ended abnormally (exit 101); resuming session {SESSION} (relaunch 1 of 3"
    ));
    for n in 3..=4 {
        sb.send(&format!("crash {}", libc::SIGKILL));
        let next = h.wait_start(&sb, n);
        assert_eq!(
            field(&next, "argv"),
            format!("[--trust-workspace][resume][{SESSION}]")
        );
        assert_eq!(field(&next, "ppid"), h.pid.to_string());
    }
    h.wait_output(&format!(
        "Muse Code ended abnormally (SIGKILL); resuming session {SESSION} (relaunch 3 of 3"
    ));
    // The budget is spent: the fourth crash ends the harness as the child ended.
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 4);
    h.wait_output("not relaunching it again");
}

fn refuses_when_muse_is_not_installed() {
    let sb = Sandbox::new("missing");
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    assert_eq!(h.wait_exit(), Exit::Code(127));
    h.wait_output("muse-alab: Muse Code cannot be run from the atpkg store: it is not installed");
    h.wait_output("Install it with `aterm pkg install muse`.");
    h.wait_output("muse-alab: Muse Code was not started.");
    assert!(!sb.decoy_ran(), "PATH's muse is never the fallback");
    assert!(sb.starts().is_empty());
}

fn refuses_an_incomplete_build() {
    let sb = Sandbox::new("incomplete");
    let old = sb.install(OLD);
    fs::remove_file(old.with_extension("ready")).expect("unready");
    sb.point_current(&old);
    let mut h = sb.spawn(&[], &[]);
    assert_eq!(h.wait_exit(), Exit::Code(127));
    h.wait_output("its build is not complete");
    assert!(sb.starts().is_empty() && !sb.decoy_ran());
}

fn refuses_to_run_itself() {
    // A store whose `muse` is a copy of this harness would have it run itself forever.
    let sb = Sandbox::new("itself");
    let old = sb.install_exe(OLD, Path::new(HARNESS));
    sb.point_current(&old);
    let mut h = sb.spawn(&[], &[]);
    assert_eq!(h.wait_exit(), Exit::Code(127));
    h.wait_output("is this harness itself");
}

fn prints_its_version_and_the_installed_builds() {
    let sb = Sandbox::new("version");
    let run = |sb: &Sandbox| {
        let out = sb.command().arg("--version").output().expect("run");
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).expect("utf8")
    };
    assert_eq!(
        run(&sb),
        format!("muse-alab {VERSION} (muse not installed)\n")
    );
    let old = sb.install(OLD);
    sb.point_current(&old);
    assert_eq!(run(&sb), format!("muse-alab {VERSION} (muse {OLD})\n"));
    // A record that cannot be read, or does not agree with its build directory, gives no
    // version: never one decoded from the directory's id, never the vendor's own answer.
    let record = old.with_extension("vendor");
    let good = fs::read_to_string(&record).expect("record");
    let unknown = format!("muse-alab {VERSION} (muse unknown)\n");
    for (why, text) in [
        ("garbage", Some("not a record\n".to_string())),
        (
            "no version",
            Some("schema = 1\nprogram = \"muse\"\n".to_string()),
        ),
        ("another version", Some(format!("version = \"{NEW}\"\n"))),
        (
            "not canonical",
            Some("version = \"1.4.10-R6000.00\"\n".to_string()),
        ),
        ("missing", None),
    ] {
        match text {
            Some(text) => fs::write(&record, text).expect("record"),
            None => fs::remove_file(&record).expect("remove the record"),
        }
        assert_eq!(run(&sb), unknown, "{why}");
    }
    fs::write(&record, &good).expect("record");
    assert_eq!(run(&sb), format!("muse-alab {VERSION} (muse {OLD})\n"));
    // A build that is not complete is not the build a run would start.
    fs::write(old.with_extension("ready"), "pending\n").expect("ready");
    assert_eq!(run(&sb), unknown, "not ready");
    assert!(sb.starts().is_empty(), "--version never runs Muse Code");
    assert!(
        !sb.root.join("decoy-ran").exists(),
        "--version never runs a `muse` from PATH"
    );
}

fn no_harness_runs_muse_in_its_place() {
    for (name, args, env) in [
        (
            "no-harness-word",
            vec!["--no-harness", "--trust-workspace"],
            vec![],
        ),
        (
            "no-harness-env",
            vec!["--trust-workspace"],
            vec![("ATERM_NO_HARNESS", "1")],
        ),
    ] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
        let mut h = sb.spawn(&args, &env);
        let start = h.wait_start(&sb, 1);
        assert_eq!(
            field(&start, "pid"),
            h.pid.to_string(),
            "Muse Code replaced the harness"
        );
        assert_eq!(field(&start, "argv"), "[--trust-workspace]");
        sb.send("exit 5");
        assert_eq!(h.wait_exit(), Exit::Code(5));
    }
}

/// The session id the fake aterm serves.
const SID: &str = "s-1a";

/// A policy that serves contract 3 and is on, with both duties' switches on.
const POLICY_ON: &str =
    "OK gen=1 effective=on why=enabled contract=2,3 relaunch=true upgrade=true human_grace_s=120";

/// The tab's environment under aterm: the session and the rendezvous directory's home.
fn under_aterm_env(sb: &Sandbox) -> Vec<(&'static str, String)> {
    vec![
        ("TERM_PROGRAM", "aterm".to_string()),
        ("ATERM_PARENT_SESSION_ID", SID.to_string()),
        ("XDG_RUNTIME_DIR", sb.root.join("run").display().to_string()),
    ]
}

/// THE FAIL-CLOSED RULE (aterm's harness design §3.2, §8.2): in an aterm tab, wherever aterm does
/// not prove the harness's contract 3 claim, Muse Code runs in the harness's place (the same pid)
/// and the harness says why in its journal: no control socket answers, the policy lists no
/// contract 3, the policy is off, or the claim does not read back proven (which is let go).
fn under_aterm_passes_through_where_no_claim_is_proven() {
    for (name, policy, prove) in [
        ("aterm-no-socket", None, true),
        (
            "aterm-no-contract",
            Some("OK gen=1 effective=on why=enabled relaunch=true upgrade=true"),
            true,
        ),
        (
            "aterm-off",
            Some(
                "OK gen=1 effective=off why=autopilot-off contract=2,3 relaunch=true upgrade=true",
            ),
            true,
        ),
        ("aterm-unproven", Some(POLICY_ON), false),
    ] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm =
            policy.map(|p| fake_aterm::FakeAterm::start(&sb.root.join("run"), SID, p, prove));
        let env = under_aterm_env(&sb);
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut h = sb.spawn(&["--trust-workspace".into()], &env);
        let start = h.wait_start(&sb, 1);
        assert_eq!(
            field(&start, "pid"),
            h.pid.to_string(),
            "{name}: Muse Code replaced the harness"
        );
        assert_eq!(field(&start, "argv"), "[--trust-workspace]", "{name}");
        assert!(sb.journal().contains(" PASS "), "{name}: {}", sb.journal());
        if let Some(aterm) = &aterm {
            let asked = aterm.requests();
            assert!(
                asked.iter().any(|r| r == &format!("@{SID} harness policy")),
                "{name}: {asked:?}"
            );
            let claimed = asked.iter().any(|r| r.contains("meta set supervisor"));
            assert_eq!(claimed, name == "aterm-unproven", "{name}: {asked:?}");
            if claimed {
                assert!(
                    asked.contains(&format!(
                        "@{SID} meta unset supervisor holder=muse-alab@{}",
                        h.pid
                    )),
                    "{name}: an unproven claim is let go: {asked:?}"
                );
            }
        }
        sb.send("exit 3");
        assert_eq!(h.wait_exit(), Exit::Code(3), "{name}");
    }
}

/// UNDER A PROVEN CLAIM the harness supervises: it claims with its holder, contract, scope and
/// both versions; a crash is relaunched only after one fresh read of aterm (the policy and
/// `status`), and not at all once aterm's `[harness] relaunch` says off.
fn under_aterm_holds_the_session_by_a_proven_claim() {
    let sb = Sandbox::new("aterm-claimed");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm::FakeAterm::start(&sb.root.join("run"), SID, POLICY_ON, true);
    let env = under_aterm_env(&sb);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = sb.spawn(&["--trust-workspace".into()], &env);
    let first = h.wait_start(&sb, 1);
    assert_eq!(
        field(&first, "ppid"),
        h.pid.to_string(),
        "the harness's child"
    );
    let claim = format!(
        "@{SID} meta set supervisor muse-alab@{} contract=3 scope=relaunch,upgrade version={VERSION} vendor={OLD}",
        h.pid
    );
    let asked = aterm.requests();
    assert!(asked.contains(&claim), "{asked:?}");
    assert!(sb.journal().contains(" CLAIMED "), "{}", sb.journal());
    let before = asked.len();
    sb.send(&format!("crash {}", libc::SIGKILL));
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    let fresh: Vec<String> = aterm.requests().split_off(before);
    let policy_at = fresh
        .iter()
        .position(|r| r == &format!("@{SID} harness policy"));
    let status_at = fresh.iter().position(|r| r == &format!("@{SID} status"));
    assert!(
        policy_at.is_some() && status_at.is_some(),
        "the relaunch waited on a fresh read of aterm: {fresh:?}"
    );
    aterm.set_policy("OK gen=2 effective=on why=enabled contract=2,3 relaunch=false upgrade=true");
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(
        sb.starts().len(),
        2,
        "no relaunch once aterm says relaunch is off"
    );
    let j = sb.journal();
    assert!(
        j.contains("NO-RELAUNCH") && j.contains("relaunch is off"),
        "{j}"
    );
}

/// A CRASH WHILE THE CLAIM LAPSED is still relaunched: in a tab the harness leads no built-in
/// lane of aterm's can bring the child back, so a claim aterm no longer shows is made again and
/// the relaunch judged on a fresh read under it, at once when aterm proves it at once, else held
/// (`HOLD`) until aterm proves it again.
fn under_aterm_a_crash_while_the_claim_lapsed_waits_for_it_and_is_relaunched() {
    let sb = Sandbox::new("aterm-lapsed");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm::FakeAterm::start(&sb.root.join("run"), SID, POLICY_ON, true);
    let env = under_aterm_env(&sb);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = sb.spawn(&["--trust-workspace".into()], &env);
    let _first = h.wait_start(&sb, 1);
    let made_again = |sb: &Sandbox| sb.journal().matches("holds the session again").count();
    assert_eq!(made_again(&sb), 0, "{}", sb.journal());
    // aterm no longer shows the claim, and proves it again when asked.
    aterm.forget_claim();
    sb.send(&format!("crash {}", libc::SIGKILL));
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    assert_eq!(made_again(&sb), 1, "{}", sb.journal());
    assert!(
        !sb.journal().contains(" HOLD "),
        "proven again at once, the relaunch held for nothing: {}",
        sb.journal()
    );
    // aterm no longer shows it, and refuses to prove it until later.
    aterm.set_prove(false);
    aterm.forget_claim();
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the relaunch's hold", |sb| {
        sb.journal().contains(" HOLD ")
    });
    assert_eq!(
        sb.starts().len(),
        2,
        "no relaunch before aterm proves the claim"
    );
    aterm.set_prove(true);
    let third = h.wait_start(&sb, 3);
    assert_eq!(field(&third, "ppid"), h.pid.to_string());
    assert_eq!(made_again(&sb), 2, "{}", sb.journal());
    let j = sb.journal();
    assert!(!j.contains("NO-RELAUNCH"), "{j}");
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A CRASH IS PASSED BACK once the claim stays away past the hold: the harness waits the hold
/// (`HOLD`), then ends as the child ended, with nothing relaunched (`NO-RELAUNCH`).
fn under_aterm_a_crash_is_passed_back_once_the_claim_stays_away_past_the_hold() {
    let sb = Sandbox::new("aterm-lapsed-past");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm::FakeAterm::start(&sb.root.join("run"), SID, POLICY_ON, true);
    let mut env = under_aterm_env(&sb);
    env.push(("MUSE_ALAB_TEST_TIMING", format!("{TIMING},1500")));
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut h = sb.spawn(&["--trust-workspace".into()], &env);
    let _first = h.wait_start(&sb, 1);
    aterm.set_prove(false);
    aterm.forget_claim();
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 1, "nothing relaunched");
    let j = sb.journal();
    assert!(
        j.contains(" HOLD ") && j.contains("NO-RELAUNCH") && j.contains("again within 1 s"),
        "{j}"
    );
}

// ---------------------------------------------------------------------------------------
// The fake aterm: one session's control socket, its graph entry and its token.
// ---------------------------------------------------------------------------------------

mod fake_aterm {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "fake-token";

    /// What the fake keeps: the policy it answers, whether it proves a claim, the claim it
    /// holds, and every request it was asked (after `AUTH`).
    #[derive(Default)]
    struct State {
        policy: String,
        prove: bool,
        holder: Option<String>,
        requests: Vec<String>,
    }

    /// A fake aterm control socket for one session.
    pub struct FakeAterm {
        sock: PathBuf,
        state: Arc<Mutex<State>>,
    }

    impl FakeAterm {
        /// Serve session `sid` under `runtime` (`<runtime>/aterm/graph/<sid>` names the socket),
        /// answering `harness policy` with `policy` and proving a claim when `prove`.
        pub fn start(runtime: &Path, sid: &str, policy: &str, prove: bool) -> FakeAterm {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            // A socket path must fit `sun_path`: a short name in the temporary directory.
            let sock = std::env::temp_dir().join(format!(
                "ma-{}-{}.sock",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_file(&sock);
            std::fs::write(sock.with_extension("sock.token"), format!("{TOKEN}\n")).expect("token");
            let graph = runtime.join("aterm").join("graph");
            std::fs::create_dir_all(&graph).expect("graph dir");
            std::fs::write(graph.join(sid), format!("pid 1\nsock {}\n", sock.display()))
                .expect("graph entry");
            let listener = UnixListener::bind(&sock).expect("bind the fake aterm");
            let state = Arc::new(Mutex::new(State {
                policy: policy.to_string(),
                prove,
                ..State::default()
            }));
            let served = Arc::clone(&state);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let state = Arc::clone(&served);
                    std::thread::spawn(move || serve(stream, &state));
                }
            });
            FakeAterm { sock, state }
        }

        /// Answer `harness policy` with `policy` from now on.
        pub fn set_policy(&self, policy: &str) {
            self.lock().policy = policy.to_string();
        }

        /// Prove a claim from now on when `prove`, else leave it unproven.
        pub fn set_prove(&self, prove: bool) {
            self.lock().prove = prove;
        }

        /// Forget the stored claim, as an aterm that no longer shows it.
        pub fn forget_claim(&self) {
            self.lock().holder = None;
        }

        /// Every request the fake was asked, in order.
        pub fn requests(&self) -> Vec<String> {
            self.lock().requests.clone()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
    }

    impl Drop for FakeAterm {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.sock);
            let _ = std::fs::remove_file(self.sock.with_extension("sock.token"));
        }
    }

    fn serve(stream: UnixStream, state: &Mutex<State>) {
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else {
                return;
            };
            if let Some(token) = line.strip_prefix("AUTH ") {
                if token != TOKEN {
                    let _ = out.write_all(b"ERR auth\n");
                    return;
                }
                continue;
            }
            let reply = {
                let mut st = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                st.requests.push(line.clone());
                let verb = line.split_once(' ').map_or("", |(_, v)| v);
                if line == "version" {
                    "OK 0.0.0-fake".to_string()
                } else if verb == "harness policy" {
                    st.policy.clone()
                } else if let Some(rest) = verb.strip_prefix("meta set supervisor ") {
                    if st.prove {
                        st.holder = rest.split(' ').next().map(str::to_string);
                    }
                    "OK".to_string()
                } else if verb.starts_with("meta unset supervisor") {
                    st.holder = None;
                    "OK".to_string()
                } else if verb == "status" {
                    format!(
                        "OK schema=1 human_ms=- supervisor={} supervisor_proven={}",
                        st.holder.as_deref().unwrap_or("-"),
                        u8::from(st.holder.is_some())
                    )
                } else {
                    format!("ERR unknown {verb}")
                }
            };
            if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// The sandbox: a home, an atpkg store, Muse Code's data home, the harness's state home,
// and the fake's control directory.
// ---------------------------------------------------------------------------------------

/// Serializes pty creation and process spawning, so no test's child inherits another
/// test's terminal descriptors between their creation and their close-on-exec flag.
static SPAWN: Mutex<()> = Mutex::new(());

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let root = base.join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for dir in ["home", "data", "state", "config", "ctl", "decoy", "run"] {
            fs::create_dir_all(root.join(dir)).expect("sandbox dir");
        }
        let root = fs::canonicalize(&root).expect("canonical");
        fs::set_permissions(root.join("home"), fs::Permissions::from_mode(0o700)).expect("mode");
        fs::write(root.join("ctl").join("cmd"), b"").expect("cmd file");
        // A `muse` on PATH that must never run.
        let decoy = root.join("decoy").join("muse");
        fs::write(
            &decoy,
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 99\n",
                root.join("decoy-ran").display()
            ),
        )
        .expect("decoy");
        fs::set_permissions(&decoy, fs::Permissions::from_mode(0o755)).expect("decoy mode");
        Self { root }
    }

    fn prefix(&self) -> PathBuf {
        let home = self.root.join("home");
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/aterm/pkg")
        } else {
            home.join(".local/share/aterm/pkg")
        }
    }

    fn program_dir(&self) -> PathBuf {
        self.prefix().join("store").join("muse")
    }

    /// Install `version` with this binary as its `muse` (the fake).
    fn install(&self, version: &str) -> PathBuf {
        let me = std::env::current_exe().expect("test exe");
        self.install_exe(version, &me)
    }

    /// Install `version` with a shell script as its `muse`.
    fn install_script(&self, version: &str, body: &str) -> PathBuf {
        let script = self.root.join(format!("script-{version}"));
        fs::write(&script, format!("#!/bin/sh\n{body}\n")).expect("script");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("mode");
        self.install_exe(version, &script)
    }

    fn install_exe(&self, version: &str, exe: &Path) -> PathBuf {
        let id = muse_alab::version::Version::parse(version)
            .expect("a vendor version")
            .build_id();
        let dir = self.program_dir().join(id.to_string());
        fs::create_dir_all(dir.join("bin")).expect("build dir");
        let bin = dir.join("bin").join("muse");
        if fs::hard_link(exe, &bin).is_err() {
            fs::copy(exe, &bin).expect("install the fake");
        }
        fs::write(
            dir.with_extension("vendor"),
            format!("schema = 1\nprogram = \"muse\"\nversion = \"{version}\"\n"),
        )
        .expect("record");
        fs::write(dir.with_extension("ready"), "ok\n").expect("ready");
        dir
    }

    /// Point `current` at `dir` the way atpkg does: a new link renamed over the old.
    fn point_current(&self, dir: &Path) {
        let tmp = self.program_dir().join(".current.tmp");
        let _ = fs::remove_file(&tmp);
        symlink(dir, &tmp).expect("link");
        fs::rename(&tmp, self.program_dir().join("current")).expect("swap");
    }

    fn session_dir(&self, session: &str) -> PathBuf {
        self.root
            .join("data/muse/sessions/2026/10/24")
            .join(session)
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(HARNESS);
        cmd.env_clear()
            .env("HOME", self.root.join("home"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("decoy").display()),
            )
            .env("FAKE_MUSE_CTL", self.root.join("ctl"))
            .env("MUSE_ALAB_TEST_TIMING", TIMING);
        cmd
    }

    /// Run the harness with `args` on a new pseudo-terminal, as the leader of its own
    /// session and foreground job.
    fn spawn(&self, args: &[OsString], env: &[(&str, &str)]) -> Harness {
        self.spawn_with(args, env, &[], None)
    }

    /// [`Sandbox::spawn`], with `ignore` left ignored for it (as `nohup` leaves `SIGHUP`;
    /// SIGINT, SIGQUIT, SIGHUP and SIGTERM are otherwise reset to their defaults, whatever
    /// this test runner inherited) and `stderr` in place of the terminal when given.
    fn spawn_with(
        &self,
        args: &[OsString],
        env: &[(&str, &str)],
        ignore: &[libc::c_int],
        stderr: Option<File>,
    ) -> Harness {
        let guard = SPAWN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut m, mut s) = (-1, -1);
        let mut ws = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty writes two descriptors; the name and mode pointers may be null.
        let rc = unsafe {
            libc::openpty(
                &raw mut m,
                &raw mut s,
                std::ptr::null_mut(),
                std::ptr::null_mut::<libc::termios>(),
                &raw mut ws,
            )
        };
        assert_eq!(rc, 0, "openpty: {}", std::io::Error::last_os_error());
        for fd in [m, s] {
            // SAFETY: fcntl on a descriptor this test owns.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        // SAFETY: openpty returned these descriptors and nothing else owns them.
        let (master, slave) = unsafe { (File::from_raw_fd(m), File::from_raw_fd(s)) };
        let mut cmd = self.command();
        let err = match stderr {
            Some(f) => Stdio::from(f),
            None => Stdio::from(slave.try_clone().expect("dup")),
        };
        cmd.args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::from(slave.try_clone().expect("dup")))
            .stdout(Stdio::from(slave))
            .stderr(err);
        let mut dispositions = [(libc::SIGINT, false); 4];
        for (slot, sig) in
            dispositions
                .iter_mut()
                .zip([libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM])
        {
            *slot = (sig, ignore.contains(&sig));
        }
        // SAFETY: setsid, ioctl and signal are async-signal-safe; the closure touches no
        // memory shared with the parent (it owns a copy of `dispositions`).
        unsafe {
            cmd.pre_exec(move || {
                for (sig, ignored) in dispositions {
                    libc::signal(
                        sig,
                        if ignored {
                            libc::SIG_IGN
                        } else {
                            libc::SIG_DFL
                        },
                    );
                }
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn the harness");
        drop(cmd);
        drop(guard);
        let pid = libc::pid_t::try_from(child.id()).expect("pid");
        // The harness is reaped with waitpid below.
        drop(child);
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().expect("dup master");
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&buf[..n]);
            }
        });
        Harness {
            pid,
            master,
            out,
            reaped: false,
        }
    }

    fn send(&self, line: &str) {
        let mut f = OpenOptions::new()
            .append(true)
            .open(self.root.join("ctl").join("cmd"))
            .expect("cmd file");
        f.write_all(format!("{line}\n").as_bytes()).expect("send");
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("ctl").join("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn starts(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.starts_with("start "))
            .collect()
    }

    fn journal(&self) -> String {
        fs::read_to_string(self.root.join("state/muse-alab/journal")).unwrap_or_default()
    }

    fn decoy_ran(&self) -> bool {
        self.root.join("decoy-ran").exists()
    }

    /// Wait for an event whose first word is `word` and that `pred` accepts.
    fn wait_event(&self, h: &Harness, word: &str, pred: impl Fn(&str) -> bool) -> String {
        let mut found = None;
        h.wait_until(self, &format!("the event {word}"), |sb| {
            found = sb
                .events()
                .into_iter()
                .find(|e| e.split(' ').next() == Some(word) && pred(e));
            found.is_some()
        });
        found.unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// `TIOCSCTTY`: make the terminal on fd 0 the controlling terminal.
#[cfg(target_os = "macos")]
const TIOCSCTTY: libc::c_ulong = 0x2000_7461;
#[cfg(not(target_os = "macos"))]
const TIOCSCTTY: libc::c_ulong = 0x540E;

/// How the harness ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Exit {
    Code(i32),
    Signal(i32),
}

/// One harness process on its pseudo-terminal.
struct Harness {
    pid: libc::pid_t,
    master: File,
    out: Arc<Mutex<Vec<u8>>>,
    reaped: bool,
}

impl Harness {
    fn output(&self) -> String {
        String::from_utf8_lossy(
            &self
                .out
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_owned()
    }

    fn type_bytes(&self, bytes: &[u8]) {
        (&self.master).write_all(bytes).expect("type");
    }

    fn kill(&self, sig: libc::c_int) {
        // SAFETY: a signal to the harness this test started.
        unsafe { libc::kill(self.pid, sig) };
    }

    fn killpg(&self, sig: libc::c_int) {
        // SAFETY: a signal to the job (process group) this test started.
        unsafe { libc::killpg(self.pid, sig) };
    }

    fn poll(&mut self, flags: libc::c_int) -> Option<libc::c_int> {
        let mut st = 0;
        // SAFETY: waitpid on this test's own child, with a valid status pointer.
        let rc = unsafe { libc::waitpid(self.pid, &raw mut st, flags | libc::WNOHANG) };
        (rc == self.pid).then_some(st)
    }

    fn running(&mut self) -> bool {
        !self.reaped && self.poll(0).is_none()
    }

    fn wait_exit(&mut self) -> Exit {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if let Some(st) = self.poll(0) {
                self.reaped = true;
                return if libc::WIFEXITED(st) {
                    Exit::Code(libc::WEXITSTATUS(st))
                } else {
                    Exit::Signal(libc::WTERMSIG(st))
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the harness did not exit; output:\n{}", self.output());
    }

    fn wait_stopped(&mut self) -> libc::c_int {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if let Some(st) = self.poll(libc::WUNTRACED) {
                assert!(libc::WIFSTOPPED(st), "status {st:#x}");
                return libc::WSTOPSIG(st);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the harness did not stop");
    }

    fn wait_output(&self, text: &str) {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if self.output().contains(text) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("no {text:?} on the terminal; it shows:\n{}", self.output());
    }

    /// Wait until `cond` holds, failing with everything the sandbox knows.
    fn wait_until(&self, sb: &Sandbox, what: &str, mut cond: impl FnMut(&Sandbox) -> bool) {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            if cond(sb) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {what}\nevents:\n{}\njournal:\n{}\nterminal:\n{}",
            sb.events().join("\n"),
            sb.journal(),
            self.output()
        );
    }

    /// Wait for the `n`th start of the fake and return its event.
    fn wait_start(&self, sb: &Sandbox, n: usize) -> String {
        self.wait_until(sb, &format!("start #{n}"), |sb| sb.starts().len() >= n);
        sb.starts()[n - 1].clone()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Whatever happened, leave nothing running: the harness, its child and any tool
        // share the job's process group.
        // SAFETY: signals and waitpid on the job this test started.
        unsafe {
            libc::killpg(self.pid, libc::SIGKILL);
            if !self.reaped {
                let mut st = 0;
                libc::waitpid(self.pid, &raw mut st, 0);
            }
        }
    }
}

/// Sleep into the next wall-clock second (and a little past its start).
fn next_second() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let into = Duration::from_nanos(u64::from(now.subsec_nanos()));
    std::thread::sleep(Duration::from_secs(1) - into + Duration::from_millis(50));
}

/// The value of `key=` in an event (to the next ` key=` or the end for `argv`, the last).
fn field(event: &str, key: &str) -> String {
    let tag = format!(" {key}=");
    let Some(at) = event.find(&tag) else {
        return String::new();
    };
    let rest = &event[at + tag.len()..];
    if key == "argv" {
        return rest.to_string();
    }
    if key == "exe" {
        return rest.split(" argv=").next().unwrap_or_default().to_string();
    }
    rest.split(' ').next().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------------------
// The runner (this target has no libtest harness: it is also the fake).
// ---------------------------------------------------------------------------------------

mod runner {
    use super::*;

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut filters = Vec::new();
        let mut list = false;
        let mut ignored = false;
        let mut exact = false;
        let mut skip_value = false;
        for a in &args {
            if skip_value {
                skip_value = false;
                continue;
            }
            match a.as_str() {
                "--list" => list = true,
                "--ignored" => ignored = true,
                "--exact" => exact = true,
                "--format" | "--test-threads" | "--skip" | "--logfile" | "--color" | "-Z" => {
                    skip_value = true;
                }
                a if a.starts_with('-') => {}
                a => filters.push(a.to_string()),
            }
        }
        let chosen: Vec<&(&str, bool, fn())> = TESTS
            .iter()
            .filter(|(name, _, _)| {
                filters.is_empty()
                    || filters.iter().any(|f| {
                        if exact {
                            name == f
                        } else {
                            name.contains(f.as_str())
                        }
                    })
            })
            .collect();
        if list {
            for (name, _, _) in &chosen {
                println!("{name}: test");
            }
            return;
        }
        if ignored {
            println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n");
            return;
        }
        println!("\nrunning {} tests", chosen.len());
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<(&str, Outcome)>> = Mutex::new(Vec::new());
        let width = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .clamp(2, 8);
        std::thread::scope(|s| {
            for _ in 0..width {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&&(name, clocks, body)) = chosen.get(i) else {
                            break;
                        };
                        let outcome = if clocks
                            && !(cfg!(debug_assertions) && muse_alab::tty::INPUT_CLOCK)
                        {
                            Outcome::Skipped
                        } else {
                            let t = Instant::now();
                            match std::panic::catch_unwind(body) {
                                Ok(()) => Outcome::Passed(t.elapsed()),
                                Err(_) => Outcome::Failed,
                            }
                        };
                        let line = match outcome {
                            Outcome::Passed(d) => format!("ok ({:.1} s)", d.as_secs_f64()),
                            Outcome::Failed => "FAILED".to_string(),
                            Outcome::Skipped => {
                                "skipped (needs a debug build's test clocks and a terminal input clock)"
                                    .to_string()
                            }
                        };
                        println!("test {name} ... {line}");
                        results
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((name, outcome));
                    }
                });
            }
        });
        let results = results
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let failed: Vec<&str> = results
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Failed))
            .map(|(n, _)| *n)
            .collect();
        let passed = results
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Passed(_)))
            .count();
        let skipped = results
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Skipped))
            .count();
        if !failed.is_empty() {
            println!("\nfailures:");
            for f in &failed {
                println!("    {f}");
            }
        }
        println!(
            "\ntest result: {}. {passed} passed; {} failed; {skipped} ignored\n",
            if failed.is_empty() { "ok" } else { "FAILED" },
            failed.len()
        );
        if !failed.is_empty() {
            std::process::exit(101);
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Outcome {
        Passed(Duration),
        Failed,
        Skipped,
    }
}

// ---------------------------------------------------------------------------------------
// The fake Muse Code.
// ---------------------------------------------------------------------------------------

mod fake {
    use super::*;
    use std::sync::atomic::AtomicBool;

    static TERM: AtomicBool = AtomicBool::new(false);
    static HUP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_term(_: libc::c_int) {
        TERM.store(true, Ordering::SeqCst);
    }

    extern "C" fn on_hup(_: libc::c_int) {
        HUP.store(true, Ordering::SeqCst);
    }

    /// Whether this process is the fake: run as `…/bin/muse` with a control directory.
    pub fn is_me() -> bool {
        let argv0 = std::env::args_os().next().unwrap_or_default();
        Path::new(&argv0).file_name() == Some(OsStr::new("muse"))
            && std::env::var_os("FAKE_MUSE_CTL").is_some()
    }

    fn now_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0))
    }

    /// One argument as the events show it: `[…]`, a byte that is not printable ASCII as
    /// `\xNN`.
    fn show(arg: &OsStr) -> String {
        let mut out = String::from("[");
        for &b in arg.as_bytes() {
            if (0x20..0x7f).contains(&b) {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("\\x{b:02x}"));
            }
        }
        out.push(']');
        out
    }

    struct Fake {
        ctl: PathBuf,
        log: Option<PathBuf>,
        session: String,
        seq: u64,
        run: u64,
        open_run: Option<String>,
        tool: Option<std::process::Child>,
        ignore_term: bool,
        composer: Vec<u8>,
        deaf: bool,
    }

    impl Fake {
        fn event(&self, text: &str) {
            if let Ok(mut f) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.ctl.join("events"))
            {
                let _ = f.write_all(format!("{text}\n").as_bytes());
            }
        }

        fn record(&mut self, payload_type: &str, payload: &str) {
            let Some(log) = &self.log else {
                return;
            };
            self.seq += 1;
            let line = format!(
                r#"{{"schema_version":1,"stream":{{"kind":"session","id":"{}"}},"sequence":{},"recorded_at":{},"record_type":"event","durability":"durable","payload_type":"{payload_type}","payload_schema_version":1,"payload":{payload}}}"#,
                self.session,
                self.seq,
                now_us()
            );
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log) {
                let _ = f.write_all(format!("{line}\n").as_bytes());
            }
        }

        fn run_started(&mut self, prompt: &str) -> String {
            self.run += 1;
            let id = format!("run-{}-{}", std::process::id(), self.run);
            self.record(
                "runtime.session",
                &format!(
                    r#"{{"kind":"run","run_id":"{id}","event":{{"kind":"started","prompt":"{prompt}"}}}}"#
                ),
            );
            id
        }

        fn run_ended(&mut self, id: &str) {
            self.record(
                "runtime.session",
                &format!(
                    r#"{{"kind":"run","run_id":"{id}","event":{{"kind":"terminal","terminal":"completed","reason":null}}}}"#
                ),
            );
        }

        fn command(&mut self, line: &str) {
            let (word, arg) = line.split_once(' ').unwrap_or((line, ""));
            match word {
                "exit" => {
                    // A graceful exit: Muse Code records the session's clean end first.
                    self.record(
                        "session.end",
                        r#"{"kind":"session_end","record":{"exit_reason":"clean","uptime_ms":5}}"#,
                    );
                    self.event(&format!("exit {arg}"));
                    std::process::exit(arg.parse().unwrap_or(0));
                }
                "crash-exit" => {
                    // An abnormal exit (a panic's 101): no end record.
                    self.event(&format!("crash-exit {arg}"));
                    std::process::exit(arg.parse().unwrap_or(101));
                }
                "crash" => {
                    self.event(&format!("crash {arg}"));
                    let sig: libc::c_int = arg.parse().unwrap_or(libc::SIGKILL);
                    die_by(sig);
                }
                "open-turn" => {
                    let id = self.run_started("work");
                    self.open_run = Some(id);
                    self.event("open-turn");
                }
                "end-turn" => {
                    if let Some(id) = self.open_run.take() {
                        self.run_ended(&id);
                    }
                    self.event("end-turn");
                }
                "tool" => {
                    let child = Command::new("/bin/sleep")
                        .arg("60")
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("tool");
                    self.event(&format!("tool {}", child.id()));
                    self.tool = Some(child);
                }
                "untool" => {
                    if let Some(mut child) = self.tool.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    self.event("untool");
                }
                "ignore-term" => {
                    self.ignore_term = true;
                    self.event("ignore-term");
                }
                "deaf" => {
                    // Busy and not reading the terminal: typed keys wait in its queue.
                    self.deaf = true;
                    self.event("deaf");
                }
                "hear" => {
                    self.deaf = false;
                    self.event("hear");
                }
                "stop" => {
                    self.event("stopping");
                    // SAFETY: a stop signal to this process.
                    unsafe { libc::raise(libc::SIGSTOP) };
                    self.event("continued");
                }
                "say" => {
                    let mut out = std::io::stdout();
                    let _ = write!(out, "{arg}\r\n");
                    let _ = out.flush();
                }
                _ => self.event(&format!("unknown-command {line}")),
            }
        }

        fn input(&mut self, bytes: &[u8]) {
            let shown: String = bytes
                .iter()
                .map(|&b| {
                    if (0x20..0x7f).contains(&b) {
                        char::from(b).to_string()
                    } else {
                        format!("\\x{b:02x}")
                    }
                })
                .collect();
            self.event(&format!("input {shown}"));
            for &b in bytes {
                if b == b'\r' || b == b'\n' {
                    if !self.composer.is_empty() {
                        let prompt = String::from_utf8_lossy(&self.composer).into_owned();
                        self.composer.clear();
                        let id = self.run_started(&prompt);
                        self.run_ended(&id);
                        self.event(&format!("submit {prompt}"));
                    }
                } else {
                    self.composer.push(b);
                }
            }
        }
    }

    fn die_by(sig: libc::c_int) -> ! {
        // SAFETY: plain libc calls on this process.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
        std::process::exit(128 + sig)
    }

    /// The signals among SIGINT, SIGQUIT, SIGHUP and SIGTERM this process inherited
    /// ignored, as `INT,HUP` (`-`: none).
    fn inherited_ignores() -> String {
        let mut out = Vec::new();
        for (sig, name) in [
            (libc::SIGINT, "INT"),
            (libc::SIGQUIT, "QUIT"),
            (libc::SIGHUP, "HUP"),
            (libc::SIGTERM, "TERM"),
        ] {
            // SAFETY: a query (null new action) into a zeroed, valid out-slot.
            let ignored = unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                libc::sigaction(sig, std::ptr::null(), &raw mut old) == 0
                    && old.sa_sigaction == libc::SIG_IGN
            };
            if ignored {
                out.push(name);
            }
        }
        if out.is_empty() {
            "-".to_string()
        } else {
            out.join(",")
        }
    }

    pub fn main() -> ! {
        let ctl = PathBuf::from(std::env::var_os("FAKE_MUSE_CTL").unwrap_or_default());
        let args: Vec<OsString> = std::env::args_os().collect();
        if args.iter().skip(1).any(|a| a == "--help") {
            // The harness's build probe: Muse Code prints its usage and exits 0.
            if let Ok(mut f) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(ctl.join("events"))
            {
                let _ = writeln!(
                    f,
                    "help pid={} exe={}",
                    std::process::id(),
                    Path::new(&args[0]).display()
                );
            }
            std::process::exit(0);
        }
        let ignores = inherited_ignores();
        let cmd_file = ctl.join("cmd");
        // Commands already in the file belong to an earlier process.
        let mut done = fs::read_to_string(&cmd_file)
            .unwrap_or_default()
            .lines()
            .count();
        let session = args.windows(2).find(|w| w[0] == "resume").map_or_else(
            || SESSION.to_string(),
            |w| w[1].to_string_lossy().into_owned(),
        );
        let pid = std::process::id();
        let mut fake = Fake {
            ctl: ctl.clone(),
            log: None,
            session: session.clone(),
            seq: 0,
            run: 0,
            open_run: None,
            tool: None,
            ignore_term: false,
            composer: Vec::new(),
            deaf: false,
        };
        // SAFETY: installing plain handlers that only store to atomics.
        unsafe {
            libc::signal(
                libc::SIGTERM,
                on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
            libc::signal(
                libc::SIGHUP,
                on_hup as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
        if std::env::var_os("FAKE_MUSE_NO_SESSION").is_none() {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let dir = data.join("muse/sessions/2026/10/24").join(&session);
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(dir.join(".session.lock"), format!("pid={pid}\n"));
            fake.log = Some(dir.join("session.jsonl"));
            fake.record(
                "runtime.session.route_facts",
                &format!(
                    r#"{{"kind":"route_facts","record":{{"cwd":"/w","pid":{pid},"terminal_title_identity":"fake"}}}}"#
                ),
            );
        }
        // SAFETY: isatty and getppid take plain integers.
        let (tty, ppid) = unsafe { (libc::isatty(0) == 1, libc::getppid()) };
        if tty {
            // Raw input, as a TUI sets it: every key reaches a read, no echo; signals
            // from keys stay on.
            // SAFETY: tcgetattr/tcsetattr on fd 0 with a valid termios.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &raw mut t) == 0 {
                    t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::IEXTEN);
                    t.c_cc[libc::VMIN] = 1;
                    t.c_cc[libc::VTIME] = 0;
                    libc::tcsetattr(0, libc::TCSANOW, &raw const t);
                }
            }
        }
        let shown: String = args[1..].iter().map(|a| show(a)).collect();
        fake.event(&format!(
            "start pid={pid} ppid={ppid} tty={} ign={ignores} exe={} argv={shown}",
            u8::from(tty),
            Path::new(&args[0]).display()
        ));
        loop {
            if TERM.swap(false, Ordering::SeqCst) {
                if fake.ignore_term {
                    fake.event("term-ignored");
                } else {
                    fake.event("term");
                    die_by(libc::SIGTERM);
                }
            }
            if HUP.swap(false, Ordering::SeqCst) {
                fake.event("hup");
                die_by(libc::SIGHUP);
            }
            let lines: Vec<String> = fs::read_to_string(&cmd_file)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            for line in lines.iter().skip(done) {
                fake.command(line);
            }
            done = done.max(lines.len());
            if let Some(child) = &mut fake.tool
                && matches!(child.try_wait(), Ok(Some(_)))
            {
                fake.tool = None;
            }
            if tty && !fake.deaf {
                let mut fds = libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: poll on one valid pollfd.
                let n = unsafe { libc::poll(&raw mut fds, 1, 50) };
                if n == 1 && fds.revents & libc::POLLIN != 0 {
                    let mut buf = [0u8; 1024];
                    // SAFETY: read into a buffer of its length.
                    let got = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                    if let Ok(got) = usize::try_from(got)
                        && got > 0
                    {
                        fake.input(&buf[..got]);
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}
