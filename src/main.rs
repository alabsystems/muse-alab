// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `muse-alab`: run Muse Code from the atpkg store under the ALab harness.
//!
//! - `muse-alab --version` (the only argument) prints the harness's version and the
//!   installed vendor build's.
//! - `muse-alab --no-harness …` (or `ATERM_NO_HARNESS=1`) runs the store's `muse`
//!   directly, replacing this process: no supervision at all.
//! - Under aterm the harness supervises under a claim of both its duties (`alab-wire`, contract
//!   4 or 3, [`muse_alab::aterm`]), held or still being made, and otherwise runs the store's
//!   `muse` in its place the same way.
//! - Every other argument is passed to `muse` unchanged.
//! - `--alab-adopt <fd>` is this harness's own re-exec onto a newer build of itself, and
//!   `--alab-adopt-probe` its probe of that build ([`muse_alab::adopt`]); neither starts Muse
//!   Code.
//!
//! A harness started as a harness's vendor (the store's `muse` runs `muse-alab` again), or
//! nested [`muse_alab::env::MAX_DEPTH`] deep, refuses before anything else, so it can
//! never recurse into itself.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;

use alab_wire::Link;
use muse_alab::adopt;
use muse_alab::aterm;
use muse_alab::env::{Env, PARENT_VAR, nesting_depth, started_as_vendor, strip_no_harness};
use muse_alab::muse::{PACKAGE, PRODUCT, PROGRAM, argv};
use muse_alab::store;
use muse_alab::supervise::{Config, Harness};
use muse_alab::{HARNESS, say, vendor_version, version_line};

/// The exit status of a refusal to start: the vendor cannot be found (as a shell's
/// "command not found").
const NOT_FOUND: i32 = 127;

/// The exit status when the vendor binary exists but cannot be run.
const CANNOT_RUN: i32 = 126;

/// The exit status when a re-exec'd image cannot take its child over.
const NO_ADOPTION: i32 = 70;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == adopt::PROBE_ARG {
        // The probe reads and runs nothing: the image started, and names the state it reads.
        let _ = writeln!(std::io::stdout(), "{}", adopt::PROTOCOL);
        return;
    }
    let env = Env::from_process();
    let parent = std::os::unix::process::parent_id();
    if started_as_vendor(env.parent_mark.as_deref(), parent, std::process::id()) {
        refuse(
            NOT_FOUND,
            &format!(
                "refusing to run: started as {PRODUCT} by {HARNESS} {} ({PARENT_VAR}); the \
                 atpkg store's `{PROGRAM}` runs this harness again",
                env.parent_mark
                    .as_deref()
                    .map(|m| m.to_string_lossy())
                    .unwrap_or_default()
            ),
        );
    }
    let depth = match nesting_depth(env.depth_mark.as_deref()) {
        Ok(d) => d,
        Err(why) => refuse(NOT_FOUND, &format!("refusing to run: {why}")),
    };
    if args.len() == 2 && args[0] == adopt::ARG {
        adopt_child(env, depth, &args[1]);
    }
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        // The store is read, never the vendor run: the version is the current build's
        // `.vendor` record, checked against its directory, or a word saying why there is none.
        let current =
            store::resolve(&env).and_then(|layout| store::current(&layout, PACKAGE, PROGRAM));
        let vendor = vendor_version(&current);
        let line = version_line(vendor.as_deref().map_err(|why| *why));
        // A closed stdout is ignored (`println!` would panic).
        let _ = writeln!(std::io::stdout(), "{line}");
        return;
    }
    let (no_harness_word, args) = strip_no_harness(args);
    let layout = match store::resolve(&env) {
        Ok(l) => l,
        Err(e) => refuse(
            NOT_FOUND,
            &format!("cannot tell which atpkg store to read: {e}"),
        ),
    };
    let harness = Harness::new(env.clone(), layout, Config::for_this_build(), depth);
    let build = match harness.vendor() {
        Ok(b) => b,
        Err(e) => refuse(
            NOT_FOUND,
            &format!("{e}. Install it with `aterm pkg install {PACKAGE}`."),
        ),
    };
    if no_harness_word || env.no_harness {
        pass_through(&harness, &build, &args);
    }
    // Under aterm: the start sequence of aterm's harness design §3.2, through alab-wire, to a
    // claim of both duties, or Muse Code in this process's place (a launch that is not the
    // interactive TUI, no session to locate, no roster row under contract 3, a claim aterm did
    // not prove or refused, or a grant of less than both duties, which is given back). An
    // aterm that answered the handshake and then answered late or `ERR` (a dialog up, a stall),
    // serves no contract this harness claims at, or has its policy switched off is busy or off
    // for now, not away: Muse Code runs supervised and the claim is made again every 5 s.
    let link = if aterm::hosted(&env, &aterm::config()) {
        match start_under_aterm(&harness, &env, &build, &args) {
            Ok(link) => link,
            Err(why) => {
                harness.journal().note(
                    "PASS",
                    &format!(
                        "{PROGRAM} {} runs in the harness's place (exec): {why}",
                        build.version
                    ),
                );
                pass_through(&harness, &build, &args);
            }
        }
    } else {
        Link::absent(aterm::config(), std::process::id())
    };
    harness.run(build, args, link)
}

/// The new image after this harness's own re-exec: the state the old image carried, and the
/// child it ran supervised on from here ([`Harness::adopt`]).
fn adopt_child(env: Env, depth: u32, fd: &std::ffi::OsStr) -> ! {
    let carried = match adopt::receive(fd) {
        Ok(c) => c,
        Err(why) => {
            say(&format!(
                "{HARNESS}: cannot take {PRODUCT} over from its previous build: {why}"
            ));
            std::process::exit(NO_ADOPTION)
        }
    };
    let layout = store::Layout {
        prefix: carried.prefix.clone(),
    };
    Harness::new(env, layout, Config::for_this_build(), depth).adopt(carried)
}

/// The start sequence's aterm steps for this process: a link that supervises (a claim of both
/// duties, or one still being made), or why Muse Code runs in the harness's place. The link's
/// notes are journalled as they are made.
fn start_under_aterm(
    harness: &Harness,
    env: &Env,
    build: &store::Build,
    args: &[OsString],
) -> Result<Link, String> {
    let launch: Option<Vec<String>> = std::iter::once(build.exe.clone().into_os_string())
        .chain(args.iter().cloned())
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok();
    if !launch.as_deref().is_some_and(argv::is_tui_argv) {
        return Err("the launch is not Muse Code's interactive TUI".to_string());
    }
    let vendor = build.version.to_string();
    let mut link = Link::launch_with(
        aterm::config(),
        std::process::id(),
        aterm::child(Some(&vendor), None),
        aterm::now_ms(),
        aterm::lookup(env),
    );
    aterm::journal(&mut link, harness.journal());
    if link.is_absent() {
        return Err("aterm holds no claim of this harness's (the UNCLAIMED line says why)".into());
    }
    if link.judges() && !aterm::takes_both(&link) {
        let granted = aterm::granted(&link);
        link.release(aterm::now_ms());
        aterm::journal(&mut link, harness.journal());
        return Err(format!(
            "aterm granted scope={granted} (the claim needs {})",
            alab_wire::claim::scope_words(&aterm::SCOPES)
        ));
    }
    Ok(link)
}

/// Run the store's `muse` in this process's place with the harness's marks: the tab runs Muse
/// Code under its own name, as if no harness existed.
fn pass_through(harness: &Harness, build: &store::Build, args: &[OsString]) -> ! {
    let err = std::process::Command::new(&build.exe)
        .args(args)
        .envs(harness.child_marks())
        .exec();
    refuse(
        CANNOT_RUN,
        &format!("cannot run {}: {err}", build.exe.display()),
    );
}

fn refuse(code: i32, msg: &str) -> ! {
    say(&format!("{HARNESS}: {msg}"));
    say(&format!("{HARNESS}: {PRODUCT} was not started."));
    std::process::exit(code)
}
