// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE BUILD PROBE: a newer vendor build must be proved to start before the harness stops a
//! working session to move it there, or relaunches a crashed one onto it.
//!
//! aterm's own Muse Code quiet restart requires the same before its `SIGTERM`
//! (`upgrade_muse_live_drive.rs`, the `build_start::proved_named` call, at 5e2c6d0c28):
//! `upgrade_build_start.rs` runs `<exe> --help` with no terminal (stdin, stdout and stderr
//! on `/dev/null`) in its own process group, waits at most 5 s (`VERSION_WAIT`), and
//! proves the build only when it exits 0; a build that failed is probed again no sooner
//! than 15 minutes later (`PROBE_GAP_S`). A build atpkg verified can still fail to start
//! (a damaged or wrong-architecture binary, a code-signing kill), and stopping an idle
//! session to land it there would end the session instead of moving it.
//!
//! The probe is the harness's own throwaway process, never the session: on a timeout its
//! own process group (and nothing else) is killed.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::os::unix::process::CommandExt as _;

/// What the probe passes: loads the program without starting a session (aterm's
/// `PROBE_ARG`).
pub const PROBE_ARG: &str = "--help";

/// How long the probe may run (aterm's `VERSION_WAIT`).
pub const PROBE_WAIT: Duration = Duration::from_secs(5);

/// How long after a failed probe the same build is probed again (aterm's `PROBE_GAP_S`).
pub const PROBE_GAP: Duration = Duration::from_secs(15 * 60);

/// What one probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// It ran and exited 0.
    Starts,
    /// It ran and exited otherwise, or a signal ended it.
    Exited,
    /// It could not be started at all.
    NoExec,
    /// It did not end within the bound; its process group was killed.
    TimedOut,
}

impl Probe {
    /// Its journal word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Probe::Starts => "starts",
            Probe::Exited => "exited",
            Probe::NoExec => "no-exec",
            Probe::TimedOut => "timed-out",
        }
    }
}

/// Run `exe --help` with no terminal, in its own process group, with `envs` added (the
/// harness's recursion marks), for at most `limit`.
#[must_use]
pub fn probe_within(exe: &Path, envs: &[(&str, String)], limit: Duration) -> Probe {
    let deadline = Instant::now() + limit;
    let mut cmd = Command::new(exe);
    cmd.arg(PROBE_ARG)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let Ok(mut child) = cmd.spawn() else {
        return Probe::NoExec;
    };
    let group = libc::pid_t::try_from(child.id()).ok();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Probe::Starts
                } else {
                    Probe::Exited
                };
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                if let Some(group) = group
                    && group > 0
                {
                    // SAFETY: kill(2) on the probe's OWN process group (`process_group(0)`
                    // made its pgid its pid), sent before the probe is reaped, so the id is
                    // still its; never the session's group.
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                return Probe::TimedOut;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exit_zero_proves_a_build() {
        assert_eq!(
            probe_within(Path::new("/usr/bin/true"), &[], PROBE_WAIT),
            Probe::Starts
        );
        assert_eq!(
            probe_within(Path::new("/usr/bin/false"), &[], PROBE_WAIT),
            Probe::Exited
        );
        assert_eq!(
            probe_within(Path::new("/nonexistent/muse"), &[], PROBE_WAIT),
            Probe::NoExec
        );
    }

    #[test]
    fn a_probe_that_hangs_is_ended_with_its_own_group() {
        let dir = std::env::temp_dir().join(format!("muse-alab-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let hang = dir.join("hang");
        // The probe and a grandchild it leaves behind: both are in the probe's own group.
        std::fs::write(&hang, "#!/bin/sh\n/bin/sleep 30 &\nexec /bin/sleep 30\n").expect("write");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }
        let t = Instant::now();
        assert_eq!(
            probe_within(&hang, &[], Duration::from_millis(300)),
            Probe::TimedOut
        );
        assert!(t.elapsed() < Duration::from_secs(5), "bounded");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
