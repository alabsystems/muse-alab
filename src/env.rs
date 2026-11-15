// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The few environment facts the harness reads, read once at start.
//!
//! The harness adds no environment knobs of its own (release builds). Debug builds read
//! one test-only timing override ([`crate::supervise::Config::for_this_build`]).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// aterm's harness opt-out (`atpkg::yolo::NO_HARNESS_ENV`, set by the `agents/` twin for
/// `--no-harness`): any non-empty value other than `0` means "no harness".
pub const NO_HARNESS_ENV: &str = "ATERM_NO_HARNESS";

/// The variable the harness sets in every process it starts as the vendor to its own pid.
/// A harness that finds it naming its parent (or itself: an `exec` in between keeps the
/// pid) was started AS THE VENDOR by a harness, through anything the same-file check
/// cannot see (a copy of another harness build, or a store `muse` that `exec`s the harness
/// again), and refuses at once (the claude-code-alab harness carries the same guard).
pub const PARENT_VAR: &str = "MUSE_ALAB_PID";

/// The variable the harness sets in every process it starts as the vendor to its own
/// nesting depth plus one. [`PARENT_VAR`] catches a harness started directly as the vendor;
/// this catches one reached again through a process in between (a store `muse` that runs
/// the harness without `exec`), which the parent check cannot see.
pub const DEPTH_VAR: &str = "MUSE_ALAB_DEPTH";

/// The most harnesses that may run nested in one another (a harness started from a Muse
/// Code tool's shell is one level deeper). At it, a harness refuses, so no chain of
/// wrappers recurses without bound.
pub const MAX_DEPTH: u32 = 8;

/// What the harness reads from its environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// `HOME`, when absolute.
    pub home: Option<PathBuf>,
    /// `XDG_CONFIG_HOME`, when non-empty (where aterm reads `aterm/aterm.toml`).
    pub xdg_config_home: Option<OsString>,
    /// `XDG_DATA_HOME` as set (Muse Code's data home; empty means unset).
    pub xdg_data_home: Option<OsString>,
    /// `XDG_STATE_HOME`, when absolute (the harness journal's home).
    pub xdg_state_home: Option<PathBuf>,
    /// [`NO_HARNESS_ENV`] is set.
    pub no_harness: bool,
    /// [`PARENT_VAR`] as inherited.
    pub parent_mark: Option<OsString>,
    /// [`DEPTH_VAR`] as inherited.
    pub depth_mark: Option<OsString>,
    /// The variables `alab-wire` reads a session from, as inherited: the names its
    /// [`alab_wire::EnvNames`] lists for this harness (the tab's session, the rendezvous
    /// directory's homes, and the marks of an aterm tab), read through [`Env::aterm_var`].
    pub aterm: Vec<(String, OsString)>,
}

impl Env {
    /// The process's environment.
    #[must_use]
    pub fn from_process() -> Self {
        let get = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Self {
            home: get("HOME").map(PathBuf::from).filter(|p| p.is_absolute()),
            xdg_config_home: get("XDG_CONFIG_HOME"),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
            xdg_state_home: get("XDG_STATE_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            no_harness: get(NO_HARNESS_ENV).is_some_and(|v| v != "0"),
            parent_mark: std::env::var_os(PARENT_VAR),
            depth_mark: std::env::var_os(DEPTH_VAR),
            aterm: aterm_names()
                .into_iter()
                .filter_map(|name| std::env::var_os(&name).map(|value| (name, value)))
                .collect(),
        }
    }

    /// An environment holding only these aterm variables (a test's, or a caller's that read
    /// them elsewhere).
    #[must_use]
    pub fn with_aterm_vars(pairs: &[(&str, &str)]) -> Self {
        Self {
            aterm: pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
                .collect(),
            ..Self::default()
        }
    }

    /// The aterm variable `name` as inherited ([`Env::aterm`]).
    #[must_use]
    pub fn aterm_var(&self, name: &str) -> Option<OsString> {
        self.aterm
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, value)| value.clone())
    }

    /// `HOME`, when absolute.
    #[must_use]
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }
}

/// Every variable `alab-wire` reads a session from for this harness ([`crate::aterm::config`]).
fn aterm_names() -> Vec<String> {
    let config = crate::aterm::config();
    let names = config.env();
    let mut out = vec![
        names.session.clone(),
        names.runtime_dir.clone(),
        names.home.clone(),
    ];
    out.extend(names.hosted.iter().map(|mark| mark.name.clone()));
    out.sort();
    out.dedup();
    out
}

/// Whether the inherited [`PARENT_VAR`] names `parent` or `me`: this process was started as
/// a harness's vendor (directly, or through an `exec`).
#[must_use]
pub fn started_as_vendor(mark: Option<&OsStr>, parent: u32, me: u32) -> bool {
    mark.and_then(OsStr::to_str)
        .and_then(|m| m.parse::<u32>().ok())
        .is_some_and(|pid| pid == parent || pid == me)
}

/// This harness's nesting depth from the inherited [`DEPTH_VAR`]: 0 when unset, refused at
/// [`MAX_DEPTH`] and past it, and refused when it is not a number (an unreadable depth is
/// never read as "not nested").
///
/// # Errors
/// Why the harness must not run.
pub fn nesting_depth(mark: Option<&OsStr>) -> Result<u32, String> {
    let Some(value) = mark else {
        return Ok(0);
    };
    let depth = value
        .to_str()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| {
            format!(
                "{DEPTH_VAR}={} is not a nesting depth",
                value.to_string_lossy()
            )
        })?;
    if depth >= MAX_DEPTH {
        return Err(format!(
            "{DEPTH_VAR}={depth}: muse-alab is already nested {depth} deep (at most \
             {MAX_DEPTH}); the atpkg store's `muse` probably runs muse-alab again"
        ));
    }
    Ok(depth)
}

/// The marks every process the harness starts as the vendor carries: [`PARENT_VAR`] (this
/// process's pid) and [`DEPTH_VAR`] (`depth + 1`).
#[must_use]
pub fn child_marks(depth: u32) -> Vec<(&'static str, String)> {
    vec![
        (PARENT_VAR, std::process::id().to_string()),
        (DEPTH_VAR, depth.saturating_add(1).to_string()),
    ]
}

/// The harness's own launch words that aterm's `agents/` twin also reads
/// (`--no-harness`, `--no-alab-harness`, before a literal `--`): whether one was given,
/// and the arguments without them.
#[must_use]
pub fn strip_no_harness(args: Vec<OsString>) -> (bool, Vec<OsString>) {
    let mut found = false;
    let mut after_dashes = false;
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if !after_dashes && (a == "--no-harness" || a == "--no-alab-harness") {
            found = true;
            continue;
        }
        if a == "--" {
            after_dashes = true;
        }
        out.push(a);
    }
    (found, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_harness_started_as_a_harnesss_vendor_is_known() {
        assert!(started_as_vendor(Some(OsStr::new("41")), 41, 50));
        assert!(
            started_as_vendor(Some(OsStr::new("50")), 41, 50),
            "through an exec"
        );
        assert!(!started_as_vendor(Some(OsStr::new("7")), 41, 50));
        assert!(!started_as_vendor(Some(OsStr::new("x")), 41, 50));
        assert!(!started_as_vendor(None, 41, 50));
    }

    #[test]
    fn the_nesting_depth_is_bounded_and_read_strictly() {
        assert_eq!(nesting_depth(None), Ok(0));
        assert_eq!(nesting_depth(Some(OsStr::new("3"))), Ok(3));
        assert_eq!(
            nesting_depth(Some(OsStr::new("7"))),
            Ok(MAX_DEPTH - 1),
            "one below the bound still runs"
        );
        assert!(nesting_depth(Some(OsStr::new("8"))).is_err());
        assert!(nesting_depth(Some(OsStr::new("4294967295"))).is_err());
        assert!(
            nesting_depth(Some(OsStr::new(""))).is_err(),
            "unreadable is not 0"
        );
        assert!(nesting_depth(Some(OsStr::new("-1"))).is_err());
        let marks = child_marks(2);
        assert_eq!(marks[0], (PARENT_VAR, std::process::id().to_string()));
        assert_eq!(marks[1], (DEPTH_VAR, "3".to_string()));
        assert_eq!(
            child_marks(u32::MAX)[1].1,
            u32::MAX.to_string(),
            "saturates"
        );
    }

    /// The variables a session is read from are alab-wire's names for them, each once.
    #[test]
    fn the_aterm_variables_are_alab_wires_names() {
        let names = aterm_names();
        for name in [
            "ATERM_PARENT_SESSION_ID",
            "XDG_RUNTIME_DIR",
            "HOME",
            "TERM_PROGRAM",
            "ATERM_CHILD",
            "ATERM_SESSION_ID",
        ] {
            assert!(names.iter().any(|n| n == name), "{name}: {names:?}");
        }
        let env = Env::with_aterm_vars(&[("TERM_PROGRAM", "aterm")]);
        assert_eq!(env.aterm_var("TERM_PROGRAM"), Some(OsString::from("aterm")));
        assert_eq!(env.aterm_var("ATERM_CHILD"), None);
    }

    #[test]
    fn the_no_harness_words_are_read_only_before_the_dashes() {
        assert_eq!(
            strip_no_harness(os(&["--no-harness", "resume", "x"])),
            (true, os(&["resume", "x"]))
        );
        assert_eq!(
            strip_no_harness(os(&["a", "--no-alab-harness"])),
            (true, os(&["a"]))
        );
        assert_eq!(
            strip_no_harness(os(&["--", "--no-harness"])),
            (false, os(&["--", "--no-harness"]))
        );
        assert_eq!(strip_no_harness(os(&[])), (false, os(&[])));
    }
}
