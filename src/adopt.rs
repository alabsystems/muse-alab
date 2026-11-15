// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HARNESS'S OWN UPDATE (aterm's harness design §4.5): when atpkg has installed a newer
//! build of the harness itself, the harness waits for the same safe point a move waits for and
//! `execve`s the new image in its own process: the same pid, the same child, which never
//! notices.
//!
//! - **Which build is newer.** The running image is `<prefix>/store/muse-alab/<n>/bin/muse-alab`
//!   (its canonical executable); a harness run from anywhere else has no build to order against
//!   and never re-execs. atpkg orders a Plain program's builds by build number (its dev commit's
//!   count), so `current` naming a larger `<n>` is a newer build ([`crate::store::own_current`]).
//! - **Proved first.** `<new exe> --alab-adopt-probe` must exit 0 within the probe bound and print
//!   exactly [`PROTOCOL`]: the new image starts, and reads the state this one writes.
//! - **What crosses** ([`Carried`]) is written as text into a pipe whose read end is the one
//!   descriptor the new image inherits (`--alab-adopt <fd>`): the child's pid, build and launch
//!   argv, when it started, the session it holds and that session's lock (the record path; no
//!   separate session record exists yet), the episode state (a move still in its startup grace,
//!   a child that outlived a `SIGTERM`, the crash ladder's streak and its last relaunch, the
//!   builds probed), the terminal's starting mode, the store prefix, and whether the old image
//!   had a link to aterm at all.
//! - **Signals.** The caught signals are blocked across the `execve` (their handlers revert to
//!   the default there) and collected by the new image ([`crate::signals::take_blocked`]); a
//!   `SIGHUP`, `SIGTERM` or `SIGCONT` already pending when the exec would start holds it.
//! - **aterm.** The claim's connections are close-on-exec, so aterm sees them close as it does
//!   on a restart; the new image claims again under the same pid with its own version, within
//!   aterm's adopted-claim grace (`harness_host.rs` `ADOPTED_CLAIM_GRACE`), reading the session
//!   from the environment `execve` kept (`alab_wire::Link::adopt_with`), and a claim it cannot
//!   make is made again every 5 s, as any lost claim is. An old image with no link (outside
//!   aterm, or a claim aterm refused for good) hands over none.

use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::FileTypeExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::probe::Probe;
use crate::store::Build;
use crate::tty::Mode;
use crate::version::Version;

/// The argument the new image runs with: `--alab-adopt <fd>`.
pub const ARG: &str = "--alab-adopt";

/// The probe's argument: the image prints [`PROTOCOL`] and exits 0, touching nothing else.
pub const PROBE_ARG: &str = "--alab-adopt-probe";

/// The first line of the carried state, and the probe's whole answer: the format's name and
/// version. A build that reads another format answers otherwise and is not re-exec'd into.
pub const PROTOCOL: &str = "muse-alab-adopt 2";

/// The most carried state the new image reads.
const MAX_BYTES: u64 = 64 * 1024;

/// What crosses the re-exec.
#[derive(Debug)]
pub struct Carried {
    /// The version of the harness that handed over.
    pub from: String,
    /// The atpkg prefix the old image read.
    pub prefix: PathBuf,
    /// The child's pid.
    pub child: u32,
    /// The vendor build it runs.
    pub build: Build,
    /// Its arguments, without `argv[0]`.
    pub args: Vec<OsString>,
    /// When it was started (ms since the epoch).
    pub started_ms: u64,
    /// The session it holds and its lock, once witnessed.
    pub lock: Option<(String, PathBuf)>,
    /// The build and session it moved from, and how long ago, while a move's startup grace
    /// may still be running.
    pub moved: Option<(Build, String, Duration)>,
    /// It outlived a `SIGTERM`.
    pub term_ignored: bool,
    /// The crash ladder ([`alab_wire::relaunch::Ladder`]): how many crash relaunches stand
    /// unforgiven, and how long ago the last one was made, when one was.
    pub ladder: (u32, Option<Duration>),
    /// The vendor builds probed: what was found, how long ago, and whether no probe can
    /// re-prove it.
    pub proofs: Vec<(Version, Probe, Duration, bool)>,
    /// The terminal's mode when the first image started.
    pub mode: Option<Mode>,
    /// The old image had a link to aterm (a claim held or being made again): the new image
    /// claims again. Without one, it makes none.
    pub linked: bool,
}

/// One field: every byte outside a plain set as `%XX`, so a field holds no space, no newline
/// and no `%` of its own.
fn pct(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"/._-+:@,".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() {
        // An empty field is a lone `%`, so a line keeps its field count.
        out.push('%');
    }
    out
}

/// A field [`pct`] wrote.
fn unpct(field: &str) -> Result<Vec<u8>, String> {
    if field == "%" {
        return Ok(Vec::new());
    }
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = field
                .get(i + 1..i + 3)
                .ok_or_else(|| format!("a cut escape in {field:?}"))?;
            out.push(
                u8::from_str_radix(hex, 16).map_err(|_| format!("a bad escape in {field:?}"))?,
            );
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn text(field: &str) -> Result<String, String> {
    String::from_utf8(unpct(field)?).map_err(|_| format!("{field:?} is not UTF-8"))
}

fn path(field: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(OsString::from_vec(unpct(field)?)))
}

fn number<T: std::str::FromStr>(field: &str) -> Result<T, String> {
    field
        .parse()
        .map_err(|_| format!("{field:?} is not a number"))
}

fn ms(d: Duration) -> u128 {
    d.as_millis()
}

fn probe_word(p: Probe) -> &'static str {
    p.word()
}

fn probe_of(word: &str) -> Result<Probe, String> {
    [Probe::Starts, Probe::Exited, Probe::NoExec, Probe::TimedOut]
        .into_iter()
        .find(|p| p.word() == word)
        .ok_or_else(|| format!("{word:?} is no probe's word"))
}

fn build_line(key: &str, b: &Build, tail: &[String], out: &mut String) {
    out.push_str(&format!(
        "{key} {} {} {}",
        b.version.build_id(),
        pct(b.dir.as_os_str().as_bytes()),
        pct(b.exe.as_os_str().as_bytes())
    ));
    for t in tail {
        out.push(' ');
        out.push_str(t);
    }
    out.push('\n');
    for (k, v) in &b.env {
        out.push_str(&format!(
            "{key}-env {} {}\n",
            pct(k.as_bytes()),
            pct(v.as_bytes())
        ));
    }
}

fn build_of(f: &[&str]) -> Result<Build, String> {
    let [id, dir, exe, ..] = f else {
        return Err("a build line names no id, directory and binary".to_string());
    };
    let id: u64 = number(id)?;
    Ok(Build {
        version: Version::from_build_id(id).ok_or_else(|| format!("{id} is no vendor build"))?,
        dir: path(dir)?,
        exe: path(exe)?,
        env: Vec::new(),
    })
}

impl Carried {
    /// The state as the text the new image reads: [`PROTOCOL`], one `<key> <field>…` line per
    /// fact, then `end`.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut out = format!("{PROTOCOL}\n");
        out.push_str(&format!("from {}\n", pct(self.from.as_bytes())));
        out.push_str(&format!(
            "prefix {}\n",
            pct(self.prefix.as_os_str().as_bytes())
        ));
        out.push_str(&format!("child {}\n", self.child));
        build_line("build", &self.build, &[], &mut out);
        for a in &self.args {
            out.push_str(&format!("arg {}\n", pct(a.as_bytes())));
        }
        out.push_str(&format!("started {}\n", self.started_ms));
        if let Some((session, lock)) = &self.lock {
            out.push_str(&format!(
                "lock {} {}\n",
                pct(session.as_bytes()),
                pct(lock.as_os_str().as_bytes())
            ));
        }
        if let Some((prev, session, age)) = &self.moved {
            build_line(
                "moved",
                prev,
                &[pct(session.as_bytes()), ms(*age).to_string()],
                &mut out,
            );
        }
        if self.term_ignored {
            out.push_str("term-ignored\n");
        }
        out.push_str(&format!(
            "ladder {} {}\n",
            self.ladder.0,
            self.ladder
                .1
                .map_or_else(|| "-".to_string(), |since| ms(since).to_string())
        ));
        for (version, found, age, blind) in &self.proofs {
            out.push_str(&format!(
                "proof {} {} {} {}\n",
                version.build_id(),
                probe_word(*found),
                ms(*age),
                u8::from(*blind)
            ));
        }
        if let Some(mode) = &self.mode {
            out.push_str(&format!("mode {}\n", pct(mode.words().as_bytes())));
        }
        out.push_str(if self.linked {
            "link claims\n"
        } else {
            "link absent\n"
        });
        out.push_str("end\n");
        out
    }

    /// The state [`Carried::encode`] wrote: every line known, every fact once, and nothing
    /// after `end`.
    ///
    /// # Errors
    /// What is wrong with it.
    pub fn decode(text_in: &str) -> Result<Carried, String> {
        let mut lines = text_in.split('\n');
        if lines.next() != Some(PROTOCOL) {
            return Err(format!("it does not start {PROTOCOL:?}"));
        }
        let (mut from, mut prefix, mut child, mut build, mut started) =
            (None, None, None, None, None);
        let (mut args, mut lock, mut moved, mut term_ignored) = (Vec::new(), None, None, false);
        let (mut ladder, mut proofs, mut mode, mut linked) = (None, Vec::new(), None, None);
        let mut ended = false;
        for line in lines.by_ref() {
            if line == "end" {
                ended = true;
                break;
            }
            let mut words = line.split(' ');
            let key = words.next().unwrap_or_default();
            let f: Vec<&str> = words.collect();
            let once = |slot: bool| {
                if slot {
                    Err(format!("{key} twice"))
                } else {
                    Ok(())
                }
            };
            match (key, f.as_slice()) {
                ("from", [v]) => {
                    once(from.is_some())?;
                    from = Some(text(v)?);
                }
                ("prefix", [p]) => {
                    once(prefix.is_some())?;
                    prefix = Some(path(p)?);
                }
                ("child", [pid]) => {
                    once(child.is_some())?;
                    child = Some(number::<u32>(pid)?);
                }
                ("build", [_, _, _]) => {
                    once(build.is_some())?;
                    build = Some(build_of(&f)?);
                }
                ("build-env", [k, v]) => build
                    .as_mut()
                    .ok_or("build-env before build")?
                    .env
                    .push((text(k)?, text(v)?)),
                ("arg", [a]) => args.push(OsString::from_vec(unpct(a)?)),
                ("started", [t]) => {
                    once(started.is_some())?;
                    started = Some(number::<u64>(t)?);
                }
                ("lock", [s, p]) => {
                    once(lock.is_some())?;
                    lock = Some((text(s)?, path(p)?));
                }
                ("moved", [_, _, _, s, age]) => {
                    once(moved.is_some())?;
                    moved = Some((build_of(&f)?, text(s)?, Duration::from_millis(number(age)?)));
                }
                ("moved-env", [k, v]) => moved
                    .as_mut()
                    .ok_or("moved-env before moved")?
                    .0
                    .env
                    .push((text(k)?, text(v)?)),
                ("term-ignored", []) => {
                    once(term_ignored)?;
                    term_ignored = true;
                }
                ("ladder", [streak, since]) => {
                    once(ladder.is_some())?;
                    let since = match *since {
                        "-" => None,
                        ms => Some(Duration::from_millis(number(ms)?)),
                    };
                    ladder = Some((number::<u32>(streak)?, since));
                }
                ("proof", [id, word, age, blind]) => {
                    let id: u64 = number(id)?;
                    proofs.push((
                        Version::from_build_id(id)
                            .ok_or_else(|| format!("{id} is no vendor build"))?,
                        probe_of(word)?,
                        Duration::from_millis(number(age)?),
                        match *blind {
                            "0" => false,
                            "1" => true,
                            other => return Err(format!("blind={other:?}")),
                        },
                    ));
                }
                ("mode", [m]) => {
                    once(mode.is_some())?;
                    mode = Some(Mode::from_words(&text(m)?).ok_or("an unreadable terminal mode")?);
                }
                ("link", [word]) => {
                    once(linked.is_some())?;
                    linked = Some(match *word {
                        "claims" => true,
                        "absent" => false,
                        other => return Err(format!("link {other:?}")),
                    });
                }
                _ => return Err(format!("an unknown line {line:?}")),
            }
        }
        if !ended || lines.any(|l| !l.is_empty()) {
            return Err("it does not end with one `end`".to_string());
        }
        let need = |what: &str| format!("it names no {what}");
        Ok(Carried {
            from: from.ok_or_else(|| need("harness version"))?,
            prefix: prefix.ok_or_else(|| need("store prefix"))?,
            child: child.ok_or_else(|| need("child"))?,
            build: build.ok_or_else(|| need("build"))?,
            args,
            started_ms: started.ok_or_else(|| need("start time"))?,
            lock,
            moved,
            term_ignored,
            ladder: ladder.ok_or_else(|| need("crash ladder"))?,
            proofs,
            mode,
            linked: linked.ok_or_else(|| need("link"))?,
        })
    }
}

/// What the probe of a newer build of the harness found: it starts only when it exited 0 having
/// printed exactly [`PROTOCOL`].
#[must_use]
pub fn judge_probe(found: Probe, said: &[u8]) -> Probe {
    match found {
        Probe::Starts if said == format!("{PROTOCOL}\n").as_bytes() => Probe::Starts,
        Probe::Starts => Probe::Exited,
        other => other,
    }
}

/// Hand this process over to the harness image `exe`, `carried` crossing on an inherited pipe:
/// `exe --alab-adopt <fd>`, with this process's environment. The caller has blocked the caught
/// signals ([`crate::signals::block_caught`]). Returns only when the exec did not happen, with
/// why; nothing was handed over then, and this image carries on.
#[must_use]
pub fn exec_into(exe: &Path, carried: &Carried) -> String {
    let state = carried.encode();
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return format!("no pipe for its state: {}", std::io::Error::last_os_error());
    }
    // SAFETY: pipe(2) just returned these two descriptors, owned by nothing else.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // The write end is this image's alone (close-on-exec) and never blocks: a state larger than
    // the pipe holds is a re-exec that does not happen, never a hang.
    // SAFETY: fcntl on descriptors this function owns.
    let set = unsafe {
        let w = write.as_raw_fd();
        let fl = libc::fcntl(w, libc::F_GETFL);
        fl >= 0
            && libc::fcntl(w, libc::F_SETFL, fl | libc::O_NONBLOCK) >= 0
            && libc::fcntl(w, libc::F_SETFD, libc::FD_CLOEXEC) >= 0
            && libc::fcntl(read.as_raw_fd(), libc::F_SETFD, 0) >= 0
    };
    if !set {
        return format!("its state's pipe: {}", std::io::Error::last_os_error());
    }
    {
        use std::io::Write as _;
        let mut w = std::fs::File::from(write);
        if let Err(e) = w.write_all(state.as_bytes()) {
            return format!(
                "its state ({} bytes) did not fit the pipe: {e}",
                state.len()
            );
        }
    }
    let fd = read.as_raw_fd();
    let (Ok(c_exe), Ok(c_arg), Ok(c_fd)) = (
        CString::new(exe.as_os_str().as_bytes()),
        CString::new(ARG),
        CString::new(fd.to_string()),
    ) else {
        return format!("{} is not a path execve takes", exe.display());
    };
    let argv = [
        c_exe.as_ptr(),
        c_arg.as_ptr(),
        c_fd.as_ptr(),
        std::ptr::null(),
    ];
    // The read end must outlive the call: an exec that succeeds hands it over open.
    let raw = read.into_raw_fd();
    // SAFETY: `c_exe` and every `argv` entry are NUL-terminated strings alive for the call, and
    // `argv` ends with a null pointer; execv(3) keeps this process's environment. Not std's
    // `Command::exec`, which empties the signal mask before the exec.
    unsafe {
        libc::execv(c_exe.as_ptr(), argv.as_ptr());
    }
    let err = std::io::Error::last_os_error();
    // SAFETY: the exec did not happen, so the read end is still this process's own.
    drop(unsafe { OwnedFd::from_raw_fd(raw) });
    format!("execve {}: {err}", exe.display())
}

/// In the new image: the state the old one wrote into descriptor `fd_arg` (`--alab-adopt
/// <fd>`), read whole and the descriptor closed. A descriptor that is not a pipe is refused.
///
/// # Errors
/// Why there is no state to adopt.
pub fn receive(fd_arg: &OsStr) -> Result<Carried, String> {
    use std::io::Read as _;
    let fd: libc::c_int = fd_arg
        .to_str()
        .and_then(|s| s.parse().ok())
        .filter(|&fd| fd > 2)
        .ok_or_else(|| format!("{fd_arg:?} names no inherited descriptor"))?;
    // SAFETY: fcntl F_GETFD only queries the descriptor number.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Err(format!("descriptor {fd} is not open"));
    }
    // SAFETY: the descriptor is open (just checked), and nothing closes it while it is borrowed
    // here: its type is read through a duplicate, which std closes.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let is_pipe = borrowed
        .try_clone_to_owned()
        .ok()
        .map(std::fs::File::from)
        .and_then(|f| f.metadata().ok())
        .is_some_and(|m| m.file_type().is_fifo());
    if !is_pipe {
        return Err(format!("descriptor {fd} is not a pipe"));
    }
    // SAFETY: the old image handed this descriptor over to this process, and nothing else here
    // owns it; it is closed when the file drops.
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("its state is unreadable: {e}"))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(format!("its state is larger than {MAX_BYTES} bytes"));
    }
    Carried::decode(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(v: &str, dir: &str, env: &[(&str, &str)]) -> Build {
        Build {
            version: Version::parse(v).expect("a vendor version"),
            dir: PathBuf::from(dir),
            exe: PathBuf::from(dir).join("bin/muse"),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn mode_words() -> String {
        let cc: String = (0..libc::NCCS)
            .map(|i| format!("{:02x}", i * 7 % 256))
            .collect();
        format!("27394 3 19200 1483 9600 9600 {cc}")
    }

    fn sample() -> Carried {
        Carried {
            from: "0.1.0+gabcdef012".into(),
            prefix: PathBuf::from("/nonexistent-home/a b/Library/Application Support/aterm/pkg"),
            child: 4242,
            build: build(
                "1.4.13-R6996.1",
                "/p/store/muse/2001004013006996002",
                &[("DISABLE_UPDATES", "1")],
            ),
            args: vec![
                OsString::from("--trust-workspace"),
                OsString::from("a prompt with spaces, 100% and a\nnewline"),
                OsString::from(""),
                OsString::from_vec(vec![0xff, b'x']),
            ],
            started_ms: 1_762_900_000_123,
            lock: Some((
                "01a148f8-9301-7752-96db-fef456f88074".into(),
                PathBuf::from("/d/muse/sessions/2026/11/12/01a148f8/.session.lock"),
            )),
            moved: Some((
                build("1.4.10-R6000.0", "/p/store/muse/2001004010006000001", &[]),
                "01a148f8-9301-7752-96db-fef456f88074".into(),
                Duration::from_millis(3_500),
            )),
            term_ignored: true,
            ladder: (2, Some(Duration::from_millis(120_000))),
            proofs: vec![(
                Version::parse("1.4.14-R7000.0").expect("v"),
                Probe::TimedOut,
                Duration::from_millis(61_000),
                true,
            )],
            mode: Some(Mode::from_words(&mode_words()).expect("mode")),
            linked: true,
        }
    }

    #[test]
    fn the_state_crosses_whole() {
        let c = sample();
        let text = c.encode();
        assert!(text.starts_with(&format!("{PROTOCOL}\n")), "{text}");
        assert!(text.ends_with("\nend\n"), "{text}");
        let back = Carried::decode(&text).expect("decodes");
        assert_eq!(back.from, c.from);
        assert_eq!(back.prefix, c.prefix);
        assert_eq!(back.child, c.child);
        assert_eq!(back.build, c.build);
        assert_eq!(back.args, c.args);
        assert_eq!(back.started_ms, c.started_ms);
        assert_eq!(back.lock, c.lock);
        assert_eq!(back.moved, c.moved);
        assert!(back.term_ignored);
        assert_eq!(back.ladder, c.ladder);
        assert_eq!(back.proofs, c.proofs);
        assert_eq!(back.mode.map(|m| m.words()), Some(mode_words()));
        assert!(back.linked);
        // Nothing optional, nothing carried.
        let bare = Carried {
            lock: None,
            moved: None,
            term_ignored: false,
            ladder: (0, None),
            proofs: Vec::new(),
            mode: None,
            linked: false,
            args: Vec::new(),
            ..sample()
        };
        let back = Carried::decode(&bare.encode()).expect("decodes");
        assert!(back.lock.is_none() && back.moved.is_none() && back.mode.is_none());
        assert!(!back.linked && back.args.is_empty() && !back.term_ignored);
        assert_eq!(back.ladder, (0, None));
    }

    #[test]
    fn a_state_that_is_not_whole_is_refused() {
        let text = sample().encode();
        let cut = text.trim_end_matches("end\n");
        assert!(Carried::decode(cut).is_err(), "no end");
        assert!(Carried::decode(&text.replacen(PROTOCOL, "muse-alab-adopt 1", 1)).is_err());
        assert!(
            Carried::decode(&text.replacen("ladder 2 120000\n", "", 1)).is_err(),
            "no ladder: an old image's crash lines are not read as none"
        );
        assert!(
            Carried::decode(&text.replacen("ladder 2 120000\n", "crash 5\n", 1)).is_err(),
            "protocol 1's crash line"
        );
        assert!(
            Carried::decode(&text.replacen("link claims\n", "", 1)).is_err(),
            "no link"
        );
        assert!(
            Carried::decode(&text.replacen("link claims\n", "link maybe\n", 1)).is_err(),
            "an unknown link"
        );
        assert!(
            Carried::decode(&format!("{text}child 1\n")).is_err(),
            "after end"
        );
        assert!(
            Carried::decode(&text.replacen("child 4242\n", "child 4242\nchild 7\n", 1)).is_err(),
            "twice"
        );
        assert!(
            Carried::decode(&text.replacen("child 4242\n", "", 1)).is_err(),
            "no child"
        );
        assert!(
            Carried::decode(&text.replacen("started ", "begun ", 1)).is_err(),
            "unknown line"
        );
        assert!(
            Carried::decode(&text.replacen("arg --trust", "arg %G-trust", 1)).is_err(),
            "a bad escape"
        );
    }

    #[test]
    fn only_a_build_that_speaks_this_protocol_is_proved() {
        let line = format!("{PROTOCOL}\n");
        assert_eq!(judge_probe(Probe::Starts, line.as_bytes()), Probe::Starts);
        assert_eq!(
            judge_probe(Probe::Starts, b"muse-alab-adopt 1\n"),
            Probe::Exited,
            "protocol 1 carried crash times and aterm's locator, not the ladder"
        );
        assert_eq!(
            judge_probe(Probe::Starts, b"muse-alab 0.1.0 (muse 1.4.13)\n"),
            Probe::Exited,
            "a build that does not know the probe runs as a harness and says otherwise"
        );
        assert_eq!(
            judge_probe(Probe::TimedOut, line.as_bytes()),
            Probe::TimedOut
        );
    }

    #[test]
    fn the_state_is_received_only_from_an_inherited_pipe() {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors pipe(2) writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: pipe(2) just returned these, owned by nothing else.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        {
            use std::io::Write as _;
            std::fs::File::from(write)
                .write_all(sample().encode().as_bytes())
                .expect("write");
        }
        let fd = read.into_raw_fd();
        let got = receive(OsStr::new(&fd.to_string())).expect("received");
        assert_eq!(got.child, 4242);
        assert!(receive(OsStr::new("1")).is_err(), "a standard descriptor");
        assert!(receive(OsStr::new("x")).is_err());
        let file = std::fs::File::open("/dev/null").expect("open");
        let n = file.as_raw_fd();
        assert!(receive(OsStr::new(&n.to_string())).is_err(), "not a pipe");
    }
}
