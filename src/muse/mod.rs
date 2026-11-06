// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! EVERYTHING MUSE CODE: the vendor facts this harness knows, each ported from aterm's
//! measured Muse Code lanes (aterm `crates/aterm-agent/src/harness/upgrade_muse*.rs` at
//! 5e2c6d0c28) and cited where it is used.
//!
//! The rest of the crate is vendor-neutral (the store, signals, the child, the gate, the
//! loop); a later `alab-harness` library can take it whole.

pub mod argv;
pub mod log;
pub mod session;

/// The vendor's command (`muse`), the binary's name in its store build.
pub const PROGRAM: &str = "muse";

/// The atpkg package that installs it (aterm `crates/atpkg/src/vendor_direct/table.rs`,
/// the `muse` row).
pub const PACKAGE: &str = "muse";

/// The product's name, for messages.
pub const PRODUCT: &str = "Muse Code";

/// How a Muse Code process ended (aterm `upgrade_muse.rs` `ExitKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// A clean exit, or a handled `SIGINT`/`SIGTERM`: the person (or someone) ended it.
    Clean,
    /// A startup refusal (`1` or `2`): typed again it would refuse again.
    Refused,
    /// Anything else (`SIGHUP`, `SIGKILL`, a crash): the session did not end by choice.
    Crashed,
}

impl ExitKind {
    /// Its journal word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            ExitKind::Clean => "clean",
            ExitKind::Refused => "refused",
            ExitKind::Crashed => "crashed",
        }
    }
}

/// Classify a Muse Code exit status in the shell's form (`128 + signal` for a signal
/// death) — aterm `upgrade_muse.rs` `exit_kind`, measured against Muse Code 1.4.10.
#[must_use]
pub const fn exit_kind(status: i64) -> ExitKind {
    match status {
        0 | 130 | 143 => ExitKind::Clean,
        1 | 2 => ExitKind::Refused,
        _ => ExitKind::Crashed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_table_is_muse_codes_own() {
        for (status, kind) in [
            (0, ExitKind::Clean),
            (130, ExitKind::Clean),
            (143, ExitKind::Clean),
            (1, ExitKind::Refused),
            (2, ExitKind::Refused),
            (129, ExitKind::Crashed),
            (137, ExitKind::Crashed),
            (139, ExitKind::Crashed),
            (134, ExitKind::Crashed),
            (101, ExitKind::Crashed),
        ] {
            assert_eq!(exit_kind(status), kind, "{status}");
        }
    }
}
