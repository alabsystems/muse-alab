// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The harness's journal: one line per decision, appended to
//! `$XDG_STATE_HOME/muse-alab/journal` (else `~/.local/state/muse-alab/journal`), kept
//! under 1 MiB by one rotation. The harness never writes to the terminal while the child
//! owns it; the journal is where it says what it waited on and why. A failed write is
//! dropped: the journal never blocks the session.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::Env;

/// The size past which the journal rotates.
const CAP: u64 = 1 << 20;

/// The journal.
#[derive(Clone, Debug)]
pub struct Journal {
    path: Option<PathBuf>,
}

impl Journal {
    /// The journal for `env` (nowhere, when there is no state home).
    #[must_use]
    pub fn open(env: &Env) -> Self {
        let dir = env
            .xdg_state_home
            .clone()
            .or_else(|| env.home().map(|h| h.join(".local").join("state")))
            .map(|d| d.join(crate::HARNESS));
        let path = dir.and_then(|d| {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&d)
                .ok()
                .map(|()| d.join("journal"))
        });
        Self { path }
    }

    /// Where it is written.
    #[must_use]
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Append `<time> pid=<harness pid> <word> <detail>`.
    pub fn note(&self, word: &str, detail: &str) {
        let Some(path) = &self.path else {
            return;
        };
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > CAP) {
            let _ = fs::rename(path, path.with_extension("1"));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let line = format!(
            "{}.{:03} pid={} {word} {}\n",
            now.as_secs(),
            now.subsec_millis(),
            std::process::id(),
            detail.replace(|c: char| c.is_control(), " ")
        );
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            && f.metadata().is_ok_and(|m| m.is_file())
        {
            let _ = f.write_all(line.as_bytes());
        }
    }
}
