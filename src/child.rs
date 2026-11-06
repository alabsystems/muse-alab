// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor child: started in the harness's own process group on the inherited
//! terminal (no `setpgid`, no pty), with every argument passed through, and watched with
//! `waitpid` for its real status.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::Command;

/// One change in the child's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// It ended (see [`End`]).
    Ended(End),
    /// It stopped with this signal.
    Stopped(libc::c_int),
    /// It continued.
    Continued,
}

/// How the child ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// It exited with this code.
    Code(i32),
    /// A signal ended it.
    Signal(libc::c_int),
}

impl End {
    /// The status in the shell's form: the code, or `128 + signal`.
    #[must_use]
    pub fn shell_status(self) -> i64 {
        match self {
            End::Code(c) => i64::from(c),
            End::Signal(s) => 128 + i64::from(s),
        }
    }
}

/// Start `exe` with `args` (argv[0] is `exe`'s path, as atpkg's twin `exec`s it) and
/// `envs` added to the inherited environment (the harness's recursion marks).
///
/// # Errors
/// The spawn failed.
pub fn spawn(exe: &Path, args: &[OsString], envs: &[(&str, String)]) -> io::Result<u32> {
    let mut cmd = Command::new(exe);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn()?;
    // The harness reaps the child with waitpid itself; the handle is dropped unwaited.
    Ok(child.id())
}

/// The child's next state change, without blocking (`None`: nothing new).
///
/// # Errors
/// `waitpid` failed (the child is not ours).
pub fn poll(pid: u32) -> io::Result<Option<Status>> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waitpid on our own child with a valid status slot.
        let r = unsafe {
            libc::waitpid(
                pid,
                &raw mut status,
                libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
            )
        };
        if r == 0 {
            return Ok(None);
        }
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        break;
    }
    Ok(Some(if libc::WIFEXITED(status) {
        Status::Ended(End::Code(libc::WEXITSTATUS(status)))
    } else if libc::WIFSIGNALED(status) {
        Status::Ended(End::Signal(libc::WTERMSIG(status)))
    } else if libc::WIFSTOPPED(status) {
        Status::Stopped(libc::WSTOPSIG(status))
    } else {
        Status::Continued
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_is_the_shells() {
        assert_eq!(End::Code(7).shell_status(), 7);
        assert_eq!(End::Signal(libc::SIGKILL).shell_status(), 137);
        assert_eq!(End::Signal(libc::SIGINT).shell_status(), 130);
    }

    #[test]
    fn a_child_is_reaped_with_its_real_status() {
        let pid = spawn(
            Path::new("/bin/sh"),
            &[OsString::from("-c"), OsString::from("exit 7")],
            &[],
        )
        .expect("spawn");
        let end = loop {
            match poll(pid).expect("ours") {
                Some(Status::Ended(e)) => break e,
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        };
        assert_eq!(end, End::Code(7));
    }
}
