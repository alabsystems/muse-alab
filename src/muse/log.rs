// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE TURN-ENDED WITNESS: Muse Code's own session log (`session.jsonl`) says whether the
//! running process has work open.
//!
//! Ported from aterm `crates/aterm-agent/src/harness/upgrade_muse_live.rs` (5e2c6d0c28),
//! the pure half of aterm's Muse Code live upgrade (its gates G-M1 and G-M2): `Open`,
//! `part_start`, `open_work` and `quiet_log`, with their measured record shapes. Added
//! here: [`run_times`], the start and end of the part's last turn, which the composer
//! witness and the settle read ([`crate::gate`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::json::{self, Value};

/// What in a session's log holds a restart off, or why the log proves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// No `runtime.session.route_facts` record names the process (written since it
    /// started) in what was read.
    Unproved,
    /// A line does not parse (a torn tail, a log written over), or the log could not be
    /// read at all.
    Unreadable,
    /// A run started with no terminal.
    Turn,
    /// A task accepted or started with no end; `background` where its last word says it
    /// runs in the background.
    Task {
        /// Its last word names background work.
        background: bool,
    },
    /// An inbox item queued with no later drain.
    Queued,
}

impl Open {
    /// The gate's wait word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Open::Unproved => "log-unproved",
            Open::Unreadable => "log-unreadable",
            Open::Turn => "turn-open",
            Open::Task { background: true } => "background-work",
            Open::Task { background: false } => "open-task",
            Open::Queued => "queued-work",
        }
    }
}

/// A record's `payload_type`, its `payload`, and its `recorded_at` (microseconds).
fn record(v: &Value) -> Option<(&str, &Value, Option<u64>)> {
    Some((
        v.get("payload_type")?.as_str()?,
        v.get("payload")?,
        v.get("recorded_at").and_then(Value::as_u64),
    ))
}

/// The pid a `runtime.session.route_facts` record names.
fn route_pid(payload: &Value) -> Option<u64> {
    payload.get("record")?.get("pid")?.as_u64()
}

/// THIS PROCESS'S PART OF THE SESSION'S LOG: the byte offset just past the last
/// `runtime.session.route_facts` record naming `pid` written at or after `since_us`, and
/// that record's time.
#[must_use]
pub fn part_start(log: &str, pid: u32, since_us: u64) -> Option<(usize, u64)> {
    let mut at = 0;
    let mut found = None;
    for line in log.split_inclusive('\n') {
        let next = at + line.len();
        if line.contains("route_facts")
            && let Ok(v) = json::parse(line.trim())
            && let Some(("runtime.session.route_facts", payload, recorded)) = record(&v)
            && route_pid(payload) == Some(u64::from(pid))
            && let Some(recorded) = recorded.filter(|r| *r >= since_us)
        {
            found = Some((next, recorded));
        }
        at = next;
    }
    found
}

/// A task event's id and event, of either shape Muse Code writes: `runtime.session` with
/// payload `kind: task`, or `runtime.session.task`.
fn task_event<'a>(payload_type: &str, payload: &'a Value) -> Option<(&'a str, &'a Value)> {
    let task = match payload_type {
        "runtime.session" => payload.get("kind")?.as_str()? == "task",
        "runtime.session.task" => true,
        _ => false,
    };
    if !task {
        return None;
    }
    let event = payload.get("event")?;
    let id = payload
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| event.get("task_id").and_then(Value::as_str))?;
    Some((id, event))
}

/// A run event of `runtime.session` (payload `kind: run`).
fn run_event<'a>(payload_type: &str, payload: &'a Value) -> Option<(&'a str, &'a Value)> {
    if payload_type != "runtime.session" || payload.get("kind")?.as_str()? != "run" {
        return None;
    }
    Some((payload.get("run_id")?.as_str()?, payload.get("event")?))
}

/// The task events that end a task.
const TASK_ENDS: [&str; 4] = ["completed", "cancelled", "failed", "rejected"];

/// The task kinds that carry no work a restart could lose (every other kind holds).
const NO_WORK_TASKS: [&str; 1] = ["session_name.allocate"];

fn says_background(event: &Value) -> bool {
    match event.get("kind").and_then(Value::as_str) {
        Some("status") => {
            event.get("message").and_then(Value::as_str) == Some("terminal process running")
        }
        Some("output") => event
            .get("chunk")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("background_running")),
        _ => false,
    }
}

fn inbox_key(source: Option<&Value>) -> (String, String) {
    let field = |k: &str| {
        source
            .and_then(|s| s.get(k))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (field("source"), field("task_id"))
}

/// NO RUNNING WORK IN `log` FROM `from_us` ON (G-M1 and G-M2 over one log).
///
/// # Errors
/// The first kind of open work found, or [`Open::Unreadable`] for a line that is not JSON.
pub fn open_work(log: &str, from_us: u64) -> Result<(), Open> {
    let mut runs: BTreeMap<String, bool> = BTreeMap::new();
    let mut tasks: BTreeMap<String, bool> = BTreeMap::new();
    let mut no_work: BTreeSet<String> = BTreeSet::new();
    // Inbox items queued and not yet drained: how many of each (source, task).
    let mut queued: BTreeMap<(String, String), usize> = BTreeMap::new();
    for line in log.lines().filter(|l| !l.trim().is_empty()) {
        let v = json::parse(line.trim()).map_err(|_| Open::Unreadable)?;
        let Some((kind, payload, recorded)) = record(&v) else {
            continue;
        };
        if recorded.is_some_and(|r| r < from_us) {
            continue;
        }
        if let Some((id, event)) = task_event(kind, payload) {
            match event.get("kind").and_then(Value::as_str) {
                Some("proposed") => {
                    if event
                        .get("task_kind")
                        .and_then(Value::as_str)
                        .is_some_and(|k| NO_WORK_TASKS.contains(&k))
                    {
                        no_work.insert(id.to_owned());
                    }
                }
                Some("accepted" | "started") if !no_work.contains(id) => {
                    tasks.entry(id.to_owned()).or_insert(false);
                }
                Some(end) if TASK_ENDS.contains(&end) => {
                    tasks.remove(id);
                }
                _ => {
                    if let Some(background) = tasks.get_mut(id) {
                        *background = says_background(event);
                    }
                }
            }
            continue;
        }
        if let Some((run, event)) = run_event(kind, payload) {
            match event.get("kind").and_then(Value::as_str) {
                Some("started") => {
                    runs.insert(run.to_owned(), true);
                }
                Some("terminal") => {
                    runs.insert(run.to_owned(), false);
                }
                Some("inbox_item_queued") => {
                    let count = queued.entry(inbox_key(event.get("source"))).or_insert(0);
                    // Each queued item is a line of `log`, so a count never nears the limit.
                    *count = count.saturating_add(1);
                }
                Some("inbox_item_drained") => {
                    let key =
                        inbox_key(event.get("delivery_snapshot").and_then(|d| d.get("source")));
                    match queued.get_mut(&key) {
                        Some(count) if *count > 1 => *count -= 1,
                        Some(_) => {
                            queued.remove(&key);
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
    }
    if runs.values().any(|open| *open) {
        return Err(Open::Turn);
    }
    if let Some(background) = tasks.values().copied().reduce(|a, b| a || b) {
        return Err(Open::Task { background });
    }
    if !queued.is_empty() {
        return Err(Open::Queued);
    }
    Ok(())
}

/// G-M1 AND G-M2 OVER THE SESSION'S OWN LOG: this process's part ([`part_start`]) holds no
/// open work. Returns the part's start time.
///
/// # Errors
/// [`Open::Unproved`] when no part names the process, else what [`open_work`] finds.
pub fn quiet_log(log: &str, pid: u32, since_us: u64) -> Result<u64, Open> {
    let (at, from_us) = part_start(log, pid, since_us).ok_or(Open::Unproved)?;
    open_work(&log[at..], 0)?;
    Ok(from_us)
}

/// When the part's last turn started and ended: the latest `recorded_at` of a run
/// `started` and of a run `terminal` event, read over `part` (lines that do not parse are
/// skipped here; [`open_work`] has already refused them).
#[must_use]
pub fn run_times(part: &str) -> (Option<u64>, Option<u64>) {
    let (mut started, mut ended) = (None, None);
    for line in part.lines().filter(|l| l.contains("\"run\"")) {
        let Ok(v) = json::parse(line.trim()) else {
            continue;
        };
        let Some((kind, payload, Some(at))) = record(&v) else {
            continue;
        };
        if let Some((_, event)) = run_event(kind, payload) {
            match event.get("kind").and_then(Value::as_str) {
                Some("started") => started = started.max(Some(at)),
                Some("terminal") => ended = ended.max(Some(at)),
                _ => {}
            }
        }
    }
    (started, ended)
}

/// WHETHER THE PART ENDS CLEAN: its last `session.end` record says `exit_reason: clean`
/// (aterm `upgrade_muse.rs` `log_verdict`, which aterm reads when it has no exit status;
/// here it only ever vetoes a crash relaunch, never causes one).
#[must_use]
pub fn ended_clean(part: &str) -> bool {
    let mut clean = false;
    for line in part.lines().filter(|l| l.contains("session.end")) {
        let Ok(v) = json::parse(line.trim()) else {
            continue;
        };
        if let Some(("session.end", payload, _)) = record(&v) {
            clean = payload
                .get("record")
                .and_then(|r| r.get("exit_reason"))
                .and_then(Value::as_str)
                == Some("clean");
        }
    }
    clean
}

/// What the session log says about the running process, when its part is quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiet {
    /// When the part began (its route record).
    pub from_us: u64,
    /// The last moment the composer is known empty: the part's last turn's start (its
    /// prompt was submitted), else the part's start.
    pub submitted_us: u64,
    /// When the last turn ended (else the part's start).
    pub ended_us: u64,
}

/// [`quiet_log`] with the times the gate reads.
///
/// # Errors
/// As [`quiet_log`].
pub fn quiet(log: &str, pid: u32, since_us: u64) -> Result<Quiet, Open> {
    let (at, from_us) = part_start(log, pid, since_us).ok_or(Open::Unproved)?;
    let part = &log[at..];
    open_work(part, 0)?;
    let (started, ended) = run_times(part);
    Ok(Quiet {
        from_us,
        submitted_us: started.unwrap_or(from_us).max(from_us),
        ended_us: ended.unwrap_or(from_us).max(from_us),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // aterm `upgrade_muse_live_tests.rs`, ported (record shapes measured from Muse Code).
    const PID: u32 = 57_495;
    const OTHER_PID: u32 = 72_752;
    const START_US: u64 = 1_792_284_625_000_000;

    fn rec(payload_type: &str, payload: &str, at_us: u64) -> String {
        format!(
            r#"{{"schema_version":1,"stream":{{"kind":"session","id":"s"}},"sequence":1,"recorded_at":{at_us},"record_type":"event","durability":"durable","payload_type":"{payload_type}","payload_schema_version":1,"payload":{payload}}}"#
        )
    }

    fn route(pid: u32, at_us: u64) -> String {
        rec(
            "runtime.session.route_facts",
            &format!(
                r#"{{"kind":"route_facts","record":{{"cwd":"/w","pid":{pid},"terminal_title_identity":"t"}}}}"#
            ),
            at_us,
        )
    }

    fn run(id: &str, event: &str, at_us: u64) -> String {
        rec(
            "runtime.session",
            &format!(r#"{{"kind":"run","run_id":"{id}","event":{event}}}"#),
            at_us,
        )
    }

    fn task(id: &str, event: &str, at_us: u64) -> String {
        rec(
            "runtime.session",
            &format!(r#"{{"kind":"task","run_id":"r","task_id":"{id}","event":{event}}}"#),
            at_us,
        )
    }

    fn turn(id: &str, at_us: u64) -> Vec<String> {
        vec![
            run(id, r#"{"kind":"started","prompt":"hi"}"#, at_us),
            run(
                id,
                r#"{"kind":"terminal","terminal":"completed","reason":null}"#,
                at_us + 10,
            ),
        ]
    }

    fn backgrounded(id: &str, at_us: u64) -> Vec<String> {
        vec![
            task(id, r#"{"kind":"proposed","task_kind":"tool.bash"}"#, at_us),
            task(id, r#"{"kind":"accepted"}"#, at_us + 1),
            task(id, r#"{"kind":"started","span_id":"x"}"#, at_us + 2),
            task(
                id,
                r#"{"kind":"status","message":"terminal process running","details":{"facets":[{"kind":"live_process","system":"unified_exec"}]}}"#,
                at_us + 3,
            ),
        ]
    }

    fn bg_end(id: &str, run_id: &str, at_us: u64) -> Vec<String> {
        vec![
            run(
                run_id,
                &format!(
                    r#"{{"kind":"inbox_item_queued","source":{{"source":"background_task_terminal","task_id":"{id}","terminal_kind":"completed"}},"summary":"background completed","disposition":"queue"}}"#
                ),
                at_us,
            ),
            task(id, r#"{"kind":"completed"}"#, at_us + 2),
        ]
    }

    fn drain(id: &str, run_id: &str, at_us: u64) -> Vec<String> {
        vec![
            run(run_id, r#"{"kind":"started","prompt":""}"#, at_us),
            run(
                run_id,
                &format!(
                    r#"{{"kind":"inbox_item_drained","item_id":"i","delivery_snapshot":{{"source":{{"source":"background_task_terminal","task_id":"{id}","terminal_kind":"completed"}}}}}}"#
                ),
                at_us + 1,
            ),
        ]
    }

    fn log(lines: &[Vec<String>]) -> String {
        let mut out = String::new();
        for l in lines.iter().flatten() {
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_part_of_a_process_starts_at_its_own_route_record() {
        let text = log(&[
            vec![route(OTHER_PID, START_US - 500)],
            turn("r0", START_US - 400),
            vec![route(PID, START_US + 1)],
            turn("r1", START_US + 2),
        ]);
        let (at, from) = part_start(&text, PID, START_US).expect("its part");
        assert_eq!(from, START_US + 1);
        assert!(text[at..].contains("r1") && !text[at..].contains("r0"));
        assert_eq!(part_start(&text, 4_242, START_US), None, "no route of it");
        let stale = log(&[vec![route(PID, START_US - 9_000_000)], turn("r", START_US)]);
        assert_eq!(
            part_start(&stale, PID, START_US),
            None,
            "an earlier process's"
        );
        assert_eq!(quiet_log(&stale, PID, START_US), Err(Open::Unproved));
    }

    #[test]
    fn an_open_run_holds_and_an_ended_one_does_not() {
        let open = log(&[
            vec![route(PID, START_US)],
            turn("r1", START_US + 1),
            vec![run(
                "r2",
                r#"{"kind":"started","prompt":"go"}"#,
                START_US + 50,
            )],
        ]);
        assert_eq!(quiet_log(&open, PID, START_US), Err(Open::Turn));
        let cancelled = log(&[
            vec![route(PID, START_US)],
            vec![
                run("r2", r#"{"kind":"started","prompt":"go"}"#, START_US + 50),
                run(
                    "r2",
                    r#"{"kind":"terminal","terminal":"cancelled","reason":"cancelled during model step"}"#,
                    START_US + 60,
                ),
            ],
        ]);
        assert_eq!(quiet_log(&cancelled, PID, START_US), Ok(START_US));
    }

    #[test]
    fn background_work_and_its_queued_end_hold_until_drained() {
        let base = || vec![vec![route(PID, START_US)], turn("r1", START_US + 1)];
        let mut running = base();
        running.push(backgrounded("t1", START_US + 100));
        assert_eq!(
            quiet_log(&log(&running), PID, START_US),
            Err(Open::Task { background: true })
        );
        let mut queued = running.clone();
        queued.push(bg_end("t1", "r1", START_US + 200));
        assert_eq!(quiet_log(&log(&queued), PID, START_US), Err(Open::Queued));
        let mut draining = queued.clone();
        draining.push(drain("t1", "r3", START_US + 300));
        assert_eq!(quiet_log(&log(&draining), PID, START_US), Err(Open::Turn));
        let mut done = draining.clone();
        done.push(vec![run(
            "r3",
            r#"{"kind":"terminal","terminal":"completed","reason":null}"#,
            START_US + 400,
        )]);
        assert_eq!(quiet_log(&log(&done), PID, START_US), Ok(START_US));
        let plain = log(&[
            vec![route(PID, START_US)],
            vec![task("t3", r#"{"kind":"accepted"}"#, START_US + 1)],
        ]);
        assert_eq!(
            quiet_log(&plain, PID, START_US),
            Err(Open::Task { background: false })
        );
        let naming = log(&[
            vec![route(PID, START_US)],
            vec![
                task(
                    "n1",
                    r#"{"kind":"proposed","task_kind":"session_name.allocate"}"#,
                    START_US + 1,
                ),
                task("n1", r#"{"kind":"started"}"#, START_US + 2),
            ],
        ]);
        assert_eq!(quiet_log(&naming, PID, START_US), Ok(START_US), "no work");
    }

    #[test]
    fn a_torn_log_fails_closed_and_a_frame_is_no_record() {
        let mut text = log(&[vec![route(PID, START_US)], turn("r1", START_US + 1)]);
        text.push_str(r#"{"schema_version":1,"payload_type":"runtime.sess"#);
        assert_eq!(quiet_log(&text, PID, START_US), Err(Open::Unreadable));
        let framed = format!(
            "{}{}\n",
            log(&[vec![route(PID, START_US)]]),
            r#"{"retained_marker":"omitted_live_only","schema_version":1,"omitted_record":{"record_type":"status"}}"#
        );
        assert_eq!(quiet_log(&framed, PID, START_US), Ok(START_US));
    }

    fn end(reason: &str, at_us: u64) -> String {
        rec(
            "session.end",
            &format!(
                r#"{{"kind":"session_end","record":{{"exit_reason":"{reason}","uptime_ms":5}}}}"#
            ),
            at_us,
        )
    }

    #[test]
    fn only_a_clean_end_record_says_clean() {
        let clean = log(&[
            vec![route(PID, START_US)],
            turn("r1", START_US + 1),
            vec![end("clean", START_US + 50)],
        ]);
        assert!(ended_clean(&clean));
        let crashed = log(&[
            vec![route(PID, START_US)],
            vec![end("panic", START_US + 50)],
        ]);
        assert!(!ended_clean(&crashed));
        let none = log(&[vec![route(PID, START_US)], turn("r1", START_US + 1)]);
        assert!(!ended_clean(&none), "SIGKILL: no end record");
        let later_crash = format!("{clean}{}\n", end("panic", START_US + 90));
        assert!(!ended_clean(&later_crash), "the last end record speaks");
    }

    #[test]
    fn the_quiet_times_are_the_last_turns() {
        let text = log(&[
            vec![route(PID, START_US)],
            turn("r1", START_US + 100),
            turn("r2", START_US + 500),
        ]);
        assert_eq!(
            quiet(&text, PID, START_US),
            Ok(Quiet {
                from_us: START_US,
                submitted_us: START_US + 500,
                ended_us: START_US + 510,
            })
        );
        let fresh = log(&[vec![route(PID, START_US)]]);
        assert_eq!(
            quiet(&fresh, PID, START_US),
            Ok(Quiet {
                from_us: START_US,
                submitted_us: START_US,
                ended_us: START_US,
            }),
            "no turn yet: the part's start"
        );
    }
}
