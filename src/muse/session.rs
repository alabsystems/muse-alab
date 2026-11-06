// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SESSION WITNESS: which Muse Code session the child holds, and its logs.
//!
//! Muse Code keeps each session under
//! `<data>/muse/sessions/YYYY/MM/DD/<uuid>/` with a `.session.lock` whose body names the
//! holding process (`pid=<pid>`) and a `session.jsonl` log (subagents under
//! `subagent/<id>/session.jsonl`). `<data>` is `$XDG_DATA_HOME`, else
//! `~/.local/share`. Ported from aterm `crates/aterm-agent/src/harness/upgrade_muse.rs`
//! (`data_home`, `sessions_root`, `session_of_lock`, `lock_pid`, `LOCK`, `LOG`),
//! `upgrade_muse_drive.rs` (`locks_naming`, `session_log_tail`) and
//! `upgrade_muse_live_drive.rs` (`log_look`), all at 5e2c6d0c28.

use std::ffi::OsStr;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::argv::is_session_uuid;
use super::log::{self, Open, Quiet};

/// Session lock file name.
pub const LOCK: &str = ".session.lock";

/// Session log file name.
pub const LOG: &str = "session.jsonl";

/// How much of a session log's end is read (aterm's `LOG_BYTES`): the measured log of a
/// session resumed twenty times was 1.2 MB, and the process's start must be in the window.
pub const LOG_BYTES: u64 = 32 << 20;

/// A second's slack on a file's clock against the log's own.
const CLOCK_SLACK_US: u64 = 1_000_000;

/// Muse Code's data directory: `XDG_DATA_HOME` when set (absolute; a relative one is no
/// answer), else `<home>/.local/share`.
#[must_use]
pub fn data_home(xdg: Option<&OsStr>, home: &Path) -> Option<PathBuf> {
    match xdg.filter(|v| !v.is_empty()) {
        Some(set) if Path::new(set).is_absolute() => Some(PathBuf::from(set)),
        Some(_) => None,
        None => Some(home.join(".local").join("share")),
    }
}

/// The sessions root under the data directory.
#[must_use]
pub fn sessions_root(data: &Path) -> PathBuf {
    data.join("muse").join("sessions")
}

/// The `pid=<pid>` a lock body names.
#[must_use]
pub fn lock_pid(body: &str) -> Option<u32> {
    body.lines()
        .find_map(|l| l.trim().strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
        .filter(|&p| p > 0)
}

/// The session a lock at `<root>/YYYY/MM/DD/<uuid>/.session.lock` names.
#[must_use]
pub fn session_of_lock(root: &Path, lock: &Path) -> Option<String> {
    let rest = lock.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<_>>()?;
    let [year, month, day, session, file] = parts.as_slice() else {
        return None;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    (digits(year, 4)
        && digits(month, 2)
        && digits(day, 2)
        && is_session_uuid(session)
        && *file == LOCK)
        .then(|| (*session).to_string())
}

fn read_small(path: &Path) -> Option<String> {
    const MAX: u64 = 64 * 1024;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX {
        return None;
    }
    let mut text = String::new();
    f.take(MAX.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (u64::try_from(text.len()).ok()? <= MAX).then_some(text)
}

/// Whether the lock at `lock` names `pid`.
#[must_use]
pub fn lock_names(lock: &Path, pid: u32) -> bool {
    read_small(lock).is_some_and(|b| lock_pid(&b) == Some(pid))
}

fn modified_ms(path: &Path) -> Option<u64> {
    let at = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    u64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The sessions whose lock under `root` names `pid`, written no earlier than a second
/// before `since_ms` (a lock left by an earlier process of the same pid is older).
#[must_use]
pub fn locks_naming(root: &Path, pid: u32, since_ms: u64) -> Vec<(String, PathBuf)> {
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    dirs(root)
        .iter()
        .flat_map(|y| dirs(y))
        .flat_map(|m| dirs(&m))
        .flat_map(|d| dirs(&d))
        .map(|s| s.join(LOCK))
        .filter(|lock| modified_ms(lock).is_some_and(|at| at.saturating_add(1000) >= since_ms))
        .filter_map(|lock| {
            let session = session_of_lock(root, &lock)?;
            lock_names(&lock, pid).then_some((session, lock))
        })
        .collect()
}

/// The one session `pid` holds under `root`: exactly one lock naming it, written since it
/// started; two or none are no witness.
#[must_use]
pub fn witness(root: &Path, pid: u32, since_ms: u64) -> Option<(String, PathBuf)> {
    match locks_naming(root, pid, since_ms).as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// At most `cap` bytes from the end of the session log beside `lock` (from the first
/// whole line when cut).
#[must_use]
pub fn session_log_tail(lock: &Path, cap: u64) -> Option<String> {
    let log = lock.parent()?.join(LOG);
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(log)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let from = meta.len().saturating_sub(cap);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    f.take(cap).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(if from > 0 {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text
    })
}

/// Whether the process `pid` (started at `started_ms`) left a clean `session.end` in its
/// part of the session log beside `lock` ([`log::ended_clean`]).
#[must_use]
pub fn ended_clean(lock: &Path, pid: u32, started_ms: u64) -> bool {
    let Some(text) = session_log_tail(lock, LOG_BYTES) else {
        return false;
    };
    let since_us = started_ms.saturating_sub(1000).saturating_mul(1000);
    log::part_start(&text, pid, since_us).is_some_and(|(at, _)| log::ended_clean(&text[at..]))
}

/// G-M1 AND G-M2 OVER THE SESSION'S LOGS: its own `session.jsonl` (this process's part)
/// and every `subagent/<id>/session.jsonl` beside it from that part's start on (aterm
/// `log_look`). `started_ms` is when the process started.
///
/// # Errors
/// What holds the restart off ([`Open`]).
pub fn log_look(lock: &Path, pid: u32, started_ms: u64) -> Result<Quiet, Open> {
    if started_ms == 0 {
        return Err(Open::Unproved);
    }
    let dir = lock.parent().ok_or(Open::Unreadable)?;
    let text = session_log_tail(lock, LOG_BYTES).ok_or(Open::Unreadable)?;
    let since_us = started_ms.saturating_sub(1000).saturating_mul(1000);
    let quiet = log::quiet(&text, pid, since_us)?;
    let subagents = match std::fs::read_dir(dir.join("subagent")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(quiet),
        Err(_) => return Err(Open::Unreadable),
    };
    let modified_us = |meta: &std::fs::Metadata| {
        meta.modified()
            .ok()
            .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| u64::try_from(d.as_micros()).ok())
    };
    for entry in subagents {
        let path = entry.map_err(|_| Open::Unreadable)?.path();
        let older = |meta: &std::fs::Metadata| {
            modified_us(meta).is_some_and(|at| at.saturating_add(CLOCK_SLACK_US) < quiet.from_us)
        };
        let sub_log = path.join(LOG);
        match std::fs::symlink_metadata(&sub_log) {
            Ok(meta) if older(&meta) => continue,
            Ok(meta) if meta.is_file() && meta.len() <= LOG_BYTES => {}
            Ok(_) => return Err(Open::Unreadable),
            Err(_) => match std::fs::symlink_metadata(&path) {
                Ok(meta) if older(&meta) => continue,
                _ => return Err(Open::Unreadable),
            },
        }
        let mut text = String::new();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&sub_log)
            .map_err(|_| Open::Unreadable)?;
        if !f
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() <= LOG_BYTES)
        {
            return Err(Open::Unreadable);
        }
        f.take(LOG_BYTES.saturating_add(1))
            .read_to_string(&mut text)
            .map_err(|_| Open::Unreadable)?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > LOG_BYTES {
            return Err(Open::Unreadable);
        }
        log::open_work(&text, quiet.from_us)?;
    }
    Ok(quiet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_names_its_session_only_at_its_own_depth() {
        let root = Path::new("/d/muse/sessions");
        let s = "01a148f8-9301-7752-96db-fef456f88074";
        assert_eq!(
            session_of_lock(root, &root.join(format!("2026/10/24/{s}/.session.lock"))),
            Some(s.to_string())
        );
        for bad in [
            format!("2026/10/{s}/.session.lock"),
            format!("2026/10/24/x/{s}/.session.lock"),
            format!("26/10/24/{s}/.session.lock"),
            format!("2026/10/24/{s}/session.jsonl"),
            "2026/10/24/not-a-uuid/.session.lock".to_string(),
        ] {
            assert_eq!(session_of_lock(root, &root.join(&bad)), None, "{bad}");
        }
    }

    #[test]
    fn the_lock_body_names_a_positive_pid() {
        assert_eq!(lock_pid("pid=4242\nstarted=1\n"), Some(4242));
        assert_eq!(lock_pid("  pid=7  "), Some(7));
        assert_eq!(lock_pid("pid=0"), None);
        assert_eq!(lock_pid("pid=x"), None);
        assert_eq!(lock_pid(""), None);
    }

    #[test]
    fn the_data_home_follows_xdg_or_home() {
        let home = Path::new("/h");
        assert_eq!(
            data_home(None, home),
            Some(PathBuf::from("/h/.local/share"))
        );
        assert_eq!(
            data_home(Some(OsStr::new("")), home),
            Some(PathBuf::from("/h/.local/share"))
        );
        assert_eq!(
            data_home(Some(OsStr::new("/x")), home),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(data_home(Some(OsStr::new("rel")), home), None);
    }
}
