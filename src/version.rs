// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! atpkg's vendor versions and their store build ids.
//!
//! Ported from aterm `crates/atpkg/src/vendor_direct/version.rs` (5e2c6d0c28): the parse,
//! the bijective build id and the order, nothing else. A vendor build's directory in the
//! store is named by its build id, so the id of `1.4.13-R6996.1` is
//! `2001004013006996002` (measured: `store/muse/2001004013006996002`).
//!
//! - `x.y.z` is `10^18 + x·10^12 + y·10^6 + z` (`x, y, z < 10^6`).
//! - `x.y.z-R<n>[.<s>]` is `2·10^18 + x·10^15 + y·10^12 + z·10^9 + n·10^3 + s'`
//!   (`x, y, z <= 999`, `n <= 999_999`, `s' = 0` or `s + 1` with `s <= 998`).

use std::fmt;

/// Smallest vendor build id (`0.0.0`).
pub const VENDOR_BUILD_BASE: u64 = 1_000_000_000_000_000_000;

const REVISION_BASE: u64 = 2 * VENDOR_BUILD_BASE;
const VENDOR_BUILD_END: u64 = 3 * VENDOR_BUILD_BASE;
const COMPONENT_LIMIT: u64 = 1_000_000;
const MAJOR_SCALE: u64 = 1_000_000_000_000;
const SHORT_LIMIT: u64 = 1_000;
const REVISION_MAJOR_SCALE: u64 = 1_000_000_000_000_000;
const REVISION_MINOR_SCALE: u64 = 1_000_000_000_000;
const REVISION_PATCH_SCALE: u64 = 1_000_000_000;

/// A canonical vendor version in either grammar, held as its build id (so `Ord` is the
/// store's order).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u64);

impl Version {
    /// `major.minor.patch`, or `None` if a component is `>= 10^6`.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Option<Self> {
        let (major, minor, patch) = (major as u64, minor as u64, patch as u64);
        if major >= COMPONENT_LIMIT || minor >= COMPONENT_LIMIT || patch >= COMPONENT_LIMIT {
            return None;
        }
        Some(Self(
            VENDOR_BUILD_BASE + major * MAJOR_SCALE + minor * COMPONENT_LIMIT + patch,
        ))
    }

    /// `major.minor.patch-R<revision>[.<sub>]`, or `None` if a field exceeds its bound.
    #[must_use]
    pub const fn with_revision(
        major: u32,
        minor: u32,
        patch: u32,
        revision: u32,
        sub: Option<u32>,
    ) -> Option<Self> {
        let (major, minor, patch) = (major as u64, minor as u64, patch as u64);
        let revision = revision as u64;
        let sub = match sub {
            Some(s) => s as u64 + 1,
            None => 0,
        };
        if major >= SHORT_LIMIT
            || minor >= SHORT_LIMIT
            || patch >= SHORT_LIMIT
            || revision >= COMPONENT_LIMIT
            || sub >= SHORT_LIMIT
        {
            return None;
        }
        Some(Self(
            REVISION_BASE
                + major * REVISION_MAJOR_SCALE
                + minor * REVISION_MINOR_SCALE
                + patch * REVISION_PATCH_SCALE
                + revision * SHORT_LIMIT
                + sub,
        ))
    }

    /// Parse canonical `x.y.z` or `x.y.z-R<n>[.<s>]` (no leading zeros).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (plain, revision) = match text.split_once("-R") {
            Some((plain, revision)) => (plain, Some(revision)),
            None => (text, None),
        };
        let mut parts = plain.split('.');
        let major = component(parts.next()?)?;
        let minor = component(parts.next()?)?;
        let patch = component(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let Some(revision) = revision else {
            return Self::new(major, minor, patch);
        };
        let (n, sub) = match revision.split_once('.') {
            Some((n, sub)) => (component(n)?, Some(component(sub)?)),
            None => (component(revision)?, None),
        };
        Self::with_revision(major, minor, patch, n, sub)
    }

    /// The version a vendor build id names, or `None` outside `[10^18, 3·10^18)`.
    #[must_use]
    pub const fn from_build_id(build: u64) -> Option<Self> {
        if build >= VENDOR_BUILD_BASE && build < VENDOR_BUILD_END {
            Some(Self(build))
        } else {
            None
        }
    }

    /// The store build id.
    #[must_use]
    pub const fn build_id(self) -> u64 {
        self.0
    }

    /// Whether this is the revision form (`x.y.z-R<n>[.<s>]`).
    #[must_use]
    pub const fn has_revision(self) -> bool {
        self.0 >= REVISION_BASE
    }

    const fn major(self) -> u64 {
        if self.has_revision() {
            self.0 / REVISION_MAJOR_SCALE % SHORT_LIMIT
        } else {
            self.0 / MAJOR_SCALE % COMPONENT_LIMIT
        }
    }

    const fn minor(self) -> u64 {
        if self.has_revision() {
            self.0 / REVISION_MINOR_SCALE % SHORT_LIMIT
        } else {
            self.0 / COMPONENT_LIMIT % COMPONENT_LIMIT
        }
    }

    const fn patch(self) -> u64 {
        if self.has_revision() {
            self.0 / REVISION_PATCH_SCALE % SHORT_LIMIT
        } else {
            self.0 % COMPONENT_LIMIT
        }
    }
}

/// One canonical number: 1–6 ASCII digits, no leading zero unless it is `0`.
fn component(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major(), self.minor(), self.patch())?;
        if self.has_revision() {
            let n = self.0 / SHORT_LIMIT % COMPONENT_LIMIT;
            // The short field holds the suffix plus one (0: none).
            match (self.0 % SHORT_LIMIT).checked_sub(1) {
                None => write!(f, "-R{n}")?,
                Some(s) => write!(f, "-R{n}.{s}")?,
            }
        }
        Ok(())
    }
}

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Version({self})")
    }
}

/// Whether a session running `running` moves onto `target`: the same grammar, and
/// `target` strictly newer (aterm `upgrade_muse_live.rs` `moves_onto`; a version in the
/// other grammar is no order at all, so it never moves).
#[must_use]
pub fn moves_onto(running: Version, target: Version) -> bool {
    running.has_revision() == target.has_revision() && target > running
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_id_of_the_measured_build_is_its_directory_name() {
        let v = Version::parse("1.4.13-R6996.1").expect("canonical");
        assert_eq!(v.build_id(), 2_001_004_013_006_996_002);
        assert_eq!(v.to_string(), "1.4.13-R6996.1");
        assert_eq!(
            Version::parse("2.1.310").map(Version::build_id),
            Some(1_000_002_000_001_000_310)
        );
        assert_eq!(
            Version::from_build_id(2_001_004_011_006_667_002).map(|v| v.to_string()),
            Some("1.4.11-R6667.1".to_string())
        );
        assert_eq!(Version::from_build_id(2_026_102_001), None, "an ALab build");
    }

    #[test]
    fn only_canonical_text_parses() {
        for bad in [
            "",
            "1.4",
            "1.4.10.1",
            "01.4.10",
            "1.4.10-R",
            "1.4.10-R6427.x",
            "1.4.11-beta",
            "1000.0.0-R1",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        for good in [
            "0.0.0",
            "1.4.10-R6427",
            "1.4.10-R6427.0",
            "999.999.999-R999999.997",
        ] {
            let v = Version::parse(good).expect(good);
            assert_eq!(v.to_string(), good);
        }
    }

    #[test]
    fn the_order_moves_only_forward_within_one_grammar() {
        let v = |s: &str| Version::parse(s).expect(s);
        for (running, target) in [
            ("1.4.10-R6427.1", "1.4.11-R6500.1"),
            ("1.4.10-R6427.1", "1.4.10-R6500.1"),
            ("1.4.10-R6427", "1.4.10-R6427.1"),
            ("1.4.10-R6427.1", "2.0.0-R1"),
        ] {
            assert!(moves_onto(v(running), v(target)), "{running} -> {target}");
        }
        for (running, target) in [
            ("1.4.10-R6427.1", "1.4.10-R6427.1"),
            ("1.4.10-R6427.1", "1.4.10-R6400.1"),
            ("1.4.11-R6500.1", "1.4.10-R6900.1"),
            ("1.4.10-R6427.1", "1.4.11"),
        ] {
            assert!(!moves_onto(v(running), v(target)), "{running} -> {target}");
        }
    }
}
