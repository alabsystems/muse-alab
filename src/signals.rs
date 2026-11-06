// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SIGNAL POLICY of a harness that runs its vendor as a child in its own process group
//! on the inherited terminal (the harness leads the foreground group; the child shares it):
//!
//! - `SIGINT` and `SIGQUIT` belong to the child: the terminal delivers them to the whole
//!   foreground group, so the child already has them. The harness catches them and does
//!   nothing (a caught signal, unlike an ignored one, reverts to default in the child at
//!   `exec`, so the child keeps its own disposition).
//! - `SIGTSTP` keeps its default: the harness stops with its group, and when the child
//!   stops on its own the harness follows it ([`stop_self`]), so the shell sees the job
//!   stopped.
//! - `SIGHUP` and `SIGTERM` are passed on to the child, and mean the session is ending: no
//!   relaunch follows.
//! - `SIGCHLD` and `SIGCONT` wake the loop.
//! - A signal the harness's caller left IGNORED stays ignored: `nohup` ignores `SIGHUP`,
//!   and a shell without job control ignores `SIGINT` and `SIGQUIT` for a `&` job. The
//!   harness does not catch such a signal among `SIGINT`, `SIGQUIT`, `SIGHUP` and
//!   `SIGTERM`, so the child inherits the ignore across `exec` exactly as it would without
//!   the harness (catching it would reset it to the default action in the child).
//!
//! How the loop hears a caught signal:
//!
//! - On macOS the handler does nothing at all; a kqueue `EVFILT_SIGNAL` filter records
//!   every signal posted to the process from its registration on, and the loop sleeps in
//!   `kevent` until one is (or its timeout passes). A handler that makes no call cannot
//!   disturb `errno` or anything else of the code it interrupts.
//! - Elsewhere handlers set a bit and write one byte to a close-on-exec self-pipe, saving
//!   and restoring `errno` around the write.

use std::io;
use std::time::Duration;

/// The signals the harness catches.
const CAUGHT: [libc::c_int; 6] = [
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGHUP,
    libc::SIGTERM,
    libc::SIGCHLD,
    libc::SIGCONT,
];

/// What arrived since the last wait.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// `SIGHUP`.
    pub hup: bool,
    /// `SIGTERM`.
    pub term: bool,
    /// `SIGCHLD`.
    pub chld: bool,
    /// `SIGCONT` (this process was continued).
    pub cont: bool,
}

impl Pending {
    /// Note that `sig` arrived (one the loop does not act on changes nothing).
    fn note(&mut self, sig: libc::c_int) {
        match sig {
            libc::SIGHUP => self.hup = true,
            libc::SIGTERM => self.term = true,
            libc::SIGCHLD => self.chld = true,
            libc::SIGCONT => self.cont = true,
            _ => {}
        }
    }
}

/// The installed policy's wake source (one per process).
#[derive(Debug)]
pub struct Signals {
    wake: wake::Wake,
}

#[cfg(target_os = "macos")]
mod wake {
    //! The kqueue wake: `EVFILT_SIGNAL` records each signal posted to the process, even
    //! while the loop is not waiting, so none is lost between two waits.

    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::time::Duration;

    use super::Pending;

    /// The caught signals' handler: nothing. The kqueue records the signal; the handler
    /// exists so the signal is caught (its default action does not run, and the child gets
    /// the default back at `exec`).
    pub extern "C" fn on_signal(_sig: libc::c_int) {}

    /// The kqueue (a kqueue is never inherited by a child).
    #[derive(Debug)]
    pub struct Wake {
        kq: OwnedFd,
    }

    /// An empty event, for `kevent` to fill.
    const BLANK: libc::kevent = libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };

    /// More room than events: each caught signal has one filter, which reports at most one
    /// event per call.
    const ROOM: usize = 8;

    /// [`ROOM`] as `kevent` takes it.
    const ROOM_C: libc::c_int = 8;

    impl Wake {
        /// A new kqueue with no filter.
        pub fn new() -> io::Result<Self> {
            // SAFETY: kqueue(2) takes no arguments.
            let kq = unsafe { libc::kqueue() };
            if kq < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: kqueue(2) just returned this descriptor, owned by nothing else.
            let kq = unsafe { OwnedFd::from_raw_fd(kq) };
            Ok(Self { kq })
        }

        /// Record `sig` from now on. Called before its handler is installed, so no posting
        /// the handler sees is missed.
        pub fn watch(&self, sig: libc::c_int) -> io::Result<()> {
            let change = libc::kevent {
                ident: usize::try_from(sig)
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
                filter: libc::EVFILT_SIGNAL,
                flags: libc::EV_ADD,
                ..BLANK
            };
            let (no_events, no_timeout) = (std::ptr::null_mut(), std::ptr::null());
            // SAFETY: one valid change and no event list; a null timeout is not read when no
            // events are requested.
            let r =
                unsafe { libc::kevent(self.kq.as_raw_fd(), &change, 1, no_events, 0, no_timeout) };
            if r < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Wait until a recorded signal was posted or `timeout` passes; what was.
        pub fn wait(&self, timeout: Duration) -> Pending {
            let mut pending = Pending::default();
            let blocking = libc::timespec {
                tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
                tv_nsec: libc::c_long::from(timeout.subsec_nanos()),
            };
            let at_once = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // A handler interrupts the blocking wait (`EINTR`) when its signal arrives during
            // it; the signal's event is then collected at once, without waiting again.
            for limit in [blocking, at_once] {
                let mut events = [BLANK; ROOM];
                // SAFETY: no changes; `events` has room for `ROOM` events; `limit` is a valid
                // timespec for the call's duration.
                let n = unsafe {
                    libc::kevent(
                        self.kq.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        events.as_mut_ptr(),
                        ROOM_C,
                        &limit,
                    )
                };
                let Ok(n) = usize::try_from(n) else {
                    continue;
                };
                for event in events.iter().take(n) {
                    let ident = event.ident;
                    if let Ok(sig) = libc::c_int::try_from(ident) {
                        pending.note(sig);
                    }
                }
                break;
            }
            pending
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod wake {
    //! The self-pipe wake: handlers set a bit and write one byte to a close-on-exec,
    //! non-blocking pipe, which the loop polls.

    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
    use std::time::Duration;

    use super::Pending;

    static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
    static PENDING: AtomicU32 = AtomicU32::new(0);

    fn errno_slot() -> *mut libc::c_int {
        // SAFETY: __errno_location returns this thread's errno slot.
        unsafe { libc::__errno_location() }
    }

    fn bit(sig: libc::c_int) -> u32 {
        match sig {
            libc::SIGHUP => 1,
            libc::SIGTERM => 2,
            libc::SIGCHLD => 4,
            libc::SIGCONT => 8,
            _ => 0,
        }
    }

    /// The caught signals' handler: a bit and one byte to the self-pipe.
    pub extern "C" fn on_signal(sig: libc::c_int) {
        // SAFETY: reading and restoring this thread's errno is async-signal-safe.
        let saved = unsafe { *errno_slot() };
        PENDING.fetch_or(bit(sig), Ordering::SeqCst);
        let fd = WAKE_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = 1u8;
            // SAFETY: write(2) is async-signal-safe; the pipe is non-blocking, so a full pipe
            // (the loop already has a wake pending) drops the byte.
            unsafe {
                libc::write(fd, (&raw const byte).cast(), 1);
            }
        }
        // SAFETY: as above.
        unsafe {
            *errno_slot() = saved;
        }
    }

    /// The self-pipe.
    #[derive(Debug)]
    pub struct Wake {
        read: OwnedFd,
        _write: OwnedFd,
    }

    fn cloexec_nonblocking(fd: libc::c_int) -> io::Result<()> {
        // SAFETY: fcntl on a descriptor this module owns.
        unsafe {
            let fd_flags = libc::fcntl(fd, libc::F_GETFD);
            let fl_flags = libc::fcntl(fd, libc::F_GETFL);
            if fd_flags < 0
                || fl_flags < 0
                || libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) < 0
                || libc::fcntl(fd, libc::F_SETFL, fl_flags | libc::O_NONBLOCK) < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    impl Wake {
        /// A new self-pipe, named to the handler.
        pub fn new() -> io::Result<Self> {
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: pipe(2) just returned these two descriptors, owned by nothing else.
            let (read, write) =
                unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            cloexec_nonblocking(read.as_raw_fd())?;
            cloexec_nonblocking(write.as_raw_fd())?;
            WAKE_FD.store(write.as_raw_fd(), Ordering::SeqCst);
            Ok(Self {
                read,
                _write: write,
            })
        }

        /// Every caught signal writes to the pipe; nothing to register.
        pub fn watch(&self, _sig: libc::c_int) -> io::Result<()> {
            Ok(())
        }

        /// Wait until a signal arrives or `timeout` passes; what arrived.
        pub fn wait(&self, timeout: Duration) -> Pending {
            let mut pfd = libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
            if PENDING.load(Ordering::SeqCst) == 0 {
                // SAFETY: one valid pollfd; EINTR is a wake like any other.
                unsafe {
                    libc::poll(&raw mut pfd, 1, ms);
                }
            }
            let mut sink = [0u8; 64];
            // SAFETY: the pipe is non-blocking; `sink` is writable for its length.
            while unsafe { libc::read(self.read.as_raw_fd(), sink.as_mut_ptr().cast(), sink.len()) }
                > 0
            {}
            let bits = PENDING.swap(0, Ordering::SeqCst);
            let mut pending = Pending::default();
            for sig in super::CAUGHT {
                if bits & bit(sig) != 0 {
                    pending.note(sig);
                }
            }
            pending
        }
    }
}

/// A `sigaction` with handler `handler`, flags `flags` and an empty mask.
#[cfg(target_os = "macos")]
fn action(handler: libc::sighandler_t, flags: libc::c_int) -> libc::sigaction {
    // Field by field (no `mem::zeroed`); a `sigset_t` is a plain bit mask here, and 0 is the
    // empty one, as `sigemptyset` makes it.
    libc::sigaction {
        sa_sigaction: handler,
        sa_mask: 0,
        sa_flags: flags,
    }
}

/// A `sigaction` with handler `handler`, flags `flags` and an empty mask.
#[cfg(not(target_os = "macos"))]
fn action(handler: libc::sighandler_t, flags: libc::c_int) -> libc::sigaction {
    // SAFETY: an all-zero sigaction is a valid value of the plain C struct (its fields'
    // layout differs between libc targets, so it is not spelled field by field here).
    let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
    sa.sa_sigaction = handler;
    sa.sa_flags = flags;
    // SAFETY: `sa.sa_mask` is a valid, writable `sigset_t`.
    unsafe {
        libc::sigemptyset(&raw mut sa.sa_mask);
    }
    sa
}

/// Whether `sig` is ignored in this process now.
///
/// # Errors
/// The `sigaction` query failed.
pub fn is_ignored(sig: libc::c_int) -> io::Result<bool> {
    let mut old = action(libc::SIG_DFL, 0);
    // SAFETY: `old` is a valid, writable out-slot; a null new action only queries.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut old) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(old.sa_sigaction == libc::SIG_IGN)
}

/// The signals the caller may have left ignored, whose ignore the child keeps.
const KEEP_IGNORED: [libc::c_int; 4] = [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM];

/// Install the policy.
///
/// # Errors
/// The wake source or a `sigaction` failed.
pub fn install() -> io::Result<Signals> {
    let wake = wake::Wake::new()?;
    for sig in CAUGHT {
        if KEEP_IGNORED.contains(&sig) && is_ignored(sig)? {
            continue;
        }
        wake.watch(sig)?;
        let sa = action(
            wake::on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            libc::SA_RESTART,
        );
        let no_old = std::ptr::null_mut();
        // SAFETY: `sa` is a valid sigaction whose handler is an `extern "C" fn(c_int)` that
        // makes no call (macOS) or only touches atomics, errno and write(2) (elsewhere).
        if unsafe { libc::sigaction(sig, &sa, no_old) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Signals { wake })
}

impl Signals {
    /// Wait until a signal arrives or `timeout` passes; what arrived.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> Pending {
        self.wake.wait(timeout)
    }
}

/// Send `sig` to `pid`; whether the kernel took it.
pub fn send(pid: u32, sig: libc::c_int) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(2) on one positive pid (never a group, never -1).
    unsafe { libc::kill(pid, sig) == 0 }
}

/// Stop this process with `sig` (a stop signal), as the child stopped.
pub fn stop_self(sig: libc::c_int) {
    // SAFETY: a stop signal to ourselves.
    unsafe {
        libc::kill(libc::getpid(), sig);
    }
}

fn dumps_core(sig: libc::c_int) -> bool {
    matches!(
        sig,
        libc::SIGQUIT
            | libc::SIGILL
            | libc::SIGTRAP
            | libc::SIGABRT
            | libc::SIGFPE
            | libc::SIGBUS
            | libc::SIGSEGV
            | libc::SIGSYS
    )
}

/// End this process as the child ended: by the same signal (so the shell sees what it
/// would have seen without the harness), with core dumps off so the harness leaves no
/// core of its own. Falls back to exit status `128 + sig`.
pub fn die_like(sig: libc::c_int) -> ! {
    // SAFETY: plain libc calls on this process with valid, initialized arguments.
    unsafe {
        if dumps_core(sig) {
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &none);
        }
        libc::signal(sig, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        libc::sigaddset(&raw mut set, sig);
        let no_old = std::ptr::null_mut();
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, no_old);
        libc::kill(libc::getpid(), sig);
    }
    // A signal number is small; saturating keeps an impossible one from overflowing.
    std::process::exit(128_i32.saturating_add(sig))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_kqueue_hears_a_signal_posted_before_the_wait() {
        let wake = wake::Wake::new().expect("kqueue");
        wake.watch(libc::SIGCHLD).expect("watch");
        // The child ends (posting SIGCHLD) before the wait starts: the filter kept it.
        let status = std::process::Command::new("/usr/bin/true")
            .status()
            .expect("spawn");
        assert!(status.success());
        let t = std::time::Instant::now();
        let got = wake.wait(Duration::from_secs(5));
        assert!(got.chld, "{got:?}");
        assert!(
            t.elapsed() < Duration::from_secs(4),
            "it did not wait out the timeout"
        );
    }

    #[test]
    fn a_wait_with_nothing_posted_returns_at_its_timeout() {
        let wake = wake::Wake::new().expect("wake");
        let t = std::time::Instant::now();
        let got = wake.wait(Duration::from_millis(60));
        assert_eq!(got, Pending::default());
        assert!(
            t.elapsed() >= Duration::from_millis(50),
            "{:?}",
            t.elapsed()
        );
    }
}
