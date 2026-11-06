// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-table facts: a process's descendants (a tool the vendor runs is work a restart
//! would kill) and whether a process is stopped now.

/// How many processes the descendant walk visits at most; past it the answer is unknown.
const MAX_WALK: usize = 4096;

/// The most pids one `proc_listchildpids` call is given room for; a process with more
/// children is unknown.
#[cfg(target_os = "macos")]
const MAX_CAP: usize = MAX_WALK * 4;

/// How many descendants `pid` has (children, grandchildren, …), or `None` when the process
/// table cannot be read.
#[must_use]
pub fn descendants(pid: u32) -> Option<usize> {
    let mut seen = std::collections::BTreeSet::new();
    let mut todo = vec![pid];
    while let Some(p) = todo.pop() {
        for c in children(p)? {
            if seen.insert(c) {
                if seen.len() > MAX_WALK {
                    return None;
                }
                todo.push(c);
            }
        }
    }
    Some(seen.len())
}

/// The direct children of `pid`.
#[cfg(target_os = "macos")]
fn children(pid: u32) -> Option<Vec<u32>> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    for cap in [64, 256, 1024, 4096, MAX_CAP] {
        match listed(pid, cap)? {
            Listed::Children(kids) => return Some(kids),
            Listed::Full => {}
        }
    }
    None
}

/// What one `proc_listchildpids` call found.
#[cfg(target_os = "macos")]
enum Listed {
    /// Every child (fewer than the room).
    Children(Vec<u32>),
    /// As many children as the room held, or more.
    Full,
}

/// The direct children of `pid`, given room for `cap` of them (`None`: unreadable, or
/// `cap` is past [`MAX_CAP`]).
#[cfg(target_os = "macos")]
fn listed(pid: libc::pid_t, cap: usize) -> Option<Listed> {
    // The bound stands before the buffer is sized, so no allocation or byte count can
    // pass it.
    if cap > MAX_CAP {
        return None;
    }
    let mut buf: Vec<libc::pid_t> = vec![0; cap];
    let bytes = libc::c_int::try_from(cap * std::mem::size_of::<libc::pid_t>()).ok()?;
    // SAFETY: `buf` holds `bytes` writable bytes for the call; measured 2026-10-24,
    // proc_listchildpids returns the NUMBER of pids written (3 for three children).
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr().cast(), bytes) };
    let n = usize::try_from(n).ok()?;
    if n >= cap {
        return Some(Listed::Full);
    }
    buf.truncate(n);
    Some(Listed::Children(
        buf.into_iter()
            .filter_map(|p| u32::try_from(p).ok())
            .filter(|&p| p > 0)
            .collect(),
    ))
}

/// The direct children of `pid`, from `/proc/*/stat`.
#[cfg(not(target_os = "macos"))]
fn children(pid: u32) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(child) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid …`: comm may hold spaces and parentheses.
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|p| p.parse::<u32>().ok());
        if ppid == Some(pid) {
            out.push(child);
        }
    }
    Some(out)
}

/// Whether `pid` is stopped now (`None`: unknown).
#[cfg(target_os = "macos")]
#[must_use]
pub fn is_stopped(pid: u32) -> Option<bool> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // A blank `proc_bsdinfo`, field by field (no `mem::zeroed`): the call fills it.
    let mut info = libc::proc_bsdinfo {
        pbi_flags: 0,
        pbi_status: 0,
        pbi_xstatus: 0,
        pbi_pid: 0,
        pbi_ppid: 0,
        pbi_uid: 0,
        pbi_gid: 0,
        pbi_ruid: 0,
        pbi_rgid: 0,
        pbi_svuid: 0,
        pbi_svgid: 0,
        rfu_1: 0,
        pbi_comm: [0; libc::MAXCOMLEN],
        pbi_name: [0; 32],
        pbi_nfiles: 0,
        pbi_pgid: 0,
        pbi_pjobc: 0,
        e_tdev: 0,
        e_tpgid: 0,
        pbi_nice: 0,
        pbi_start_tvsec: 0,
        pbi_start_tvusec: 0,
    };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is a writable `proc_bsdinfo` of `size` bytes for the call.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (n == size).then_some(info.pbi_status == libc::SSTOP)
}

/// Whether `pid` is stopped now (`None`: unknown).
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn is_stopped(pid: u32) -> Option<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let state = stat.rsplit_once(')')?.1.split_whitespace().next()?;
    Some(state == "T" || state == "t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_walk_counts_children_and_grandchildren() {
        let mut kid = std::process::Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 5 & /bin/sleep 5; wait"])
            .spawn()
            .expect("spawn");
        let kid_pid = kid.id();
        let mut seen = None;
        for _ in 0..100 {
            seen = descendants(kid_pid);
            if seen == Some(2) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(seen, Some(2), "the two sleeps under sh");
        let mine = descendants(std::process::id()).expect("readable");
        assert!(
            mine >= 3,
            "sh and its sleeps are this process's descendants: {mine}"
        );
        assert_eq!(is_stopped(kid_pid), Some(false));
        let _ = kid.kill();
        let _ = kid.wait();
    }
}
