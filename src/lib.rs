// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! muse-alab: the ALab harness for Muse Code.
//!
//! The harness is a Rust program that runs the UNMODIFIED vendor CLI (`muse`, Meta's
//! closed binary, installed by atpkg) as its child on the inherited terminal. It never
//! ships, forks or patches vendor bytes. What it adds:
//!
//! - run by atpkg's launcher (`muse-alab run -- <muse arguments>`), it does what the launcher
//!   did for the word `muse` before Muse Code starts: the yolo default, byte for byte
//!   ([`launcher`]);
//! - it finds the vendor binary in the atpkg store only ([`store`]), never through `PATH`
//!   or the `agents/` twin, refuses a store binary that is itself, and marks every process
//!   it starts as the vendor ([`env::child_marks`]) so that a harness started by a harness
//!   refuses at once: it can never run itself;
//! - it keeps the vendor's signals and exit status faithful ([`signals`], [`child`]);
//! - it moves the running session onto a newer vendor build while the harness keeps
//!   running: at a proven safe point ([`gate`]), with the new build proved to start
//!   ([`probe`]), it stops the child and relaunches the new build with Muse Code's own
//!   resume argv ([`muse::argv`]); a busy child is never stopped;
//! - it relaunches a crashed child on its session after aterm's back-off ladder's pause, and
//!   restarts it on the same session when aterm asks ([`supervise`]);
//! - under aterm it holds the session through a claim of both its duties
//!   (`muse-alab@<pid> scope=relaunch,upgrade`, at contract 4, never 3) made by the shared
//!   aterm client `alab-wire` ([`aterm`]), every act waiting on a fresh read of aterm, and
//!   gets out of the way (Muse Code `exec`ed in its place) wherever aterm refuses that claim;
//! - it moves itself onto a newer build of the harness the same way: at the same safe point it
//!   `execve`s the new image in its own process, the child and its episode carried across on an
//!   inherited descriptor ([`adopt`]).

pub mod adopt;
pub mod aterm;
pub mod child;
pub mod env;
pub mod gate;
pub mod journal;
pub mod json;
pub mod launcher;
pub mod muse;
pub mod probe;
pub mod procs;
pub mod signals;
pub mod store;
pub mod supervise;
pub mod tty;
pub mod version;

/// The harness's own name: the program, the exposed tool and the claim holder
/// (`muse-alab@<pid>`). Never the vendor's command name.
pub const HARNESS: &str = "muse-alab";

/// The harness's own version, independent of the vendor CLI's: the version an atpkg pack signs
/// (`MAJOR.MINOR.0+g<sha9>`), which `atpkg-publish pack` hands the build as
/// `ALAB_HARNESS_VERSION` (aterm's harness design, D46), else the package's `MAJOR.MINOR.0`. A
/// value that is not this package's line ([`is_pack_version`]) fails the build.
pub const HARNESS_VERSION: &str = match option_env!("ALAB_HARNESS_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

const _: () = assert!(
    is_pack_version(HARNESS_VERSION, env!("CARGO_PKG_VERSION")),
    "ALAB_HARNESS_VERSION is not this package's MAJOR.MINOR.0, alone or with +g<sha>"
);

const fn release_component(bytes: &[u8]) -> Option<&[u8]> {
    let (first, mut rest) = match bytes.split_first() {
        Some((&first, rest)) if first.is_ascii_digit() => (first, rest),
        _ => return None,
    };
    while let Some((&digit, tail)) = rest.split_first() {
        if digit == b'.' {
            return Some(tail);
        }
        if first == b'0' || !digit.is_ascii_digit() {
            return None;
        }
        rest = tail;
    }
    None
}

const fn release_base(bytes: &[u8]) -> bool {
    let Some(after_major) = release_component(bytes) else {
        return false;
    };
    let Some(after_minor) = release_component(after_major) else {
        return false;
    };
    matches!(after_minor, [b'0'])
}

/// Whether `v` is the `MAJOR.MINOR.0` release line `line` alone, or `line` followed by `+g` and
/// 9 to 40 lowercase hex digits (a dev build's commit): the only versions a pack may hand the
/// build.
#[must_use]
pub const fn is_pack_version(v: &str, line: &str) -> bool {
    let mut base = line.as_bytes();
    if !release_base(base) {
        return false;
    }
    let mut rest = v.as_bytes();
    while let Some((&expected, tail)) = base.split_first() {
        rest = match rest.split_first() {
            Some((&actual, tail)) if actual == expected => tail,
            _ => return false,
        };
        base = tail;
    }
    if rest.is_empty() {
        return true;
    }
    rest = match rest {
        [b'+', b'g', stamp @ ..] => stamp,
        _ => return false,
    };
    if !matches!(rest.len(), 9..=40) {
        return false;
    }
    while let Some((&digit, tail)) = rest.split_first() {
        if !matches!(digit, b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        rest = tail;
    }
    true
}

/// Write one line to stderr. A closed or broken stderr is ignored: `eprintln!` would
/// panic, and a panic between a stop and its relaunch would lose the session.
pub fn say(line: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// What `--version` says when atpkg has no `current` build of the vendor.
pub const NOT_INSTALLED: &str = "not installed";

/// What `--version` says when a `current` build exists but its version cannot be read: no
/// readable store, a link that names no build, a `.vendor` record that is missing, names no
/// canonical version or disagrees with its build directory, or a build that is not complete.
pub const UNKNOWN: &str = "unknown";

/// The vendor part of the `--version` line, from what the store says of its `current` build
/// ([`store::current`]): the version its `.vendor` record names, checked against its build
/// directory, else [`NOT_INSTALLED`] or [`UNKNOWN`]. Never decoded from the build id alone and
/// never asked of the vendor binary, which `--version` does not run.
///
/// # Errors
/// [`NOT_INSTALLED`] or [`UNKNOWN`], the word the line prints in place of a version.
pub fn vendor_version(
    current: &Result<store::Build, store::StoreError>,
) -> Result<String, &'static str> {
    match current {
        Ok(build) => Ok(build.version.to_string()),
        Err(store::StoreError::NotInstalled { .. }) => Err(NOT_INSTALLED),
        Err(_) => Err(UNKNOWN),
    }
}

/// The `--version` line: `muse-alab <version> (muse <vendor version>)`, or
/// `(muse not installed)` / `(muse unknown)` where there is no version to name.
#[must_use]
pub fn version_line(vendor: Result<&str, &str>) -> String {
    match vendor {
        Ok(v) => format!("{HARNESS} {HARNESS_VERSION} ({} {v})", muse::PROGRAM),
        Err(why) => format!("{HARNESS} {HARNESS_VERSION} ({} {why})", muse::PROGRAM),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_names_the_harness_and_the_vendor_build() {
        assert_eq!(
            version_line(Ok("1.4.13-R6996.1")),
            format!("muse-alab {HARNESS_VERSION} (muse 1.4.13-R6996.1)")
        );
        assert_eq!(
            version_line(Err(NOT_INSTALLED)),
            format!("muse-alab {HARNESS_VERSION} (muse not installed)")
        );
        assert_eq!(
            version_line(Err(UNKNOWN)),
            format!("muse-alab {HARNESS_VERSION} (muse unknown)")
        );
    }

    #[test]
    fn only_a_missing_current_link_is_not_installed() {
        use std::path::PathBuf;
        let link = PathBuf::from("/p/store/muse/current");
        assert_eq!(
            vendor_version(&Err(store::StoreError::NotInstalled { link: link.clone() })),
            Err(NOT_INSTALLED)
        );
        for err in [
            store::StoreError::NoHome,
            store::StoreError::Link {
                link: link.clone(),
                why: "names no build",
            },
            store::StoreError::Record {
                record: PathBuf::from("/p/store/muse/1.vendor"),
                why: "is missing or unreadable",
            },
            store::StoreError::NotReady {
                marker: PathBuf::from("/p/store/muse/1.ready"),
            },
            store::StoreError::Binary {
                exe: PathBuf::from("/p/store/muse/1/bin/muse"),
                why: "is missing",
            },
        ] {
            assert_eq!(vendor_version(&Err(err.clone())), Err(UNKNOWN), "{err:?}");
        }
        let version = version::Version::parse("1.4.13-R6996.1").expect("canonical");
        let build = store::Build {
            version,
            dir: PathBuf::from("/p/store/muse/2001004013006996002"),
            exe: PathBuf::from("/p/store/muse/2001004013006996002/bin/muse"),
            env: Vec::new(),
        };
        assert_eq!(vendor_version(&Ok(build)), Ok("1.4.13-R6996.1".to_string()));
    }

    #[test]
    fn the_harness_is_never_named_like_the_vendor() {
        assert_ne!(HARNESS, muse::PROGRAM);
        assert!(HARNESS.ends_with("-alab"));
        let parts: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
        assert_eq!(parts.len(), 3, "MAJOR.MINOR.0");
        assert_eq!(parts[2], "0", "the patch slot is always 0");
    }

    #[test]
    fn a_pack_hands_the_release_line_or_the_line_and_its_commit() {
        let line = "0.1.0";
        for (v, ok) in [
            ("0.1.0", true),
            ("0.1.0+gabcdef012", true),
            ("0.1.0+g0123456789abcdef0123456789abcdef01234567", true),
            ("0.1.0+gabcdef01", false),
            ("0.1.0+g0123456789abcdef0123456789abcdef012345678", false),
            ("0.1.0+gABCDEF012", false),
            ("0.1.0+gabcdef01z", false),
            ("0.1.0+abcdef012", false),
            ("0.1.0+g", false),
            ("0.1.0+", false),
            ("0.1.0-dev.3+gabcdef012", false),
            ("0.2.0+gabcdef012", false),
            ("0.1", false),
            ("", false),
        ] {
            assert_eq!(is_pack_version(v, line), ok, "{v:?}");
        }
        for bad_line in ["0.1.2", "01.1.0", "0.01.0", "0.1", "0.1.0.0", ""] {
            assert!(!is_pack_version(bad_line, bad_line), "{bad_line:?}");
            assert!(
                !is_pack_version(&format!("{bad_line}+gabcdef012"), bad_line),
                "{bad_line:?}"
            );
        }
        assert!(is_pack_version("10.20.0+gabcdef012", "10.20.0"));
        assert!(is_pack_version(HARNESS_VERSION, env!("CARGO_PKG_VERSION")));
        assert_eq!(
            HARNESS_VERSION,
            option_env!("ALAB_HARNESS_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
    }
}
