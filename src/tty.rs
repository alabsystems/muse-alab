// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE TERMINAL WITNESS: when the inherited terminal last delivered input, and whether
//! this job is in its foreground.
//!
//! The harness never reads the terminal (the child owns it). It reads the terminal
//! device's access time, which the kernel moves when a process reads the terminal.
//! Measured on a pty on macOS (Darwin 27, 2026-10-24), devfs keeps it like this:
//!
//! - output, `stat`, `select`/`poll` timeouts and the other side's writes never move it;
//! - a read whose wall-clock second differs from the stored time's sets it to the read's
//!   time, to the microsecond (a blocking read: when it is entered);
//! - a read in the SAME second as the stored time only marks the node, and the next
//!   `stat` (by anyone) stores `min(stat second, stored second + 1)` with the stat's
//!   fraction: later than the read, never earlier.
//!
//! So the stored time is never earlier than the last read: "no input since T" is
//! `atime <= T`, soundly, and "no person typing for G" is `now - atime >= G`. It can be
//! LATER than the last read (up to the next `stat`), which only ever holds the safe point
//! longer. Muse Code links crossterm (its binary's strings, 2026-10-24), whose Unix event
//! source reads the terminal only once `poll` says it is readable, so its reads are its
//! input. The terminal must be in raw mode for a keystroke to reach a read at all; a
//! TUI's is.
//!
//! Elsewhere the clock is not this: Linux stores a tty's times only to an 8-second bucket
//! (`tty_update_time`), so the stored time can be EARLIER than the last read. The harness
//! reads no input clock there ([`INPUT_CLOCK`]) and the safe point is never reached.

use std::fmt;
use std::io::IsTerminal as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// A terminal mode, saved to be put back.
#[derive(Clone, Copy)]
pub struct Mode(libc::termios);

impl fmt::Debug for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mode(..)")
    }
}

/// Whether this platform's terminal access time is never earlier than the last read
/// (measured on macOS only).
pub const INPUT_CLOCK: bool = cfg!(target_os = "macos");

/// The inherited terminal (one of fds 0, 1, 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tty {
    fd: libc::c_int,
}

impl Tty {
    /// The first of stdin, stdout and stderr that is a terminal.
    #[must_use]
    pub fn find() -> Option<Self> {
        [0, 1, 2]
            .into_iter()
            .find(|&fd| stdio_is_terminal(fd))
            .map(|fd| Self { fd })
    }

    /// No earlier than when the terminal last delivered input, microseconds since the
    /// epoch (`None` where the platform keeps no such clock, [`INPUT_CLOCK`]).
    #[must_use]
    pub fn last_input_us(self) -> Option<u64> {
        if !INPUT_CLOCK {
            return None;
        }
        // `fstat` through std: a duplicate of the descriptor names the same open terminal,
        // and neither the duplicate, the `fstat` nor the close moves its access time.
        let st = std::fs::File::from(dup_stdio(self.fd)?).metadata().ok()?;
        let secs = u64::try_from(st.atime()).ok()?;
        let nanos = u64::try_from(st.atime_nsec()).ok()?;
        secs.checked_mul(1_000_000)?.checked_add(nanos / 1000)
    }

    /// How many bytes of input wait in the terminal's input queue, typed but not yet read
    /// by anyone (`FIONREAD`; `None`: unreadable). Input a busy child has not read yet has
    /// not moved the input clock, so this is the person's typing the clock cannot see.
    #[must_use]
    pub fn pending_input(self) -> Option<usize> {
        let mut n: libc::c_int = 0;
        // SAFETY: FIONREAD writes one `int` through the pointer, which is valid for it.
        if unsafe { libc::ioctl(self.fd, libc::FIONREAD, &raw mut n) } != 0 {
            return None;
        }
        usize::try_from(n).ok()
    }

    /// The terminal's mode now.
    #[must_use]
    pub fn mode(self) -> Option<Mode> {
        let mut t = blank_termios();
        // SAFETY: `t` is a valid, writable `termios` for the call's duration.
        (unsafe { libc::tcgetattr(self.fd, &raw mut t) } == 0).then_some(Mode(t))
    }

    /// Set the terminal's mode (at once); whether it took.
    #[must_use]
    pub fn set_mode(self, mode: &Mode) -> bool {
        // SAFETY: `mode.0` is a `termios` read from a terminal by `tcgetattr`.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const mode.0) == 0 }
    }

    /// Whether this process's group is the terminal's foreground group.
    #[must_use]
    pub fn foreground(self) -> Option<bool> {
        // SAFETY: tcgetpgrp and getpgrp take plain integers and touch no memory of ours.
        let (fg, ours) = unsafe { (libc::tcgetpgrp(self.fd), libc::getpgrp()) };
        (fg > 0).then_some(fg == ours)
    }
}

/// Whether stdin, stdout or stderr (by number) is a terminal.
fn stdio_is_terminal(fd: libc::c_int) -> bool {
    match fd {
        0 => std::io::stdin().is_terminal(),
        1 => std::io::stdout().is_terminal(),
        2 => std::io::stderr().is_terminal(),
        _ => false,
    }
}

/// A duplicate of stdin, stdout or stderr (by number): the same open file.
fn dup_stdio(fd: libc::c_int) -> Option<OwnedFd> {
    match fd {
        0 => std::io::stdin().as_fd().try_clone_to_owned().ok(),
        1 => std::io::stdout().as_fd().try_clone_to_owned().ok(),
        2 => std::io::stderr().as_fd().try_clone_to_owned().ok(),
        _ => None,
    }
}

/// A blank `termios` for `tcgetattr` to fill, field by field (no `mem::zeroed`).
#[cfg(target_os = "macos")]
fn blank_termios() -> libc::termios {
    libc::termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_cc: [0; libc::NCCS],
        c_ispeed: 0,
        c_ospeed: 0,
    }
}

/// A blank `termios` for `tcgetattr` to fill.
#[cfg(not(target_os = "macos"))]
fn blank_termios() -> libc::termios {
    // SAFETY: an all-zero `termios` is a valid value of the plain C struct (its fields
    // differ between libc targets, so it is not spelled field by field here).
    unsafe { std::mem::zeroed() }
}

/// Now, microseconds since the epoch.
#[must_use]
pub fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}
