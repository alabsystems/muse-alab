// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `muse-alab`: run Muse Code from the atpkg store under the ALab harness.
//!
//! - `muse-alab --version` (the only argument) prints the harness's version and the
//!   installed vendor build's.
//! - `muse-alab --no-harness …` (or `ATERM_NO_HARNESS=1`) runs the store's `muse`
//!   directly, replacing this process: no supervision at all.
//! - Under aterm the harness supervises only under a contract 3 claim aterm proved, and
//!   otherwise runs the store's `muse` in its place the same way ([`muse_alab::aterm`]).
//! - Every other argument is passed to `muse` unchanged.
//!
//! A harness started as a harness's vendor (the store's `muse` runs `muse-alab` again), or
//! nested [`muse_alab::env::MAX_DEPTH`] deep, refuses before anything else, so it can
//! never recurse into itself.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;

use muse_alab::aterm;
use muse_alab::env::{Env, PARENT_VAR, nesting_depth, started_as_vendor, strip_no_harness};
use muse_alab::link::Link;
use muse_alab::muse::{PACKAGE, PRODUCT, PROGRAM, argv};
use muse_alab::store;
use muse_alab::supervise::{Config, Harness};
use muse_alab::{HARNESS, HARNESS_VERSION, say, vendor_version, version_line};

/// The exit status of a refusal to start: the vendor cannot be found (as a shell's
/// "command not found").
const NOT_FOUND: i32 = 127;

/// The exit status when the vendor binary exists but cannot be run.
const CANNOT_RUN: i32 = 126;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
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
    // Under aterm: the start sequence of aterm's harness design §3.2, to a proven claim, or
    // Muse Code in this process's place (a launch that is not the interactive TUI, no aterm that
    // answers, no contract 3, a policy that is off, or a claim aterm did not prove).
    let claim = if env.under_aterm {
        match start_under_aterm(&env, &build, &args) {
            Ok(claim) => {
                harness.journal().note(
                    "CLAIMED",
                    &format!(
                        "{} holds the session under aterm contract {}, scope={} (aterm {})",
                        claim.holder,
                        aterm::REQUIRED_CONTRACT,
                        aterm::SCOPE,
                        claim.conn.version
                    ),
                );
                Some(claim)
            }
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
        None
    };
    harness.run(build, args, Link::new(claim))
}

/// The start sequence's aterm steps for this process: a proven claim, or why the harness gets
/// out of the way.
fn start_under_aterm(
    env: &Env,
    build: &store::Build,
    args: &[OsString],
) -> Result<aterm::Claimed, String> {
    let launch: Option<Vec<String>> = std::iter::once(build.exe.clone().into_os_string())
        .chain(args.iter().cloned())
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .ok();
    if !launch.as_deref().is_some_and(argv::is_tui_argv) {
        return Err("the launch is not Muse Code's interactive TUI".to_string());
    }
    let locator = aterm::Locator::from_env(env)?;
    aterm::claim(
        &locator,
        std::process::id(),
        HARNESS_VERSION,
        Some(&build.version.to_string()),
        aterm::TIMEOUT,
    )
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
