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

use alab_wire::Scope;
use alab_wire::fake::{FakeAterm, POLICY_ON};

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
/// How long a test waits for something that must happen: a deadline, not a window, so a
/// passing test returns as soon as the condition holds and a loaded machine only slows it.
const PATIENCE: Duration = Duration::from_secs(30);

/// One of the harness's clocks as [`TIMING`] sets them: 0 the tick, 1 the look, 2 the person
/// grace, 3 the settle, 4 the stop wait.
fn clock(i: usize) -> Duration {
    TIMING
        .split(',')
        .nth(i)
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis)
        .expect("TIMING names the clock")
}

/// THE WINDOW A SIGNAL WOULD BE ACTED ON IN: the harness waits for its signals a tick at a
/// time and wakes at once for a caught one, so a signal it acted on (or one that killed it)
/// shows within a few ticks. An absence can only be proved by waiting, so this is a window:
/// on a loaded machine it can only miss a fault, never fail a sound harness.
fn signal_window() -> Duration {
    clock(0) * 8
}

/// THE WINDOW A MOVE WOULD BE MADE IN: a quiet session is stopped at the first look after
/// the settle and the person grace, so a harness that wrongly moves does it within one look
/// past both. Three of those, as a window for an absence (see [`signal_window`]).
fn move_window() -> Duration {
    (clock(1) + clock(2) + clock(3)) * 3
}

/// The journal time (ms since the epoch) of the last line whose word is `word`.
fn journal_ms(journal: &str, word: &str) -> Option<u64> {
    let line = journal
        .lines()
        .rfind(|l| l.split(' ').nth(2) == Some(word))?;
    let (s, ms) = line.split(' ').next()?.split_once('.')?;
    Some(s.parse::<u64>().ok()? * 1000 + ms.parse::<u64>().ok()?)
}

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
        "a_stopped_childs_stop_wait_starts_again_when_it_is_continued",
        true,
        a_stopped_childs_stop_wait_starts_again_when_it_is_continued,
    ),
    (
        "a_harness_continued_alone_starts_its_childs_stop_wait_again",
        true,
        a_harness_continued_alone_starts_its_childs_stop_wait_again,
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
        "relaunches_a_crashed_child_on_aterms_ladder",
        false,
        relaunches_a_crashed_child_on_aterms_ladder,
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
        "under_aterm_an_aterm_busy_or_off_for_now_keeps_the_child_supervised_and_claims_later",
        false,
        under_aterm_an_aterm_busy_or_off_for_now_keeps_the_child_supervised_and_claims_later,
    ),
    (
        "under_aterm_a_claim_follows_the_policy_off_and_back_on",
        false,
        under_aterm_a_claim_follows_the_policy_off_and_back_on,
    ),
    (
        "under_aterm_holds_the_session_by_a_claim_and_relaunches_on_the_ladder",
        false,
        under_aterm_holds_the_session_by_a_claim_and_relaunches_on_the_ladder,
    ),
    (
        "under_aterm_a_crash_is_passed_on_when_aterm_says_relaunch_is_off",
        false,
        under_aterm_a_crash_is_passed_on_when_aterm_says_relaunch_is_off,
    ),
    (
        "under_aterm_a_crash_while_aterm_is_away_waits_for_the_claim_and_is_relaunched",
        false,
        under_aterm_a_crash_while_aterm_is_away_waits_for_the_claim_and_is_relaunched,
    ),
    (
        "under_aterm_a_crash_is_passed_on_once_aterm_stays_away_past_the_budget",
        true,
        under_aterm_a_crash_is_passed_on_once_aterm_stays_away_past_the_budget,
    ),
    (
        "under_aterm_keys_typed_while_a_crash_waits_never_reach_the_shell",
        true,
        under_aterm_keys_typed_while_a_crash_waits_never_reach_the_shell,
    ),
    (
        "under_aterm_a_crash_before_aterm_admits_the_claim_is_passed_on_at_once",
        false,
        under_aterm_a_crash_before_aterm_admits_the_claim_is_passed_on_at_once,
    ),
    (
        "under_aterm_a_waiting_restart_is_refused_when_the_child_crashes",
        false,
        under_aterm_a_waiting_restart_is_refused_when_the_child_crashes,
    ),
    (
        "under_aterm_a_restart_is_never_made_past_its_limit",
        true,
        under_aterm_a_restart_is_never_made_past_its_limit,
    ),
    (
        "under_aterm_a_restart_waiting_on_a_child_that_refuses_to_start_is_passed_over",
        true,
        under_aterm_a_restart_waiting_on_a_child_that_refuses_to_start_is_passed_over,
    ),
    (
        "under_aterm_a_move_waits_while_the_claim_is_lost_or_given_back",
        true,
        under_aterm_a_move_waits_while_the_claim_is_lost_or_given_back,
    ),
    (
        "under_aterm_a_move_waits_until_aterm_admits_the_claim",
        true,
        under_aterm_a_move_waits_until_aterm_admits_the_claim,
    ),
    (
        "under_aterm_a_restart_aterm_asks_for_resumes_the_session",
        false,
        under_aterm_a_restart_aterm_asks_for_resumes_the_session,
    ),
    (
        "under_aterm_a_stall_restart_kills_a_child_deaf_to_sigterm",
        false,
        under_aterm_a_stall_restart_kills_a_child_deaf_to_sigterm,
    ),
    (
        "under_aterm_a_restart_waits_a_bounded_time_and_is_refused",
        true,
        under_aterm_a_restart_waits_a_bounded_time_and_is_refused,
    ),
    (
        "under_aterm_a_move_waits_for_the_upgrade_grant",
        true,
        under_aterm_a_move_waits_for_the_upgrade_grant,
    ),
    (
        "under_aterm_a_move_waits_on_aterms_composer",
        true,
        under_aterm_a_move_waits_on_aterms_composer,
    ),
    (
        "moves_itself_onto_a_newer_build_of_itself_at_a_safe_point",
        true,
        moves_itself_onto_a_newer_build_of_itself_at_a_safe_point,
    ),
    (
        "under_aterm_its_own_move_waits_on_aterm_and_claims_again_under_the_same_pid",
        true,
        under_aterm_its_own_move_waits_on_aterm_and_claims_again_under_the_same_pid,
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
    // An absence: the window a signal that ended the harness would show in.
    std::thread::sleep(signal_window());
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
    // An absence: the window a move would be made in, had the hold not held.
    std::thread::sleep(move_window());
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
            // A person types a draft, a key at a time (each read on its own before the
            // next is typed), and leaves it in the composer.
            for key in ["d", "r", "a"] {
                h.type_bytes(key.as_bytes());
                sb.wait_event(h, "input", |e| e == format!("input {key}"));
            }
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
    // An absence: the window a move would be made in, had the hold not held.
    std::thread::sleep(move_window());
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
    // An absence: the window another stop would be made in, had the harness not given up.
    std::thread::sleep(move_window());
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

/// TIME STOPPED IS NOT GRACE: a child stopped after the harness's `SIGTERM` could not act on it,
/// so its wait starts again once it is continued. NEGATIVE CONTROL: before, the harness judged
/// the stop unhonoured at its first tick after the continue, the time stopped counted.
fn a_stopped_childs_stop_wait_starts_again_when_it_is_continued() {
    let sb = Sandbox::new("stopped-stop");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    // The child stops itself as soon as it has ignored the SIGTERM, so the stop wait cannot
    // run out before the stop, however slowly this test is scheduled.
    sb.send("ignore-term stop");
    sb.wait_event(&h, "ignore-term", |_| true);
    sb.point_current(&sb.install(NEW));
    assert_eq!(
        h.wait_stopped(),
        libc::SIGSTOP,
        "the harness stops with its child"
    );
    sb.wait_event(&h, "term-ignored", |_| true);
    sb.wait_event(&h, "stopping", |_| true);
    stay_stopped_past_the_stop_wait();
    h.killpg(libc::SIGCONT);
    sb.wait_event(&h, "continued", |_| true);
    // The positive control: the whole wait after the continue, and it is judged.
    h.wait_until(&sb, "STOP-UNHONOURED", |sb| {
        sb.journal().contains("STOP-UNHONOURED")
    });
    judged_a_whole_wait_after_the_continue(&sb);
    assert!(h.running());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// TIME STOPPED IS NOT GRACE when only the harness is continued (`kill -CONT` on its pid, the
/// child still stopped): the harness continues the child, and the child's wait starts again then.
/// NEGATIVE CONTROL: before, the harness judged the stop unhonoured on the pass it was continued
/// in, the time stopped counted, before it continued the child.
fn a_harness_continued_alone_starts_its_childs_stop_wait_again() {
    let sb = Sandbox::new("stopped-alone");
    sb.point_current(&sb.install(OLD));
    let mut h = sb.spawn(&["--trust-workspace".into()], &[]);
    h.wait_start(&sb, 1);
    // The child stops itself as soon as it has ignored the SIGTERM (see above).
    sb.send("ignore-term stop");
    sb.wait_event(&h, "ignore-term", |_| true);
    sb.point_current(&sb.install(NEW));
    assert_eq!(
        h.wait_stopped(),
        libc::SIGSTOP,
        "the harness stops with its child"
    );
    sb.wait_event(&h, "term-ignored", |_| true);
    sb.wait_event(&h, "stopping", |_| true);
    // Then the harness alone is continued.
    stay_stopped_past_the_stop_wait();
    h.kill(libc::SIGCONT);
    sb.wait_event(&h, "continued", |_| true);
    h.wait_until(&sb, "STOP-UNHONOURED", |sb| {
        sb.journal().contains("STOP-UNHONOURED")
    });
    judged_a_whole_wait_after_the_continue(&sb);
    assert!(h.running());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// Stay stopped past the whole stop wait: the SIGTERM was sent before the stop this follows,
/// so a stop wait and a tick from here has spent the whole wait stopped. Time that must pass,
/// not a state to wait for; a slow machine only stops it longer.
fn stay_stopped_past_the_stop_wait() {
    std::thread::sleep(clock(4) + clock(0) * 2);
}

/// TIME STOPPED IS NOT GRACE, read from the journal's own times: the stop was judged unhonoured
/// only a whole stop wait after the harness last restarted it (`CONTINUED`), never on a pass
/// soon after the continue with the time stopped counted. The journal's wall-clock time is
/// taken just after the monotonic restart; a second's slack absorbs a preemption between the
/// two, while the stale judgement this guards against came within a tick of the continue.
fn judged_a_whole_wait_after_the_continue(sb: &Sandbox) {
    let j = sb.journal();
    let continued = journal_ms(&j, "CONTINUED").unwrap_or_else(|| panic!("no CONTINUED: {j}"));
    let judged = journal_ms(&j, "STOP-UNHONOURED").unwrap_or_else(|| panic!("not judged: {j}"));
    let slack = Duration::from_secs(1);
    let wait = u64::try_from((clock(4) - slack).as_millis()).unwrap_or(u64::MAX);
    assert!(
        judged >= continued + wait,
        "the time stopped was counted: judged {} ms after the continue, before a whole {:?}: {j}",
        judged.saturating_sub(continued),
        clock(4)
    );
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
    // An absence: the window a move would be made in, had the hold not held.
    std::thread::sleep(move_window());
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
    // An absence: the window a SIGHUP the harness acted on would show in.
    std::thread::sleep(signal_window());
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

/// A CRASH IS RELAUNCHED ON aterm's LADDER, outside aterm too (nobody else would bring the session
/// back): a 1 s pause before the first relaunch, 10 s before the second (then 60 s, then 10
/// minutes; a child that ran 10 minutes starts it over). A signal that ends a session reaching
/// the harness while a relaunch waits (here `SIGTERM`) passes the crash on: the harness ends as
/// the child did.
fn relaunches_a_crashed_child_on_aterms_ladder() {
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
    assert_eq!(field(&next, "ppid"), h.pid.to_string());
    h.wait_output(&format!(
        "Muse Code ended abnormally (exit 101); resuming session {SESSION} (crash relaunch 1 after a 1000 ms pause)"
    ));
    let j = sb.journal();
    assert!(j.contains("streak=0 pause-ms=1000"), "{j}");
    let paused = journal_ms(&j, "RELAUNCH").unwrap() - journal_ms(&j, "CRASH").unwrap();
    assert!(
        paused >= 1000,
        "relaunched {paused} ms after the crash: {j}"
    );
    // The second crash waits 10 s; a SIGTERM meanwhile ends the session.
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the second crash's pause", |sb| {
        sb.journal().contains("streak=1 pause-ms=10000")
    });
    h.kill(libc::SIGTERM);
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 2);
    assert!(
        sb.journal()
            .contains("NO-RELAUNCH SIGTERM reached the harness while the crash relaunch waited"),
        "{}",
        sb.journal()
    );
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

// ---------------------------------------------------------------------------------------
// Under aterm: the fake aterm is alab-wire's (feature `fake`), laid for this harness.
// ---------------------------------------------------------------------------------------

/// A fake aterm laid for this harness: muse-alab's roster row, the policy in effect serving
/// contracts 2, 3 and 4, a quiet session, and no claim. A scenario changes it while the harness
/// runs.
fn fake_aterm(name: &str) -> FakeAterm {
    FakeAterm::for_harness(name, &muse_alab::aterm::config())
}

/// What aterm answers a read while it shows a dialog: it cannot say now, and asks to be asked
/// again.
const DIALOG_ERR: &str = "ERR aterm is showing a dialog; answer it, then retry";

/// A scenario laid on a fake aterm: its name, what it changes before the launch, and a word
/// the journal says of it.
type Laid = (&'static str, fn(&FakeAterm), &'static str);

/// A scenario aterm lifts later: its name, what it changes before the launch, what lifts it,
/// and a word the journal says of it.
type Lifted = (&'static str, fn(&FakeAterm), fn(&FakeAterm), &'static str);

/// The fake's tab environment as [`Sandbox::spawn`] takes it.
fn pairs(env: &[(String, String)]) -> Vec<(&str, &str)> {
    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

/// Every claim the harness sent (`meta set supervisor …`), in order, without its `@<sid> `.
fn claims(aterm: &FakeAterm) -> Vec<String> {
    aterm
        .verbs()
        .into_iter()
        .filter(|verb| verb.starts_with("meta set supervisor "))
        .collect()
}

/// The words every contract-4 claim of the harness `pid` starts with.
fn claim_of(pid: libc::pid_t) -> String {
    format!(
        "meta set supervisor muse-alab@{pid} contract=4 scope=relaunch,upgrade version={VERSION}"
    )
}

/// How many journal lines carry `word` as their event.
fn notes(journal: &str, word: &str) -> usize {
    journal
        .lines()
        .filter(|l| l.split(' ').nth(2) == Some(word))
        .count()
}

/// THE FAIL-CLOSED RULE (aterm's harness design §3.2, §8.2): in an aterm tab, wherever aterm
/// refuses the harness's claim of both duties, Muse Code runs in the harness's place (the same
/// pid) and the harness says why in its journal: no control socket answers, aterm's window
/// cannot host a harness (`why=unhosted`), no registry row proves the holder, the claim does not
/// read back proven under contract 3, the harness has no roster row under contract 3 (it is then
/// never sent a claim), or aterm granted less than both duties (the claim is given back).
fn under_aterm_passes_through_where_no_claim_is_proven() {
    {
        let sb = Sandbox::new("aterm-no-socket");
        sb.point_current(&sb.install(OLD));
        let run = sb.root.join("run").display().to_string();
        let env = [
            ("TERM_PROGRAM", "aterm"),
            ("ATERM_PARENT_SESSION_ID", "s-1a"),
            ("XDG_RUNTIME_DIR", run.as_str()),
        ];
        let mut h = sb.spawn(&["--trust-workspace".into()], &env);
        let start = h.wait_start(&sb, 1);
        assert_eq!(
            field(&start, "pid"),
            h.pid.to_string(),
            "in the harness's place"
        );
        let j = sb.journal();
        assert!(j.contains(" UNCLAIMED ") && j.contains(" PASS "), "{j}");
        sb.send("exit 3");
        assert_eq!(h.wait_exit(), Exit::Code(3));
    }
    let cases: [Laid; 6] = [
        (
            "aterm-unhosted",
            |a| {
                a.set_policy(
                    &POLICY_ON.replace("effective=on why=enabled", "effective=off why=unhosted"),
                )
            },
            "why=unhosted",
        ),
        (
            "aterm-unregistered",
            |a| a.set_registered(false),
            "unproven",
        ),
        (
            "aterm-unproven-3",
            |a| {
                a.serve_contracts(&[2, 3]);
                a.set_proven(false);
            },
            "supervisor_proven=0",
        ),
        (
            "aterm-unrostered-3",
            |a| {
                a.serve_contracts(&[2, 3]);
                a.set_row("muse-alab", &[]);
                a.set_row("grok-alab", &Scope::CONTRACT_3);
            },
            "unrostered",
        ),
        (
            "aterm-narrow-4",
            |a| a.set_known(&[Scope::Upgrade]),
            "aterm granted scope=upgrade (the claim needs relaunch,upgrade)",
        ),
        (
            "aterm-narrow-3",
            |a| {
                a.serve_contracts(&[2, 3]);
                a.set_cap_after_next_claim(Some(&[Scope::Upgrade]));
            },
            "supervisor_scope=upgrade",
        ),
    ];
    for (name, lay, says) in cases {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm = fake_aterm(name);
        lay(&aterm);
        let env = aterm.env();
        let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
        let start = h.wait_start(&sb, 1);
        assert_eq!(
            field(&start, "pid"),
            h.pid.to_string(),
            "{name}: Muse Code replaced the harness"
        );
        assert_eq!(field(&start, "argv"), "[--trust-workspace]", "{name}");
        let j = sb.journal();
        assert!(j.contains(" PASS ") && j.contains(says), "{name}: {j}");
        let verbs = aterm.verbs();
        assert!(
            verbs.iter().any(|v| v == "harness policy"),
            "{name}: {verbs:?}"
        );
        if name == "aterm-unrostered-3" {
            assert!(verbs.iter().any(|v| v == "native-act info"), "{verbs:?}");
            assert!(
                claims(&aterm).is_empty(),
                "a claim was sent off the roster: {verbs:?}"
            );
        }
        if matches!(
            name,
            "aterm-unproven-3" | "aterm-narrow-4" | "aterm-narrow-3"
        ) {
            assert!(
                verbs.contains(&format!("meta unset supervisor holder=muse-alab@{}", h.pid)),
                "{name}: a claim that does not stand is let go: {verbs:?}"
            );
            assert_eq!(aterm.holder(), None, "{name}");
        }
        sb.send("exit 3");
        assert_eq!(h.wait_exit(), Exit::Code(3), "{name}");
    }
}

/// AN aterm THAT IS BUSY, OFF FOR NOW OR NOT YET SERVING A CONTRACT THIS HARNESS CLAIMS AT IS
/// NOT AWAY: it answered the handshake and then late, said `ERR` to its policy (a dialog up),
/// has its Autopilot switched off, or serves contract 2 alone. Muse Code runs as the harness's
/// child (never in its place, unsupervised for the session), nothing is claimed meanwhile, and
/// the claim is made on the reclaim clock once aterm admits it, naming the child that runs.
fn under_aterm_an_aterm_busy_or_off_for_now_keeps_the_child_supervised_and_claims_later() {
    let cases: [Lifted; 4] = [
        (
            "aterm-busy",
            |a| a.set_reply_delay(Duration::from_millis(900)),
            |a| a.set_reply_delay(Duration::ZERO),
            "(aterm answered the handshake)",
        ),
        (
            "aterm-dialog",
            |a| a.set_policy_reply(DIALOG_ERR),
            FakeAterm::clear_policy_reply,
            "policy reply=ERR:aterm",
        ),
        (
            "aterm-autopilot-off",
            |a| {
                a.set_policy(&POLICY_ON.replace(
                    "effective=on why=enabled",
                    "effective=off why=autopilot-off",
                ));
            },
            |a| a.set_policy(POLICY_ON),
            "why=autopilot-off",
        ),
        (
            "aterm-contract-2",
            |a| a.serve_contracts(&[2]),
            |a| a.serve_contracts(&[2, 3, 4]),
            "contract=2",
        ),
    ];
    for (name, lay, lift, says) in cases {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm = fake_aterm(name);
        lay(&aterm);
        let env = aterm.env();
        let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
        let first = h.wait_start(&sb, 1);
        assert_eq!(
            field(&first, "ppid"),
            h.pid.to_string(),
            "{name}: the harness's child, not Muse Code in its place"
        );
        let j = sb.journal();
        assert!(
            j.contains(" UNCLAIMED ") && j.contains(says) && !j.contains(" PASS "),
            "{name}: {j}"
        );
        if name != "aterm-busy" {
            assert!(claims(&aterm).is_empty(), "{name}: {:?}", aterm.verbs());
        }
        lift(&aterm);
        h.wait_until(&sb, "the claim made once aterm admits it", |sb| {
            sb.journal().contains(" CLAIMED ")
        });
        let child = field(&first, "pid");
        assert!(
            claims(&aterm)
                .iter()
                .any(|c| c.starts_with(&claim_of(h.pid)) && c.contains(&format!(" child={child}:"))),
            "{name}: claimed on the clock with the child that runs: {:?}",
            claims(&aterm)
        );
        sb.send("exit 3");
        assert_eq!(h.wait_exit(), Exit::Code(3), "{name}");
    }
}

/// A CLAIM FOLLOWS aterm's POLICY: switched off (here the session's Autopilot), it is given
/// back (`RELEASED`, `meta unset supervisor`) and nothing is relaunched or moved under it; back
/// on, it is made again through the whole guarded sequence.
fn under_aterm_a_claim_follows_the_policy_off_and_back_on() {
    let sb = Sandbox::new("aterm-policy");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-policy");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_policy(&POLICY_ON.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    h.wait_until(&sb, "the claim given back", |sb| {
        sb.journal().contains(" RELEASED ")
    });
    assert!(
        aterm
            .verbs()
            .contains(&format!("meta unset supervisor holder=muse-alab@{}", h.pid)),
        "{:?}",
        aterm.verbs()
    );
    assert_eq!(aterm.holder(), None);
    aterm.set_policy(POLICY_ON);
    h.wait_until(&sb, "the claim made again", |sb| {
        notes(&sb.journal(), "CLAIMED") >= 2
    });
    assert_eq!(aterm.holder(), Some(format!("muse-alab@{}", h.pid)));
    sb.send("exit 3");
    assert_eq!(h.wait_exit(), Exit::Code(3));
}

/// UNDER A CLAIM the harness supervises: it claims with its holder, contract, scope, version and
/// vendor build, then announces the claim again with each child it starts (`child=<pid>:<start>`);
/// a crash is relaunched on aterm's ladder (a 1 s pause first) only after one fresh read of aterm
/// (the policy and `status`). A second crash waits the ladder's 10 s: a restart aterm asks for
/// meanwhile is refused (no child runs), and a `SIGINT` to the harness passes the crash on, the
/// harness ending as the child did.
fn under_aterm_holds_the_session_by_a_claim_and_relaunches_on_the_ladder() {
    let sb = Sandbox::new("aterm-claimed");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-claimed");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    let first = h.wait_start(&sb, 1);
    assert_eq!(
        field(&first, "ppid"),
        h.pid.to_string(),
        "the harness's child"
    );
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    let sent = claims(&aterm);
    assert_eq!(
        sent[0],
        format!("{} vendor={OLD}", claim_of(h.pid)),
        "{sent:?}"
    );
    assert!(
        sent[1].starts_with(&format!(
            "{} vendor={OLD} child={}:",
            claim_of(h.pid),
            field(&first, "pid")
        )),
        "{sent:?}"
    );
    let j = sb.journal();
    assert!(
        j.contains(" CLAIMED ") && j.contains(" REANNOUNCED "),
        "{j}"
    );
    let before = aterm.verbs().len();
    sb.send(&format!("crash {}", libc::SIGKILL));
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    let fresh: Vec<String> = aterm.verbs().split_off(before);
    assert!(
        fresh.iter().any(|v| v == "harness policy") && fresh.iter().any(|v| v == "status"),
        "the relaunch waited on a fresh read of aterm: {fresh:?}"
    );
    let j = sb.journal();
    assert!(j.contains("streak=0 pause-ms=1000"), "{j}");
    let paused = journal_ms(&j, "RELAUNCH").unwrap() - journal_ms(&j, "CRASH").unwrap();
    assert!(
        paused >= 1000,
        "relaunched {paused} ms after the crash: {j}"
    );
    let pid2 = field(&second, "pid");
    h.wait_until(&sb, "the new child announced", |_| {
        aterm
            .child()
            .is_some_and(|c| c.starts_with(&format!("{pid2}:")))
    });
    // The second crash waits 10 s.
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the second crash", |sb| {
        sb.journal().contains("streak=1 pause-ms=10000")
    });
    aterm.send_restart("asked");
    h.wait_until(&sb, "the restart refused while no child runs", |sb| {
        sb.journal()
            .contains("RESTART-REFUSED cause=asked crash-pending")
    });
    h.kill(libc::SIGINT);
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 2, "the crash was passed on");
    let j = sb.journal();
    assert!(
        j.contains(
            "NO-RELAUNCH SIGINT or SIGQUIT reached the harness while the crash relaunch waited"
        ),
        "{j}"
    );
}

/// A CRASH aterm's `[harness] relaunch = false` covers is passed on: the harness ends as the child
/// did, nothing relaunched.
fn under_aterm_a_crash_is_passed_on_when_aterm_says_relaunch_is_off() {
    let sb = Sandbox::new("aterm-relaunch-off");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-relaunch-off");
    aterm.set_policy(&POLICY_ON.replace("relaunch=true", "relaunch=false"));
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 1);
    let j = sb.journal();
    assert!(
        j.contains("NO-RELAUNCH aterm's word: relaunch=false"),
        "{j}"
    );
}

/// A CRASH WHILE aterm IS AWAY is still relaunched: in a tab the harness leads no built-in lane of
/// aterm's can bring the child back, so the crash waits (`CRASH-WAIT`) while the claim is made
/// again on the reclaim clock (here refused once: no registry row proves it), and is relaunched
/// once aterm proves it again. The wait sleeps between its asks: past the ladder's pause it costs
/// the harness under a tenth of the time in CPU (NEGATIVE CONTROL: a wait capped at the pause's
/// end spun on a zero timeout, a whole core on an idle machine). A line typed at the dead screen
/// meanwhile never reaches the resumed Muse Code: the relaunched child reads only what is typed
/// after it starts.
fn under_aterm_a_crash_while_aterm_is_away_waits_for_the_claim_and_is_relaunched() {
    let sb = Sandbox::new("aterm-away");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-away");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_registered(false);
    aterm.restart();
    h.wait_until(&sb, "the link lost", |sb| sb.journal().contains(" LOST "));
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the crash waiting for aterm", |sb| {
        sb.journal().contains(" CRASH-WAIT ")
    });
    h.type_bytes(b"retry\r");
    h.wait_until(&sb, "a refused reclaim", |sb| {
        sb.journal().contains(" UNCLAIMED ")
    });
    // Past the 1 s pause (the reclaim is 5 s after the loss), aterm still away.
    let window = Duration::from_secs(2);
    let (cpu, wall) = (cpu_time(h.pid), Instant::now());
    std::thread::sleep(window);
    let (cpu, wall) = (cpu_time(h.pid) - cpu, wall.elapsed());
    assert_eq!(
        sb.starts().len(),
        1,
        "no relaunch before aterm proves the claim"
    );
    assert!(
        cpu < wall / 10,
        "the crash wait used {cpu:?} of CPU in {wall:?} while aterm was away"
    );
    aterm.set_registered(true);
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    let j = sb.journal();
    assert_eq!(notes(&j, "CLAIMED"), 2, "{j}");
    assert!(!j.contains("NO-RELAUNCH"), "{j}");
    // What the relaunched child reads first is what was typed after it started.
    h.type_bytes(b"after\r");
    sb.wait_event(&h, "submit", |e| e == "submit after");
    assert!(
        !sb.events().iter().any(|e| e.contains("retry")),
        "a line typed while the crash waited reached the resumed child: {:?}",
        sb.events()
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// KEYS TYPED WHILE A CRASH WAITS NEVER REACH THE SHELL: the harness runs as a shell's command,
/// the crash waits for an aterm that stays away past the away budget (1.5 s here), a person types
/// a line at the dead screen meanwhile, and the crash is passed on. The shell's next read gets
/// only what is typed after the harness ended (NEGATIVE CONTROL: before, the shell read the line
/// typed during the wait, and an interactive one would have run it as a command).
fn under_aterm_keys_typed_while_a_crash_waits_never_reach_the_shell() {
    let sb = Sandbox::new("aterm-away-keys");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-away-keys");
    let mut env = aterm.env();
    env.push(("MUSE_ALAB_TEST_TIMING".into(), format!("{TIMING},1500")));
    let mut h = sb.spawn_under_shell(
        r#""$0" --trust-workspace; st=$?; IFS= read -r line; printf 'harness ended %s; shell read [%s]\n' "$st" "$line""#,
        &pairs(&env),
    );
    let first = h.wait_start(&sb, 1);
    let harness: libc::pid_t = field(&first, "ppid").parse().expect("the harness's pid");
    assert_ne!(harness, h.pid, "the harness is the shell's command");
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_registered(false);
    aterm.restart();
    h.wait_until(&sb, "the link lost", |sb| sb.journal().contains(" LOST "));
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the crash", |sb| sb.journal().contains(" CRASH "));
    h.type_bytes(b"retry\r");
    h.wait_until(&sb, "the crash passed on", |sb| {
        sb.journal().contains("NO-RELAUNCH claim-not-proven-again")
    });
    // SAFETY: a null signal to a pid this test saw start; it only asks whether it exists.
    h.wait_until(&sb, "the harness ended", |_| unsafe {
        libc::kill(harness, 0) != 0
    });
    // An absence: the window the shell would read a line still queued for it in.
    std::thread::sleep(signal_window());
    assert!(
        !h.output().contains("shell read ["),
        "the shell read a line typed while the crash waited: {}",
        h.output()
    );
    h.type_bytes(b"after\r");
    h.wait_output("shell read [");
    let out = h.output();
    assert!(
        out.contains(&format!(
            "harness ended {}; shell read [after]",
            128 + libc::SIGKILL
        )),
        "{out}"
    );
    assert_eq!(h.wait_exit(), Exit::Code(0));
    assert_eq!(sb.starts().len(), 1, "nothing relaunched");
}

/// A CRASH BEFORE aterm ADMITS THE CLAIM is passed on at once (`NO-RELAUNCH no-claim`): with
/// Autopilot off at the launch the harness supervises Muse Code and claims every 5 s, but aterm
/// never let it lead the tab, so it does not wait out aterm's 45 s away grace for a claim nobody
/// lost (NEGATIVE CONTROL: before, the crash waited, `CRASH-WAIT … link-lost`, the full 45 s).
fn under_aterm_a_crash_before_aterm_admits_the_claim_is_passed_on_at_once() {
    let sb = Sandbox::new("aterm-unadmitted-crash");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-unadmitted-crash");
    aterm.set_policy(&POLICY_ON.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    ));
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    let first = h.wait_start(&sb, 1);
    assert_eq!(field(&first, "ppid"), h.pid.to_string(), "supervised");
    h.wait_until(&sb, "the claim not made", |sb| {
        sb.journal().contains(" UNCLAIMED ")
    });
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 1, "nothing relaunched");
    let j = sb.journal();
    assert!(
        j.contains("NO-RELAUNCH no-claim: aterm has not admitted this harness's claim"),
        "{j}"
    );
    assert!(!j.contains(" CRASH-WAIT "), "{j}");
    assert!(claims(&aterm).is_empty(), "{:?}", aterm.verbs());
}

/// A CRASH IS PASSED ON once aterm stays away past the away budget (shortened to 1.5 s here, 45 s
/// in production): the harness waits it (`CRASH-WAIT`), then ends as the child ended, with
/// nothing relaunched (`NO-RELAUNCH claim-not-proven-again`).
fn under_aterm_a_crash_is_passed_on_once_aterm_stays_away_past_the_budget() {
    let sb = Sandbox::new("aterm-away-past");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-away-past");
    let mut env = aterm.env();
    env.push(("MUSE_ALAB_TEST_TIMING".into(), format!("{TIMING},1500")));
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_registered(false);
    aterm.restart();
    h.wait_until(&sb, "the link lost", |sb| sb.journal().contains(" LOST "));
    sb.send(&format!("crash {}", libc::SIGKILL));
    assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL));
    assert_eq!(sb.starts().len(), 1, "nothing relaunched");
    let j = sb.journal();
    assert!(
        j.contains(" CRASH-WAIT ") && j.contains("NO-RELAUNCH claim-not-proven-again"),
        "{j}"
    );
}

/// A RESTART aterm ASKS FOR (`restart cause=asked`, a person's `aterm ctl restart`) ends the child
/// with `SIGTERM` and resumes the same session at once, no safe point waited for and no crash
/// counted; it is made whatever `[harness] relaunch` says, which limits crashes only. One that
/// names a model (`cause=model model=<word>`) starts Muse Code on it (`--model=<word>` before
/// `resume`), and one onto another account is refused, the child running on: Muse Code takes no
/// account home this harness can set. Each new child is announced.
fn under_aterm_a_restart_aterm_asks_for_resumes_the_session() {
    let sb = Sandbox::new("aterm-restart");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-restart");
    aterm.set_policy(&POLICY_ON.replace("relaunch=true", "relaunch=false"));
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.send_restart("asked");
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    assert!(sb.events().iter().any(|e| e == "term"), "{:?}", sb.events());
    let j = sb.journal();
    assert!(j.contains(" RESTART cause=asked: child="), "{j}");
    assert!(!j.contains(" CRASH "), "a restart is no crash: {j}");
    let pid2 = field(&second, "pid");
    h.wait_until(&sb, "the new child announced", |_| {
        aterm
            .child()
            .is_some_and(|c| c.starts_with(&format!("{pid2}:")))
    });
    // An account move is refused; the child runs on.
    aterm.send_account_restart("account", "work", None);
    h.wait_until(&sb, "the account move refused", |sb| {
        sb.journal()
            .contains("RESTART-REFUSED cause=account to=work")
    });
    // A model move starts Muse Code on aterm's model.
    aterm.send_model_restart("model", "lab-m2");
    let third = h.wait_start(&sb, 3);
    assert_eq!(
        field(&third, "argv"),
        format!("[--trust-workspace][--model=lab-m2][resume][{SESSION}]")
    );
    assert!(
        sb.journal()
            .contains(" RESTART cause=model model=lab-m2: child=")
    );
    assert_eq!(sb.starts().len(), 3, "the account move started nothing");
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A RESTART FOR A STALL is made whatever the child does with its `SIGTERM`: a child that ignores
/// it gets `SIGKILL` after the stall's grace (the stop wait here, at most aterm's 5 s), and the
/// session is resumed as for any restart. Another restart aterm sends while that child is being
/// ended is passed over (`RESTART-PASSED-OVER … already being ended`): it was meant for the
/// child that is going, and the child resumed after it is never restarted for it.
fn under_aterm_a_stall_restart_kills_a_child_deaf_to_sigterm() {
    let sb = Sandbox::new("aterm-stall");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-stall");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    sb.send("ignore-term");
    sb.wait_event(&h, "ignore-term", |_| true);
    aterm.send_restart("stall");
    sb.wait_event(&h, "term-ignored", |_| true);
    aterm.send_restart("asked");
    h.wait_until(&sb, "the second restart passed over", |sb| {
        sb.journal()
            .contains("RESTART-PASSED-OVER cause=asked the child is already being ended")
    });
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    let j = sb.journal();
    assert!(j.contains(" RESTART-KILL cause=stall "), "{j}");
    let killed = journal_ms(&j, "RESTART-KILL").unwrap() - journal_ms(&j, "RESTART").unwrap();
    assert!(
        killed >= clock(4).as_millis() as u64,
        "SIGKILL {killed} ms after SIGTERM: {j}"
    );
    // An absence: the window the passed-over restart would have ended the resumed child in.
    std::thread::sleep(signal_window());
    assert_eq!(sb.starts().len(), 2, "{:?}", sb.events());
    assert_eq!(notes(&sb.journal(), "RESTART"), 1, "{}", sb.journal());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A RESTART WAITING WHEN ITS CHILD CRASHES is refused (`RESTART-REFUSED … child-ended`): aterm
/// asked it for that child (here while aterm said `ERR` to its policy, a dialog up, so it waited),
/// and the child the crash relaunch resumes once aterm answers again is never restarted for it.
fn under_aterm_a_waiting_restart_is_refused_when_the_child_crashes() {
    let sb = Sandbox::new("aterm-restart-crash");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-restart-crash");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_policy_reply(DIALOG_ERR);
    aterm.send_restart("asked");
    h.wait_until(&sb, "the restart waiting for aterm", |sb| {
        sb.journal()
            .contains("RESTART-WAITS cause=asked aterm did not answer")
    });
    sb.send(&format!("crash {}", libc::SIGKILL));
    h.wait_until(&sb, "the restart refused with its child", |sb| {
        sb.journal()
            .contains("RESTART-REFUSED cause=asked child-ended")
    });
    h.wait_until(&sb, "the crash waiting for aterm", |sb| {
        sb.journal().contains(" CRASH-WAIT ")
    });
    aterm.clear_policy_reply();
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    // An absence: the window a restart still waiting would be made in (asked again each second).
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(sb.starts().len(), 2, "{:?}", sb.events());
    let j = sb.journal();
    assert_eq!(notes(&j, "RESTART"), 0, "{j}");
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A RESTART IS NEVER MADE PAST ITS LIMIT: one waiting for aterm (a dialog up) when the harness is
/// stopped, and continued only after its limit (3 s here, 45 s in production) has passed, is
/// refused on the first pass after the continue, though aterm answers again by then; the child is
/// never signalled (NEGATIVE CONTROL: before, that pass asked aterm first, and a permitted
/// restart ended the child for a request long gone).
fn under_aterm_a_restart_is_never_made_past_its_limit() {
    let sb = Sandbox::new("aterm-restart-late");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-restart-late");
    let mut env = aterm.env();
    env.push(("MUSE_ALAB_TEST_TIMING".into(), format!("{TIMING},3000")));
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_policy_reply(DIALOG_ERR);
    aterm.send_restart("asked");
    h.wait_until(&sb, "the restart waiting for aterm", |sb| {
        sb.journal()
            .contains("RESTART-WAITS cause=asked aterm did not answer")
    });
    h.kill(libc::SIGSTOP);
    assert_eq!(h.wait_stopped(), libc::SIGSTOP);
    // Time that must pass: the restart's whole limit, and a little more, stopped.
    std::thread::sleep(Duration::from_millis(3500));
    aterm.clear_policy_reply();
    h.kill(libc::SIGCONT);
    h.wait_until(&sb, "the restart refused past its limit", |sb| {
        sb.journal()
            .contains("RESTART-REFUSED cause=asked not made within 3000 ms")
    });
    // An absence: the window a restart made on that pass would have ended the child in.
    std::thread::sleep(signal_window());
    assert!(
        !sb.events().iter().any(|e| e.starts_with("term")),
        "{:?}",
        sb.events()
    );
    assert_eq!(sb.starts().len(), 1);
    assert_eq!(notes(&sb.journal(), "RESTART"), 0, "{}", sb.journal());
    assert!(h.running());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A RESTART WAITING ON A CHILD THAT REFUSES TO START is passed over at the next child's start
/// (`RESTART-PASSED-OVER … it waited for the child before this one`): a move starts a new build,
/// aterm asks a restart while that build has no session yet (so it waits), the build refuses to
/// start and the harness falls back onto the build it ran; that child is never restarted for a
/// request aterm made of the one before it.
fn under_aterm_a_restart_waiting_on_a_child_that_refuses_to_start_is_passed_over() {
    let sb = Sandbox::new("aterm-restart-fallback");
    let old = sb.install(OLD);
    sb.point_current(&old);
    let aterm = fake_aterm("aterm-restart-fallback");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    // Answers its probe, then refuses to start once the test says so.
    let go = sb.root.join("ctl").join("go");
    let refusing = sb.install_script(
        NEW,
        &format!(
            "if [ \"$1\" = --help ]; then exit 0; fi\nwhile [ ! -e '{}' ]; do sleep 0.05; done\nexit 1",
            go.display()
        ),
    );
    sb.point_current(&refusing);
    h.wait_until(&sb, "the move onto the new build announced", |sb| {
        sb.journal().contains(" RELAUNCH moving ") && notes(&sb.journal(), "REANNOUNCED") >= 2
    });
    aterm.send_restart("asked");
    h.wait_until(
        &sb,
        "the restart waiting for the new child's session",
        |sb| {
            sb.journal()
                .contains("RESTART-WAITS cause=asked no session lock")
        },
    );
    fs::write(&go, b"").expect("go");
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "exe"),
        old.join("bin").join("muse").display().to_string(),
        "fell back onto the build it ran"
    );
    let j = sb.journal();
    assert!(
        j.contains("RESTART-PASSED-OVER cause=asked it waited for the child before this one"),
        "{j}"
    );
    // An absence: the window a restart still waiting would be made in (asked again each second).
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(sb.starts().len(), 2, "{:?}", sb.events());
    assert_eq!(notes(&sb.journal(), "RESTART"), 0, "{}", sb.journal());
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A RESTART THAT CANNOT BE MADE YET waits, bounded: with no session lock naming the child it
/// waits (`RESTART-WAITS`) and is refused once the away budget (1.5 s here, 45 s in production)
/// has passed since it arrived; the child runs on, never signalled.
fn under_aterm_a_restart_waits_a_bounded_time_and_is_refused() {
    let sb = Sandbox::new("aterm-restart-waits");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-restart-waits");
    let mut env = aterm.env();
    env.push(("MUSE_ALAB_TEST_TIMING".into(), format!("{TIMING},1500")));
    env.push(("FAKE_MUSE_NO_SESSION".into(), "1".into()));
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.send_restart("asked");
    h.wait_until(&sb, "the restart refused", |sb| {
        sb.journal()
            .contains("RESTART-REFUSED cause=asked not made within 1500 ms")
    });
    let j = sb.journal();
    assert!(
        j.contains("RESTART-WAITS cause=asked no session lock"),
        "{j}"
    );
    assert!(!sb.events().iter().any(|e| e.starts_with("term")));
    assert_eq!(sb.starts().len(), 1);
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// A MOVE IS aterm's `upgrade` DUTY: once a person's cap narrows the claim to `relaunch`, the
/// harness holds every move (`upgrade-not-granted`) however quiet the session, and moves again
/// once aterm grants `upgrade` back.
fn under_aterm_a_move_waits_for_the_upgrade_grant() {
    let sb = Sandbox::new("aterm-cap");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-cap");
    let env = aterm.env();
    let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
    h.wait_start(&sb, 1);
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    aterm.set_cap(Some(&[Scope::Relaunch]));
    h.wait_until(&sb, "the narrowed grant", |sb| {
        sb.journal().contains(" NARROWED scope=relaunch ")
    });
    sb.point_current(&sb.install(NEW));
    h.wait_until(&sb, "the move held", |sb| {
        sb.journal().contains("waits: upgrade-not-granted (")
    });
    // An absence: the window a move would be made in, had the grant not held it.
    std::thread::sleep(move_window());
    assert!(
        !sb.events().iter().any(|e| e.starts_with("term")),
        "{:?}",
        sb.events()
    );
    assert_eq!(sb.starts().len(), 1);
    aterm.set_cap_restores(true);
    aterm.set_cap(None);
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    assert!(
        field(&second, "exe").contains(
            &muse_alab::version::Version::parse(NEW)
                .unwrap()
                .build_id()
                .to_string()
        )
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// The policy with the session's Autopilot switched off.
fn policy_off() -> String {
    POLICY_ON.replace(
        "effective=on why=enabled",
        "effective=off why=autopilot-off",
    )
}

/// A newer build installed while no claim stands, `word` holding the move however quiet the
/// session: the hold is journalled, and nothing is signalled or started in the window the move
/// would be made in.
fn move_held(sb: &Sandbox, h: &Harness, name: &str, word: &str) {
    sb.point_current(&sb.install(NEW));
    let waits = format!("waits: {word} (");
    h.wait_until(sb, &waits, |sb| sb.journal().contains(&waits));
    // An absence: the window a move would be made in, had the hold not held.
    std::thread::sleep(move_window());
    assert!(
        !sb.events().iter().any(|e| e.starts_with("term")),
        "{name}: {:?}",
        sb.events()
    );
    assert_eq!(sb.starts().len(), 1, "{name}");
}

/// The move made once aterm admits the claim again: the session resumed on the newer build.
fn move_made(sb: &Sandbox, h: &Harness, name: &str) {
    let second = h.wait_start(sb, 2);
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]"),
        "{name}"
    );
    assert_eq!(field(&second, "ppid"), h.pid.to_string(), "{name}");
}

/// A MOVE WAITS WHILE THE CLAIM IS LOST OR GIVEN BACK: a link lost and being claimed again (aterm
/// restarted and does not prove the claim yet) holds every move (`no-claim`), and so does a claim
/// given back while Autopilot is off (`policy-off`); the move is made once aterm admits the claim
/// again.
fn under_aterm_a_move_waits_while_the_claim_is_lost_or_given_back() {
    for name in ["aterm-move-lost", "aterm-move-released"] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm = fake_aterm(name);
        let env = aterm.env();
        let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
        h.wait_start(&sb, 1);
        h.wait_until(&sb, "the claim and its announcement", |_| {
            claims(&aterm).len() >= 2
        });
        if name == "aterm-move-lost" {
            aterm.set_registered(false);
            aterm.restart();
            h.wait_until(&sb, "the link lost", |sb| sb.journal().contains(" LOST "));
            move_held(&sb, &h, name, "no-claim");
            aterm.set_registered(true);
        } else {
            aterm.set_policy(&policy_off());
            h.wait_until(&sb, "the claim given back", |sb| {
                sb.journal().contains(" RELEASED ")
            });
            move_held(&sb, &h, name, "policy-off");
            aterm.set_policy(POLICY_ON);
        }
        move_made(&sb, &h, name);
        sb.send("exit 0");
        assert_eq!(h.wait_exit(), Exit::Code(0), "{name}");
    }
}

/// A MOVE WAITS UNTIL aterm ADMITS THE CLAIM: with Autopilot off at the launch the harness
/// supervises Muse Code and claims every 5 s, but a move is aterm's `upgrade` duty and none was
/// handed over, so atpkg's newer Muse Code waits (`no-claim`) until aterm admits the claim, and
/// is moved onto then. Refused for good after such a launch (no registry row proves the claim),
/// the link is absent for the session and every move waits for good, as a crash is passed on at
/// once. NEGATIVE CONTROL: before, a link that never held a claim counted as outside aterm, and
/// the harness moved with no grant and no read of aterm.
fn under_aterm_a_move_waits_until_aterm_admits_the_claim() {
    for name in ["aterm-move-unadmitted", "aterm-move-refused"] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm = fake_aterm(name);
        aterm.set_policy(&policy_off());
        let env = aterm.env();
        let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
        let first = h.wait_start(&sb, 1);
        assert_eq!(field(&first, "ppid"), h.pid.to_string(), "{name}");
        h.wait_until(&sb, "the claim not made", |sb| {
            sb.journal().contains(" UNCLAIMED ")
        });
        if name == "aterm-move-unadmitted" {
            move_held(&sb, &h, name, "no-claim");
            assert!(claims(&aterm).is_empty(), "{name}: {:?}", aterm.verbs());
            aterm.set_policy(POLICY_ON);
            move_made(&sb, &h, name);
            sb.send("exit 0");
            assert_eq!(h.wait_exit(), Exit::Code(0), "{name}");
        } else {
            aterm.set_registered(false);
            aterm.set_policy(POLICY_ON);
            h.wait_until(&sb, "the claim refused", |sb| {
                sb.journal().contains("unproven")
            });
            move_held(&sb, &h, name, "no-claim");
            sb.send(&format!("crash {}", libc::SIGKILL));
            assert_eq!(h.wait_exit(), Exit::Signal(libc::SIGKILL), "{name}");
            assert_eq!(sb.starts().len(), 1, "{name}");
            let j = sb.journal();
            assert!(
                j.contains("NO-RELAUNCH no-claim: aterm refused this harness's claim"),
                "{name}: {j}"
            );
        }
    }
}

/// UNDER ATERM A MOVE'S COMPOSER IS ATERM'S WORD (`status composer=`, contract 3's §8.1 item 9 in
/// aterm's harness design): input after the last submission (a focus report, which types no draft
/// but moves the terminal's input clock past the submission) holds no move while aterm reads the
/// composer `empty`, and aterm's `draft` and `-` hold one. NEGATIVE CONTROL: the same input under an
/// aterm that serves no `composer=` holds the move on the input clock (`draft-unproven`).
fn under_aterm_a_move_waits_on_aterms_composer() {
    for (name, served) in [("aterm-composer", true), ("aterm-no-composer", false)] {
        let sb = Sandbox::new(name);
        sb.point_current(&sb.install(OLD));
        let aterm = fake_aterm(name);
        if served {
            aterm.set_status("composer", Some("draft"));
        } else {
            aterm.set_status("composer", None);
        }
        let env = aterm.env();
        let mut h = sb.spawn(&["--trust-workspace".into()], &pairs(&env));
        h.wait_start(&sb, 1);
        next_second();
        h.type_bytes(b"go\r");
        sb.wait_event(&h, "submit", |e| e == "submit go");
        next_second();
        h.type_bytes(b"\x1b[I");
        sb.wait_event(&h, "input", |e| e == "input \\x1b[I");
        sb.point_current(&sb.install(NEW));
        let held = |word: &str| {
            let word = format!("waits: {word} (");
            h.wait_until(&sb, &word, |sb| sb.journal().contains(&word));
        };
        if served {
            held("draft");
            aterm.set_status("composer", Some("-"));
            held("composer-unproven");
            assert!(!sb.events().iter().any(|e| e.starts_with("term")));
            assert_eq!(sb.starts().len(), 1);
            aterm.set_status("composer", Some("empty"));
            let second = h.wait_start(&sb, 2);
            assert_eq!(
                field(&second, "argv"),
                format!("[--trust-workspace][resume][{SESSION}]")
            );
            assert_eq!(field(&second, "ppid"), h.pid.to_string());
        } else {
            held("draft-unproven");
            // An absence: the window a move would be made in, had the hold not held.
            std::thread::sleep(move_window());
            assert!(
                !sb.events().iter().any(|e| e.starts_with("term")),
                "{name}: {:?}",
                sb.events()
            );
            assert_eq!(sb.starts().len(), 1, "{name}");
        }
        sb.send("exit 0");
        assert_eq!(h.wait_exit(), Exit::Code(0), "{name}");
    }
}

/// THE HARNESS'S OWN UPDATE (aterm's harness design §4.5): with a newer build of the harness in
/// the store, at the same safe point a move waits for, the harness `execve`s it in its own process
/// (the same pid now runs the new build's image) and the child runs on untouched: never
/// signalled, never started again. The episode crossed: a vendor move afterwards resumes the same
/// session with the launch's resume argv, on a terminal put back to the mode the FIRST image found
/// (cooked), not the raw mode the child held when the new image started. NEGATIVE CONTROL: a
/// build that does not answer this protocol (`--alab-adopt-probe`) is never exec'd into.
fn moves_itself_onto_a_newer_build_of_itself_at_a_safe_point() {
    let sb = Sandbox::new("reexec");
    sb.point_current(&sb.install(OLD));
    let first = sb.install_harness(1, None);
    let launch: Vec<OsString> = ["--trust-workspace", "--model", "m1", "hello prompt"]
        .into_iter()
        .map(OsString::from)
        .collect();
    let mut h = sb.spawn_from(&first, &launch, &[]);
    let start = h.wait_start(&sb, 1);
    assert_eq!(field(&start, "icanon"), "1", "the terminal starts cooked");
    assert_eq!(exe_of(h.pid), Some(canonical(&first)));
    next_second();
    h.type_bytes(b"go\r");
    sb.wait_event(&h, "submit", |e| e == "submit go");
    let alien = sb.install_harness(2, Some("echo muse-alab-adopt 0"));
    h.wait_until(&sb, "the unproved build's hold", |sb| {
        sb.journal().contains("-> build 2 waits: build-unproved")
    });
    assert!(
        sb.journal().contains(&format!(
            "build 2: `{} --alab-adopt-probe` exited",
            alien.display()
        )),
        "{}",
        sb.journal()
    );
    assert_eq!(exe_of(h.pid), Some(canonical(&first)), "never exec'd into");
    let next = sb.install_harness(3, None);
    h.wait_until(&sb, "the new image", |_| {
        exe_of(h.pid) == Some(canonical(&next))
    });
    h.wait_until(&sb, "the adoption", |sb| sb.journal().contains(" ADOPTED "));
    let j = sb.journal();
    assert!(
        j.contains(&format!(
            "REEXEC muse-alab {VERSION} (build 1) -> build 3 ({})",
            next.display()
        )),
        "{j}"
    );
    assert!(
        !sb.events().iter().any(|e| e.starts_with("term")),
        "the child was never signalled: {:?}",
        sb.events()
    );
    assert_eq!(sb.starts().len(), 1, "the child ran on");
    sb.point_current(&sb.install(NEW));
    let second = h.wait_start(&sb, 2);
    assert_eq!(
        field(&second, "argv"),
        format!("[--trust-workspace][resume][{SESSION}]")
    );
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    assert_eq!(
        field(&second, "icanon"),
        "1",
        "the terminal's starting mode crossed the re-exec"
    );
    sb.send("exit 3");
    assert_eq!(h.wait_exit(), Exit::Code(3));
}

/// UNDER ATERM THE HARNESS'S OWN MOVE waits on aterm's word like a vendor move (here a served
/// `composer=draft` holds it), and the new image claims again under the same pid with the child
/// it took over: the old image's connections closed with the exec (they are close-on-exec), the
/// claim names the same holder and `child=`, and the next act waits on a fresh read under it.
fn under_aterm_its_own_move_waits_on_aterm_and_claims_again_under_the_same_pid() {
    let sb = Sandbox::new("aterm-reexec");
    sb.point_current(&sb.install(OLD));
    let aterm = fake_aterm("aterm-reexec");
    aterm.set_status("composer", Some("draft"));
    let first = sb.install_harness(1, None);
    let env = aterm.env();
    let mut h = sb.spawn_from(&first, &["--trust-workspace".into()], &pairs(&env));
    let start = h.wait_start(&sb, 1);
    let child = field(&start, "pid");
    h.wait_until(&sb, "the claim and its announcement", |_| {
        claims(&aterm).len() >= 2
    });
    next_second();
    h.type_bytes(b"go\r");
    sb.wait_event(&h, "submit", |e| e == "submit go");
    let next = sb.install_harness(2, None);
    h.wait_until(&sb, "aterm's draft holding it", |sb| {
        sb.journal().contains("-> build 2 waits: draft (")
    });
    assert_eq!(exe_of(h.pid), Some(canonical(&first)));
    aterm.set_status("composer", Some("empty"));
    h.wait_until(&sb, "the claim made again by the new image", |sb| {
        sb.journal().contains(" ADOPTED ") && notes(&sb.journal(), "CLAIMED") >= 2
    });
    assert_eq!(exe_of(h.pid), Some(canonical(&next)));
    let sent = claims(&aterm);
    assert!(
        sent.len() >= 3
            && sent[2].starts_with(&format!("{} vendor={OLD} child={child}:", claim_of(h.pid))),
        "claimed at the start, announced with the child, and claimed again after the exec: {sent:?}"
    );
    assert_eq!(aterm.holder(), Some(format!("muse-alab@{}", h.pid)));
    let before = aterm.verbs().len();
    sb.send(&format!("crash {}", libc::SIGKILL));
    let second = h.wait_start(&sb, 2);
    assert_eq!(field(&second, "ppid"), h.pid.to_string());
    let fresh: Vec<String> = aterm.verbs().split_off(before);
    assert!(
        fresh.iter().any(|v| v == "status"),
        "the relaunch waited on a fresh read under the new claim: {fresh:?}"
    );
    sb.send("exit 0");
    assert_eq!(h.wait_exit(), Exit::Code(0));
}

/// The executable process `pid` runs now.
fn exe_of(pid: libc::pid_t) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = vec![0u8; 4096];
        let size = u32::try_from(buf.len()).ok()?;
        // SAFETY: proc_pidpath writes at most `size` bytes into `buf`, which holds that many.
        let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), size) };
        let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
        buf.truncate(n);
        fs::canonicalize(PathBuf::from(OsString::from_vec(buf))).ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        fs::canonicalize(format!("/proc/{pid}/exe")).ok()
    }
}

fn canonical(p: &Path) -> PathBuf {
    fs::canonicalize(p).expect("canonical")
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

    /// Install the harness under test as its own program's build `build` (a copy: each build
    /// its own file, so the image a process runs names its build), or, with `script`, a shell
    /// script in its place, and point that program's `current` at it as atpkg does. Its binary.
    fn install_harness(&self, build: u64, script: Option<&str>) -> PathBuf {
        let program = self.prefix().join("store").join("muse-alab");
        let dir = program.join(build.to_string());
        fs::create_dir_all(dir.join("bin")).expect("build dir");
        let bin = dir.join("bin").join("muse-alab");
        match script {
            Some(body) => {
                fs::write(&bin, format!("#!/bin/sh\n{body}\n")).expect("script");
                fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).expect("mode");
            }
            None => {
                fs::copy(HARNESS, &bin).expect("install the harness");
            }
        }
        fs::write(dir.with_extension("ready"), "ok\n").expect("ready");
        let tmp = program.join(".current.tmp");
        let _ = fs::remove_file(&tmp);
        symlink(build.to_string(), &tmp).expect("link");
        fs::rename(&tmp, program.join("current")).expect("swap");
        bin
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
        self.command_for(Path::new(HARNESS))
    }

    fn command_for(&self, exe: &Path) -> Command {
        let mut cmd = Command::new(exe);
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

    /// [`Sandbox::spawn`] of the harness binary `exe` (one installed in the store).
    fn spawn_from(&self, exe: &Path, args: &[OsString], env: &[(&str, &str)]) -> Harness {
        self.spawn_cmd(self.command_for(exe), args, env, &[], None)
    }

    /// A non-interactive `/bin/sh -c script` as the session's leader and foreground job on a new
    /// pseudo-terminal, `$0` naming the harness under test: the harness is then one command of a
    /// shell sharing its terminal, which reads the terminal again once the harness has ended.
    fn spawn_under_shell(&self, script: &str, env: &[(&str, &str)]) -> Harness {
        self.spawn_cmd(
            self.command_for(Path::new("/bin/sh")),
            &["-c".into(), script.into(), HARNESS.into()],
            env,
            &[],
            None,
        )
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
        self.spawn_cmd(self.command(), args, env, ignore, stderr)
    }

    fn spawn_cmd(
        &self,
        mut cmd: Command,
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

/// The CPU time (user and system) the process `pid` has used so far: `/proc/<pid>/stat`'s
/// `utime` and `stime` on Linux.
#[cfg(target_os = "linux")]
fn cpu_time(pid: libc::pid_t) -> Duration {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("the process's stat");
    // Fields 14 and 15; the first field after the command's `)` is field 3.
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .expect("a command in parentheses")
        .1
        .split_whitespace()
        .collect();
    let ticks: u64 =
        fields[11].parse::<u64>().expect("utime") + fields[12].parse::<u64>().expect("stime");
    // SAFETY: sysconf takes a plain integer.
    let hz = u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).expect("clock ticks");
    Duration::from_millis(ticks * 1000 / hz)
}

/// The CPU time (user and system) the process `pid` has used so far: `ps`'s `time`,
/// `[[dd-]hh:]mm:ss.cc` on macOS.
#[cfg(not(target_os = "linux"))]
fn cpu_time(pid: libc::pid_t) -> Duration {
    let out = Command::new("/bin/ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout);
    let text = text.trim();
    let (days, clock) = text
        .split_once('-')
        .map_or((0.0, text), |(d, c)| (d.parse().expect("ps days"), c));
    let secs = clock.split(':').fold(0.0, |acc, part| {
        acc * 60.0
            + part
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("ps time {text:?}"))
    });
    Duration::from_secs_f64(days * 86_400.0 + secs)
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
        /// Stop (as `stop` does) as soon as a SIGTERM was ignored.
        stop_on_term: bool,
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
                    // Longer than any test runs: the tool ends only by `untool` or with the
                    // job, never by a clock on a slow machine.
                    let child = Command::new("/bin/sleep")
                        .arg("3600")
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
                    // `ignore-term stop`: and stop as soon as one was ignored.
                    self.ignore_term = true;
                    self.stop_on_term = arg == "stop";
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
            stop_on_term: false,
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
        let mut icanon = "-";
        if tty {
            // Raw input, as a TUI sets it: every key reaches a read, no echo; signals
            // from keys stay on.
            // SAFETY: tcgetattr/tcsetattr on fd 0 with a valid termios.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &raw mut t) == 0 {
                    // The mode it found: cooked (`1`) as a shell leaves the terminal, or raw.
                    icanon = if t.c_lflag & libc::ICANON == 0 {
                        "0"
                    } else {
                        "1"
                    };
                    t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::IEXTEN);
                    t.c_cc[libc::VMIN] = 1;
                    t.c_cc[libc::VTIME] = 0;
                    libc::tcsetattr(0, libc::TCSANOW, &raw const t);
                }
            }
        }
        let shown: String = args[1..].iter().map(|a| show(a)).collect();
        fake.event(&format!(
            "start pid={pid} ppid={ppid} tty={} ign={ignores} icanon={icanon} exe={} argv={shown}",
            u8::from(tty),
            Path::new(&args[0]).display()
        ));
        loop {
            if TERM.swap(false, Ordering::SeqCst) {
                if fake.ignore_term {
                    fake.event("term-ignored");
                    if fake.stop_on_term {
                        fake.command("stop");
                    }
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
