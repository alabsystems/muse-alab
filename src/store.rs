// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ATPKG STORE, READ ONLY: where the vendor's builds live and which one is current.
//!
//! The harness resolves the vendor binary here and nowhere else: never through `PATH`,
//! never through atpkg's `agents/<cmd>` twin (a shell script that would `exec` whatever it
//! names, and that a later atpkg may point at this harness). So the harness can never run
//! itself.
//!
//! Layout (aterm `crates/atpkg/src/store.rs` `Layout::program_current` and
//! `platform/unix.rs` `default_prefix`, 5e2c6d0c28; measured on this Mac 2026-10-24):
//!
//! ```text
//! <prefix>/store/<program>/<build>/bin/<cmd>    the unpacked vendor build
//! <prefix>/store/<program>/<build>.vendor       its install record: version = "<x.y.z[-R<n>[.<s>]]>"
//! <prefix>/store/<program>/<build>.ready        "ok" once the build is complete
//! <prefix>/store/<program>/current -> <prefix>/store/<program>/<build>
//! ```
//!
//! `<prefix>` is `~/Library/Application Support/aterm/pkg` on macOS
//! (`~/.local/share/aterm/pkg` elsewhere), or `[packages] prefix` in aterm's
//! `aterm.toml` when it is set (read by [`configured_prefix`], vetted by [`vet_prefix`]).

use std::fmt;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::env::Env;
use crate::version::Version;

/// The store directory under the prefix.
pub const STORE_DIR: &str = "store";

/// The link naming a program's current build.
pub const CURRENT: &str = "current";

/// Why the store names no runnable vendor build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// `HOME` is unset or not absolute.
    NoHome,
    /// aterm's config sets `[packages] prefix` in a spelling the harness does not read.
    Config {
        /// The config file.
        path: PathBuf,
        /// What was found.
        why: &'static str,
    },
    /// The configured prefix is not one the harness can use.
    Prefix {
        /// The configured value.
        prefix: String,
        /// Why.
        why: &'static str,
    },
    /// No `current` link: the program is not installed.
    NotInstalled {
        /// The link that is missing.
        link: PathBuf,
    },
    /// The `current` link does not name a build of this program in the store.
    Link {
        /// The link.
        link: PathBuf,
        /// Why.
        why: &'static str,
    },
    /// The build's install record is missing or disagrees with its directory.
    Record {
        /// The record.
        record: PathBuf,
        /// Why.
        why: &'static str,
    },
    /// The build's ready marker is missing.
    NotReady {
        /// The marker.
        marker: PathBuf,
    },
    /// The build's binary is missing or not a runnable regular file.
    Binary {
        /// The binary.
        exe: PathBuf,
        /// Why.
        why: &'static str,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NoHome => write!(f, "HOME is unset or not absolute"),
            StoreError::Config { path, why } => write!(
                f,
                "{} sets [packages] prefix as {why}, which this harness does not read",
                path.display()
            ),
            StoreError::Prefix { prefix, why } => {
                write!(f, "the configured atpkg prefix {prefix:?} is {why}")
            }
            StoreError::NotInstalled { link } => {
                write!(f, "it is not installed ({} is missing)", link.display())
            }
            StoreError::Link { link, why } => write!(f, "{} {why}", link.display()),
            StoreError::Record { record, why } => {
                write!(f, "its install record {} {why}", record.display())
            }
            StoreError::NotReady { marker } => {
                write!(
                    f,
                    "its build is not complete ({} is missing)",
                    marker.display()
                )
            }
            StoreError::Binary { exe, why } => write!(f, "{} {why}", exe.display()),
        }
    }
}

/// The install prefix the harness reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The prefix.
    pub prefix: PathBuf,
}

impl Layout {
    /// `<prefix>/store/<program>`.
    #[must_use]
    pub fn program_dir(&self, program: &str) -> PathBuf {
        self.prefix.join(STORE_DIR).join(program)
    }

    /// `<prefix>/store/<program>/current`.
    #[must_use]
    pub fn current_link(&self, program: &str) -> PathBuf {
        self.program_dir(program).join(CURRENT)
    }
}

/// atpkg's default prefix under `home` (aterm `crates/atpkg/src/platform/unix.rs`).
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("aterm")
            .join("pkg")
    } else {
        home.join(".local").join("share").join("aterm").join("pkg")
    }
}

/// aterm's config file (aterm `crates/atpkg/src/config.rs` `config_path`):
/// `$XDG_CONFIG_HOME/aterm/aterm.toml`, else `~/.config/aterm/aterm.toml`.
#[must_use]
pub fn config_path(env: &Env) -> Option<PathBuf> {
    if let Some(x) = &env.xdg_config_home {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    env.home()
        .map(|h| h.join(".config").join("aterm").join("aterm.toml"))
}

/// The prefix atpkg uses, as far as the harness can tell; a configuration it cannot read
/// with certainty is refused rather than guessed.
///
/// # Errors
/// [`StoreError::NoHome`], [`StoreError::Config`] or [`StoreError::Prefix`].
pub fn resolve(env: &Env) -> Result<Layout, StoreError> {
    let home = env.home().ok_or(StoreError::NoHome)?;
    let configured = match config_path(env) {
        Some(path) => match read_bounded(&path, 1024 * 1024) {
            Some(text) => {
                configured_prefix(&text).map_err(|why| StoreError::Config { path, why })?
            }
            None => None,
        },
        None => None,
    };
    let prefix = match configured.as_deref().map(str::trim) {
        None | Some("") => default_prefix(home),
        Some(value) => vet_prefix(value, home).map_err(|why| StoreError::Prefix {
            prefix: value.to_string(),
            why,
        })?,
    };
    Ok(Layout { prefix })
}

/// `[packages] prefix` in `aterm.toml`'s text, read conservatively: `Ok(None)` when it is
/// not set, `Ok(Some)` for a plain basic or literal string in the `[packages]` table, and
/// `Err` for any other spelling that might set it (a dotted key, an inline table, an
/// escape, a multi-line string, a second definition), so the harness never runs a binary
/// from a store atpkg does not use.
///
/// # Errors
/// What was found, for the refusal.
pub fn configured_prefix(text: &str) -> Result<Option<String>, &'static str> {
    let mut table: Option<String> = None;
    let mut multi: Option<&'static str> = None;
    let mut found: Option<String> = None;
    for raw in text.lines() {
        if let Some(delim) = multi {
            if raw.matches(delim).count() % 2 == 1 {
                multi = None;
            }
            continue;
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let opens = ["\"\"\"", "'''"]
            .into_iter()
            .find(|d| line.matches(d).count() % 2 == 1);
        if line.starts_with('[') {
            let header = line.split('#').next().unwrap_or_default().trim();
            let name = header
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            table = Some(name.to_string());
            multi = opens;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            multi = opens;
            continue;
        };
        let key = key.trim();
        let in_packages = table.as_deref() == Some("packages");
        let at_root = table.is_none();
        if in_packages && matches!(key, "prefix" | "\"prefix\"" | "'prefix'") {
            if opens.is_some() {
                return Err("a multi-line string");
            }
            if found.is_some() {
                return Err("two definitions");
            }
            found = Some(plain_string(value.trim())?);
        } else if at_root && key.starts_with("packages") && line.contains("prefix") {
            return Err("a dotted key or an inline table");
        }
        multi = opens;
    }
    Ok(found)
}

/// A TOML basic string with no escapes, or a literal string, then nothing but a comment.
fn plain_string(v: &str) -> Result<String, &'static str> {
    if v.starts_with("\"\"\"") || v.starts_with("'''") {
        return Err("a multi-line string");
    }
    let quote = match v.chars().next() {
        Some(q @ ('"' | '\'')) => q,
        _ => return Err("a value that is not a string"),
    };
    let rest = &v[1..];
    let Some(end) = rest.find(quote) else {
        return Err("an unterminated string");
    };
    let (content, tail) = (&rest[..end], rest[end + 1..].trim());
    if quote == '"' && content.contains('\\') {
        return Err("a string with escapes");
    }
    if !(tail.is_empty() || tail.starts_with('#')) {
        return Err("a string followed by more than a comment");
    }
    Ok(content.to_string())
}

/// The configured prefix as atpkg's `vet_prefix` (aterm `crates/atpkg/src/store.rs`) takes
/// one under `home`: absolute or `~/…`, no `..`, strictly inside `home`; and where atpkg
/// would fall back to its default (an ancestor inside `home` that is a symlink, not ours,
/// or writable by group or others), so does this. A prefix outside `home` is a shared
/// system store, which atpkg admits by rules this harness does not reproduce: refused.
///
/// # Errors
/// Why the prefix cannot be used.
pub fn vet_prefix(value: &str, home: &Path) -> Result<PathBuf, &'static str> {
    let p = if let Some(rest) = value.strip_prefix('~') {
        match rest.strip_prefix('/') {
            Some(tail) => home.join(tail),
            None if rest.is_empty() => home.to_path_buf(),
            None => return Err("a ~user path"),
        }
    } else if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        return Err("relative");
    };
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("a path through ..");
    }
    if !p.starts_with(home) {
        return Err(
            "outside the home directory (a shared system store this harness does not read)",
        );
    }
    if p == home {
        return Ok(default_prefix(home));
    }
    // SAFETY: getuid takes no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    for anc in p.ancestors().filter(|a| a.starts_with(home)) {
        if let Ok(m) = fs::symlink_metadata(anc) {
            let private = m.uid() == uid && m.mode() & 0o022 == 0;
            if m.file_type().is_symlink() || !private {
                return Ok(default_prefix(home));
            }
        }
    }
    Ok(p)
}

/// One installed vendor build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// Its version (its store build id).
    pub version: Version,
    /// `<prefix>/store/<program>/<build>`.
    pub dir: PathBuf,
    /// `<dir>/bin/<cmd>`: what the harness runs.
    pub exe: PathBuf,
    /// The environment its `<build>.shim-env` sidecar declares (`ShimEnv::from_lines`).
    pub env: Vec<(String, String)>,
}

/// The `current` link's target, read without following it further: the cheap fact the
/// update watch polls.
///
/// # Errors
/// [`StoreError::NotInstalled`] or [`StoreError::Link`].
pub fn current_target(layout: &Layout, program: &str) -> Result<PathBuf, StoreError> {
    let link = layout.current_link(program);
    let target = match fs::read_link(&link) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StoreError::NotInstalled { link });
        }
        Err(_) => {
            return Err(StoreError::Link {
                link,
                why: "is not a readable symlink",
            });
        }
    };
    Ok(if target.is_absolute() {
        target
    } else {
        layout.program_dir(program).join(target)
    })
}

/// The program's current build, checked: the link names a build directory of this program
/// in this store, its directory name is a vendor build id, its install record's version is
/// that id, its ready marker says `ok`, and `bin/<cmd>` is an executable regular file.
///
/// # Errors
/// The first check that fails.
pub fn current(layout: &Layout, program: &str, cmd: &str) -> Result<Build, StoreError> {
    let link = layout.current_link(program);
    let dir = current_target(layout, program)?;
    checked(layout, program, cmd, dir, &link)
}

/// The build at `dir` (`<prefix>/store/<program>/<build>`), checked as [`current`] checks
/// the one `current` names: a build the harness ran before, looked at again before it is
/// run again (atpkg may have removed it since).
///
/// # Errors
/// The first check that fails.
pub fn build_at(
    layout: &Layout,
    program: &str,
    cmd: &str,
    dir: &Path,
) -> Result<Build, StoreError> {
    checked(layout, program, cmd, dir.to_path_buf(), dir)
}

/// A store build directory name: decimal digits with no leading zero (`store.rs`
/// `parse_build_name`).
#[must_use]
pub fn parse_build_id(name: &str) -> Option<u64> {
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) && !name.starts_with('0'))
        .then(|| name.parse().ok())
        .flatten()
}

/// Whether a `.ready` marker admits its build on `running` (`store.rs` `ready_text_accepts`):
/// the first line is `ok`, and any `platform=` line matches `running` (`<arch>-<os>`).
#[must_use]
pub fn ready_text_accepts(text: &str, running: &str) -> bool {
    if text.lines().next().map(str::trim) != Some("ok") {
        return false;
    }
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("platform="))
        .map(str::trim)
        .find(|p| !p.is_empty())
        .is_none_or(|p| p == running)
}

fn checked(
    layout: &Layout,
    program: &str,
    cmd: &str,
    dir: PathBuf,
    link: &Path,
) -> Result<Build, StoreError> {
    let bad_link = |why| StoreError::Link {
        link: link.to_path_buf(),
        why,
    };
    let id = located(layout, program, &dir, link)?;
    let version = Version::from_build_id(id).ok_or_else(|| bad_link("names no vendor build"))?;
    let record = dir.with_extension("vendor");
    let text = read_sidecar(&record, 64 * 1024).ok_or_else(|| StoreError::Record {
        record: record.clone(),
        why: "is missing or unreadable",
    })?;
    let recorded = record_version(&text).ok_or_else(|| StoreError::Record {
        record: record.clone(),
        why: "names no version",
    })?;
    if Version::parse(&recorded) != Some(version) {
        return Err(StoreError::Record {
            record,
            why: "names another version than its build directory",
        });
    }
    let exe = complete(&dir, cmd)?;
    let env = read_sidecar(&dir.with_extension("shim-env"), 4 * 1024)
        .as_deref()
        .map(parse_shim_env)
        .unwrap_or_default();
    Ok(Build {
        version,
        dir,
        exe,
        env,
    })
}

/// The build id of `dir`, when it is a plain directory of `program`'s store named by a build
/// id (`link` is what named it, for the error).
fn located(layout: &Layout, program: &str, dir: &Path, link: &Path) -> Result<u64, StoreError> {
    let bad_link = |why| StoreError::Link {
        link: link.to_path_buf(),
        why,
    };
    let canon = |p: &Path| fs::canonicalize(p).ok();
    if dir.parent().and_then(canon) != canon(&layout.program_dir(program)) {
        return Err(bad_link("points outside the program's store directory"));
    }
    if !fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        return Err(bad_link("names a directory that is not a plain directory"));
    }
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| bad_link("names no build"))?;
    parse_build_id(name).ok_or_else(|| bad_link("names a directory that is not a build id"))
}

/// `<dir>/bin/<cmd>`, when the build's ready marker admits it on this platform and the binary
/// is an executable regular file.
fn complete(dir: &Path, cmd: &str) -> Result<PathBuf, StoreError> {
    let marker = dir.with_extension("ready");
    let running = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let ready = read_sidecar(&marker, 64 * 1024).is_some_and(|t| ready_text_accepts(&t, &running));
    if !ready {
        return Err(StoreError::NotReady { marker });
    }
    let bin = dir.join("bin");
    let exe = bin.join(cmd);
    if !fs::symlink_metadata(&bin).is_ok_and(|m| m.is_dir()) {
        return Err(StoreError::Binary {
            exe,
            why: "is missing",
        });
    }
    let meta = fs::symlink_metadata(&exe).map_err(|_| StoreError::Binary {
        exe: exe.clone(),
        why: "is missing",
    })?;
    if !meta.file_type().is_file() {
        return Err(StoreError::Binary {
            exe,
            why: "is not a regular file",
        });
    }
    if meta.permissions().mode() & 0o100 == 0 {
        return Err(StoreError::Binary {
            exe,
            why: "is not executable",
        });
    }
    Ok(exe)
}

/// One installed build of the harness itself (a Plain program: no install record; atpkg
/// orders its builds by build number, the dev commit's count).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnBuild {
    /// Its store build number.
    pub id: u64,
    /// `<prefix>/store/<program>/<id>/bin/<cmd>`.
    pub exe: PathBuf,
}

/// The harness program's current build, checked as [`current`] checks a vendor build but for
/// the install record a Plain program does not have: the link names a build directory of this
/// program in this store, its ready marker says `ok`, and `bin/<cmd>` is an executable regular
/// file.
///
/// # Errors
/// The first check that fails.
pub fn own_current(layout: &Layout, program: &str, cmd: &str) -> Result<OwnBuild, StoreError> {
    let link = layout.current_link(program);
    let dir = current_target(layout, program)?;
    let id = located(layout, program, &dir, &link)?;
    let exe = complete(&dir, cmd)?;
    Ok(OwnBuild { id, exe })
}

/// The build number of the harness image `me` (its canonical executable) when it is
/// `<prefix>/store/<program>/<id>/bin/<cmd>` of this store; `None` for an image run from
/// anywhere else (a checkout's target folder), which has no build to order a newer one against.
#[must_use]
pub fn own_build_of(layout: &Layout, program: &str, cmd: &str, me: &Path) -> Option<u64> {
    if me.file_name().and_then(|n| n.to_str()) != Some(cmd) {
        return None;
    }
    let bin = me.parent()?;
    if bin.file_name().and_then(|n| n.to_str()) != Some("bin") {
        return None;
    }
    let dir = bin.parent()?;
    located(layout, program, dir, dir).ok()
}

/// Names a shim never sets (atpkg `shim_env.rs` `NEVER_SET`).
const NEVER_SET: &[&str] = &[
    "PATH",
    "HOME",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "SHELL",
    "PWD",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PATHEXT",
    "COMSPEC",
    "SYSTEMROOT",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5OPT",
    "RUBYOPT",
];

/// Parse a `.shim-env` sidecar fail-closed (atpkg `shim_env.rs` `ShimEnv::from_lines`).
#[must_use]
pub fn parse_shim_env(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.len() > 8 {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in lines {
        if entry.len() > 256 {
            return Vec::new();
        }
        let Some((name, value)) = entry.split_once('=') else {
            return Vec::new();
        };
        let name_ok = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            && !name.as_bytes()[0].is_ascii_digit()
            && !NEVER_SET.contains(&name)
            && !name.starts_with("LD_")
            && !name.starts_with("DYLD_");
        let value_ok = !value.is_empty()
            && !value.chars().any(char::is_control)
            && value.trim() == value
            && !value.contains('"')
            && !value.contains('%');
        if !name_ok || !value_ok || out.iter().any(|(n, _)| n == name) {
            return Vec::new();
        }
        out.push((name.to_string(), value.to_string()));
    }
    out
}

/// Read a regular file of at most `max` bytes without blocking on FIFOs.
fn read_bounded(path: &Path, max: u64) -> Option<String> {
    read_bounded_opts(path, max, true)
}

/// Read a store sidecar (≤ `max` bytes, regular, never a symlink) from one open handle.
fn read_sidecar(path: &Path, max: u64) -> Option<String> {
    read_bounded_opts(path, max, false)
}

fn read_bounded_opts(path: &Path, max: u64, follow: bool) -> Option<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut flags = libc::O_NONBLOCK;
    if !follow {
        flags |= libc::O_NOFOLLOW;
    }
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    let mut text = String::new();
    f.take(max.saturating_add(1))
        .read_to_string(&mut text)
        .ok()?;
    (u64::try_from(text.len()).ok()? <= max).then_some(text)
}

/// The `version = "…"` of a store install record (aterm `upgrade_drive.rs` `store_version`).
#[must_use]
pub fn record_version(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.trim().strip_prefix("version")?.trim_start();
        let v = v
            .strip_prefix('=')?
            .trim()
            .strip_prefix('"')?
            .strip_suffix('"')?;
        (!v.is_empty() && !v.contains('"') && !v.chars().any(char::is_control))
            .then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_is_read_only_from_a_plain_packages_string() {
        assert_eq!(configured_prefix(""), Ok(None));
        assert_eq!(
            configured_prefix("# x\n[kitty]\nprefix = \"no\"\n"),
            Ok(None),
            "another table's prefix"
        );
        assert_eq!(
            configured_prefix("[packages]\nprefix = \"~/pkgs\" # mine\n"),
            Ok(Some("~/pkgs".to_string()))
        );
        assert_eq!(
            configured_prefix("[ packages ]\n  prefix='/x/y'\n[other]\n"),
            Ok(Some("/x/y".to_string()))
        );
        assert_eq!(
            configured_prefix("[packages.links]\nprefix = \"/no\"\n"),
            Ok(None),
            "a subtable"
        );
        assert_eq!(
            configured_prefix("t = \"\"\"\n[packages]\nprefix = \"/no\"\n\"\"\"\n"),
            Ok(None),
            "inside a multi-line string"
        );
        for bad in [
            "packages.prefix = \"/x\"\n",
            "packages = { prefix = \"/x\" }\n",
            "[packages]\nprefix = \"/a\\\\b\"\n",
            "[packages]\nprefix = \"\"\"/x\"\"\"\n",
            "[packages]\nprefix = \"\"\"#\"\"\"\n",
            "[packages]\nprefix = 3\n",
            "[packages]\nprefix = \"/x\" junk\n",
            "[packages]\nprefix = \"/x\"\nprefix = \"/y\"\n",
        ] {
            assert!(configured_prefix(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_prefix_outside_home_or_through_dots_is_refused() {
        let home = Path::new("/nonexistent-home/u");
        assert_eq!(vet_prefix("relative/x", home), Err("relative"));
        assert_eq!(vet_prefix("~bob/x", home), Err("a ~user path"));
        assert!(vet_prefix("/opt/aterm/pkg", home).is_err());
        assert!(vet_prefix("/nonexistent-home/u/../v", home).is_err());
        assert_eq!(vet_prefix("~", home), Ok(default_prefix(home)));
        assert_eq!(
            vet_prefix("~/pkgs", home),
            Ok(home.join("pkgs")),
            "no ancestor exists to disqualify it"
        );
    }

    #[test]
    fn the_record_version_is_read_like_aterm_reads_it() {
        let text =
            "schema = 1\nprogram = \"muse\"\nversion = \"1.4.13-R6996.1\"\nvendor = \"Meta\"\n";
        assert_eq!(record_version(text).as_deref(), Some("1.4.13-R6996.1"));
        assert_eq!(record_version("program = \"muse\"\n"), None);
        assert_eq!(record_version("version = \"\"\n"), None);
        assert_eq!(record_version("version = 1.4.13\n"), None);
        assert_eq!(record_version("version = \"1.4.\"extra\"\n"), None);
        assert_eq!(record_version("versions = \"1.4.13\"\n"), None);
    }

    #[test]
    fn build_ids_are_canonical_decimal_and_ready_markers_match_platform() {
        assert_eq!(
            parse_build_id("2001004013006996002"),
            Some(2_001_004_013_006_996_002)
        );
        for bad in [
            "",
            "0",
            "02001004013006996002",
            "+2001004013006996002",
            "12a",
        ] {
            assert_eq!(parse_build_id(bad), None, "{bad:?}");
        }
        assert!(ready_text_accepts(
            "ok\nplatform=aarch64-macos\ncontents=bin:muse\n",
            "aarch64-macos"
        ));
        assert!(ready_text_accepts("ok\n", "aarch64-macos"));
        assert!(!ready_text_accepts(
            "ok\nplatform=x86_64-linux\n",
            "aarch64-macos"
        ));
        assert!(!ready_text_accepts(
            "pending\nplatform=aarch64-macos\n",
            "aarch64-macos"
        ));
    }

    #[test]
    fn the_harness_finds_its_own_builds_by_number() {
        let root = std::env::temp_dir().join(format!("muse-alab-own-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let layout = Layout {
            prefix: root.clone(),
        };
        let program = layout.program_dir("muse-alab");
        for n in [1, 2] {
            let bin = program.join(n.to_string()).join("bin");
            fs::create_dir_all(&bin).expect("dir");
            fs::write(bin.join("muse-alab"), b"#!/bin/sh\n").expect("binary");
            fs::set_permissions(bin.join("muse-alab"), fs::Permissions::from_mode(0o755))
                .expect("mode");
            fs::write(program.join(format!("{n}.ready")), "ok\n").expect("ready");
        }
        std::os::unix::fs::symlink("2", program.join(CURRENT)).expect("current");
        assert_eq!(
            own_current(&layout, "muse-alab", "muse-alab"),
            Ok(OwnBuild {
                id: 2,
                exe: program.join("2/bin/muse-alab"),
            }),
            "a Plain program's build has no install record"
        );
        let first = program.join("1/bin/muse-alab");
        assert_eq!(
            own_build_of(&layout, "muse-alab", "muse-alab", &first),
            Some(1)
        );
        assert_eq!(
            own_build_of(
                &layout,
                "muse-alab",
                "muse-alab",
                &root.join("target/debug/muse-alab")
            ),
            None,
            "an image outside the store"
        );
        assert_eq!(
            own_build_of(&layout, "muse-alab", "muse", &first),
            None,
            "another program's name"
        );
        fs::remove_file(program.join("2.ready")).expect("unready");
        assert!(
            matches!(
                own_current(&layout, "muse-alab", "muse-alab"),
                Err(StoreError::NotReady { .. })
            ),
            "an incomplete build"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn shim_env_is_admitted_fail_closed() {
        assert_eq!(
            parse_shim_env("DISABLE_UPDATES=1\n# comment\nMUSE_FLAG=true\n"),
            vec![
                ("DISABLE_UPDATES".to_string(), "1".to_string()),
                ("MUSE_FLAG".to_string(), "true".to_string()),
            ]
        );
        for bad in [
            "PATH=/tmp\n",
            "DYLD_INSERT_LIBRARIES=/x\n",
            "lower=1\n",
            "1A=1\n",
            "A=\n",
            "A=\"q\"\n",
            "A=100%\n",
            "A=1\nA=2\n",
        ] {
            assert!(parse_shim_env(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn sidecars_are_read_from_one_handle_and_refuse_symlinks_and_fifos() {
        let dir = std::env::temp_dir().join(format!("muse-alab-sidecar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("1.ready");
        fs::write(&file, "ok\n").expect("write");
        assert_eq!(read_sidecar(&file, 16).as_deref(), Some("ok\n"));
        assert_eq!(read_sidecar(&file, 2), None);
        let link = dir.join("2.ready");
        std::os::unix::fs::symlink(&file, &link).expect("symlink");
        assert_eq!(read_sidecar(&link, 16), None, "symlink sidecar refused");
        assert_eq!(read_bounded(&link, 16).as_deref(), Some("ok\n"));
        let fifo = dir.join("3.ready");
        let c_fifo =
            std::ffi::CString::new(std::os::unix::ffi::OsStrExt::as_bytes(fifo.as_os_str()))
                .expect("cstr");
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our private scratch directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(read_sidecar(&fifo, 16), None);
        assert_eq!(read_bounded(&fifo, 16), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
