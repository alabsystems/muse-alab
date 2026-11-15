// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE LAUNCHER'S BLOCKS FOR `muse`, RUN BY `muse-alab run`: this harness's half of aterm's
//! harness interface design §2.1, "The launcher keeps its other duties"
//! (`docs/DESIGN-harness-interface-2026-11-12.md`). Once `alab-harness.toml` leads the word
//! `muse`, atpkg's launcher keeps only its check that this harness's build is live and then runs
//! `exec <prefix>/bin/muse-alab run -- "$@"` (aterm `crates/atpkg/src/platform/mod.rs:659-690`,
//! the `exec … run -- "$@"` at line 686). Every other block the launcher runs for `muse` is
//! this harness's to run, in the launcher's order (`twin_prelude_with_launcher`,
//! `platform/mod.rs:609-640`), in its own process before Muse Code starts. Cited at aterm
//! `617e45d8c95`:
//!
//! 1. **The self-update intercept** (`sh_selfupdate_prelude`, `platform/mod.rs:970-1009`): none.
//!    The muse row's `self_update_verbs` is empty (`crates/atpkg/src/vendor_direct/table.rs:627`),
//!    so the launcher renders no block (`platform/mod.rs:976-984`) and `muse update` reaches
//!    Muse Code as typed. `run` does the same.
//! 2. **The sign-in gate** (`sh_signin_prelude`, `platform/mod.rs:720`): none. The row's
//!    `signin_gate` is `None` (`table.rs:678`), so the launcher renders none
//!    (`platform/mod.rs:622-633`).
//! 3. **The yolo default** ([`yolo`]): carried, byte for byte. The launcher's block
//!    (`sh_yolo_prelude_with_launcher`, `platform/mod.rs:1023-1459`) built from the row's
//!    `yolo_flags`, `yolo_markers`, `yolo_after` and `yolo_skip` (`table.rs:628-673`) and the
//!    `"muse"` arm of its word scan (`crates/atpkg/src/yolo.rs:149-152`), and its stderr line
//!    (`yolo.rs:10-13`). `tests/fixtures/atpkg-muse-yolo.sh` is that block as atpkg lays it, and
//!    a unit test holds [`yolo`] to it on every argv of a corpus, under each POSIX shell here.
//! 4. **The shim's environment** (`crates/atpkg/src/shim_env.rs`): none. The row's `shim_env`
//!    is empty (`table.rs:625`), and Muse Code runs from its store build's `bin/muse`, never
//!    through atpkg's shim.
//!
//! The launcher's `--no-harness` strip (`platform/mod.rs:1109-1123`) only sets
//! `ATERM_NO_HARNESS=1`; its yolo block then runs all the same (aterm's own test: `claude
//! --no-harness -p hello` runs `claude --dangerously-skip-permissions -p hello`,
//! `platform/mod_sh_shim_tests.rs:790-793`), and the variable decides only whether the harness
//! takes the launch (`platform/mod.rs:672`). So `run` does the same: it strips `--no-harness`
//! as the launcher does ([`crate::env::strip_no_harness`]), runs these blocks, and only then
//! reads whether the harness is off, which skips the supervision and nothing else (the design's
//! "`--no-harness` gets its own variable, which skips only the harness"). The design's list
//! says the yolo default stands "unless the launch says `--no-harness`"; the launcher's code
//! does otherwise, and the code is what a launch gets today. The launcher never hands `run` a
//! `--no-harness`, but it hands off every `ATERM_NO_HARNESS` other than `1`, which this harness
//! reads as off (any value but empty or `0`), so a `run` under `ATERM_NO_HARNESS=true` still
//! gets the default it gets today.
//!
//! The usage tap (`crates/atpkg/src/launch_tap.rs`) is Claude Code's alone (`PROGRAMS`, line 14),
//! and Muse Code takes no account token from the launcher, so neither is a block here.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;

/// The word atpkg's launcher runs this harness with: `muse-alab run -- <muse arguments>`. Muse
/// Code has no `run` subcommand (`crate::muse::argv`'s `SUBCOMMANDS`), so a first word `run` is
/// always this harness's.
pub const RUN: &str = "run";

/// The literal that ends the launcher's `run` and starts Muse Code's own words.
pub const DASHES: &str = "--";

/// The vendor word the launcher's stderr line names (`sh_quote_str(program)`).
const PROGRAM: &str = crate::muse::PROGRAM;

/// The word the launcher's `--yolo` alias claims (`yolo.rs:7`).
const WORD: &str = "--yolo";

/// What a by-the-flag stderr line starts with (`yolo.rs:10`).
const ECHO_PREFIX: &str = "aterm: --yolo: running ";

/// What a by-default stderr line starts with (`yolo.rs:13`).
const DEFAULT_ECHO_PREFIX: &str = "aterm: yolo (default): running ";

/// The flags yolo means for Muse Code, in the launcher's order (`table.rs:628-633`).
pub const FLAGS: [&str; 4] = [
    "--disable-approval",
    "--disable-sandbox",
    "--trust-workspace",
    "--disable-request-user-input",
];

/// The tokens that already decide a launch's permission posture (`table.rs:634-652`). A
/// marker ending in `=`, or a two-letter short option, matches every token it starts; any
/// other matches itself and its `=value` form; one of [`FLAGS`] matches only itself
/// (`platform/mod.rs:1141-1170`).
const MARKERS: [&str; 17] = [
    "--disable-approval",
    "--disable-sandbox",
    "--trust-workspace",
    "--disable-request-user-input",
    "--approval-mode",
    "--approval-mode=",
    "--permission-profile",
    "--permission-profile=",
    "--approval-judge",
    "--approval-judge=",
    "--sandbox-network",
    "--sandbox-network=",
    "--disable-write",
    "--disable-shell",
    "--config",
    "--config=",
    "-c",
];

/// First words the default flags go behind rather than in front of (`table.rs:653`).
const AFTER: [&str; 2] = ["exec", "resume"];

/// First words the default never lands on (`table.rs:654-673`; the row's self-update verbs,
/// which the launcher adds, are none).
const SKIP: [&str; 18] = [
    "resume",
    "config",
    "export",
    "trace",
    "model-profile",
    "skills",
    "voice",
    "plugins",
    "sandbox",
    "schema",
    "serve",
    "session-message",
    "mcp",
    "auth",
    "login",
    "logout",
    "usage",
    "init",
];

/// Leading options that take the next token as their value (`yolo.rs:149-152`).
const VALUES: [&str; 1] = ["--model"];

/// What the command line asks of the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `run -- <muse arguments>`: atpkg's launcher handing the word `muse` to this harness. The
    /// arguments are Muse Code's, verbatim, and the launcher's blocks run on them first.
    Run(Vec<OsString>),
    /// `muse-alab <muse arguments>`: the harness typed by its own name, every argument passed
    /// through as before, with no launcher block.
    Direct(Vec<OsString>),
}

/// Read the command line (without the program name).
///
/// # Errors
/// A first word `run` without the launcher's `--` right after it: no launcher writes that, and
/// Muse Code would refuse the word.
pub fn read(args: Vec<OsString>) -> Result<Command, String> {
    if args.first().is_none_or(|word| word != RUN) {
        return Ok(Command::Direct(args));
    }
    let mut words = args.into_iter().skip(1);
    if words.next().is_none_or(|word| word != DASHES) {
        return Err(format!(
            "`{RUN}` is atpkg's launcher's form, `{RUN} {DASHES} <{PROGRAM} arguments>`"
        ));
    }
    Ok(Command::Run(words.collect()))
}

/// What the launcher's yolo block makes of a launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Yolo {
    /// The arguments Muse Code runs with.
    pub args: Vec<OsString>,
    /// The line the block prints on stderr first, its newline included: the command as run.
    /// `None` for a launch that runs as typed.
    pub echo: Option<Vec<u8>>,
}

/// THE LAUNCHER'S YOLO BLOCK for `muse` (`platform/mod.rs:1023-1459`), byte for byte: the
/// arguments it hands Muse Code and the line it prints. Reading the words ahead of a literal
/// `--`:
///
/// - a typed `--yolo`, with nothing the word scan cannot read ahead of the first word, becomes
///   [`FLAGS`] (each one not already typed) in its place, or right behind the first word when
///   that is `exec` or `resume`; every `--yolo` ahead of the `--` goes;
/// - else a token that decides the posture ([`MARKERS`]), a first word the default skips, an
///   option the scan does not know ahead of the first word, or `--model` with no value, runs as
///   typed;
/// - else [`FLAGS`] go in front, or right behind a first word `exec`.
#[must_use]
pub fn yolo(args: Vec<OsString>) -> Yolo {
    match decide(&args) {
        Decision::AsTyped => Yolo { args, echo: None },
        Decision::Flag(out) => Yolo {
            echo: Some(echo(ECHO_PREFIX, &out)),
            args: out,
        },
        Decision::Default(out) => Yolo {
            echo: Some(echo(DEFAULT_ECHO_PREFIX, &out)),
            args: out,
        },
    }
}

/// The block's verdict (aterm's `yolo::Launch`).
enum Decision {
    /// The argv as typed, nothing printed.
    AsTyped,
    /// `--yolo` was typed.
    Flag(Vec<OsString>),
    /// Nothing decided the posture.
    Default(Vec<OsString>),
}

/// Whether the shell pattern the launcher makes of `marker` (`marker_pattern`,
/// `platform/mod.rs:1148-1160`) matches `token`: `'<marker>'*` for a marker ending in `=` or a
/// two-letter short option, else `'<marker>'|'<marker>='*`.
fn pattern_matches(marker: &str, token: &[u8]) -> bool {
    let m = marker.as_bytes();
    let short = m.len() == 2 && m.first() == Some(&b'-') && m.get(1) != Some(&b'-');
    if marker.ends_with('=') || short {
        return token.starts_with(m);
    }
    token == m
        || token
            .strip_prefix(m)
            .is_some_and(|rest| rest.first() == Some(&b'='))
}

/// The block's two scans and its rewrite, step for step.
fn decide(args: &[OsString]) -> Decision {
    let is = |token: &[u8], word: &str| token == word.as_bytes();
    // The posture scan: a typed `--yolo`, each of the flags typed, and any token that decides
    // the posture, ahead of a literal `--`.
    let mut typed = false;
    let mut posture = false;
    let mut present = [false; FLAGS.len()];
    for token in args.iter().map(|a| a.as_bytes()) {
        if is(token, DASHES) {
            break;
        }
        if is(token, WORD) {
            typed = true;
        } else if let Some(i) = FLAGS.iter().position(|flag| is(token, flag)) {
            if let Some(slot) = present.get_mut(i) {
                *slot = true;
            }
            posture = true;
        } else if MARKERS
            .iter()
            .filter(|marker| !FLAGS.contains(marker))
            .any(|marker| pattern_matches(marker, token))
        {
            posture = true;
        }
    }
    // The word scan: the first word's place past known leading options and their values, or why
    // there is none to read (`word_ok` false).
    let mut word: Option<(usize, &[u8])> = None;
    let mut word_ok = true;
    let mut value = false;
    for (i, token) in args.iter().map(|a| a.as_bytes()).enumerate() {
        if is(token, DASHES) {
            break;
        }
        if value {
            value = false;
            continue;
        }
        if is(token, WORD) || FLAGS.iter().any(|flag| is(token, flag)) {
            continue;
        }
        if VALUES.iter().any(|option| is(token, option)) {
            value = true;
            continue;
        }
        if VALUES.iter().any(|option| pattern_matches(option, token)) {
            continue;
        }
        if token.first() == Some(&b'-') {
            word_ok = false;
            break;
        }
        word = Some((i, token));
        break;
    }
    if value {
        word_ok = false;
    }
    let after = word.is_some_and(|(_, w)| AFTER.iter().any(|a| is(w, a)));
    let before_word = |i: usize| word.is_some_and(|(at, _)| i < at);
    let at_word = |i: usize| word.is_some_and(|(at, _)| i == at);
    let mut out: Vec<OsString> = Vec::with_capacity(args.len() + FLAGS.len());
    if typed && word_ok {
        // Each flag not yet typed, once, where the first `--yolo` (or the first word) puts it.
        let fresh = |out: &mut Vec<OsString>, present: &mut [bool; FLAGS.len()]| {
            for (flag, seen) in FLAGS.iter().zip(present.iter_mut()) {
                if !*seen {
                    *seen = true;
                    out.push(OsString::from(flag));
                }
            }
        };
        let mut stop = false;
        let mut pending = false;
        for (i, arg) in args.iter().enumerate() {
            if !stop && is(arg.as_bytes(), WORD) {
                if after && before_word(i) {
                    pending = true;
                } else {
                    fresh(&mut out, &mut present);
                }
            } else {
                if is(arg.as_bytes(), DASHES) {
                    stop = true;
                }
                out.push(arg.clone());
                if pending && at_word(i) {
                    pending = false;
                    fresh(&mut out, &mut present);
                }
            }
        }
        return Decision::Flag(out);
    }
    if posture || !word_ok || word.is_some_and(|(_, w)| SKIP.iter().any(|s| is(w, s))) {
        return Decision::AsTyped;
    }
    if after {
        for (i, arg) in args.iter().enumerate() {
            out.push(arg.clone());
            if at_word(i) {
                out.extend(FLAGS.iter().map(OsString::from));
            }
        }
    } else {
        out.extend(FLAGS.iter().map(OsString::from));
        out.extend(args.iter().cloned());
    }
    Decision::Default(out)
}

/// The block's stderr line: `prefix`, the program, then each argument, each in single quotes
/// with a `'` spelled `'\''`, and a newline (`platform/mod.rs:1443-1457`).
fn echo(prefix: &str, args: &[OsString]) -> Vec<u8> {
    let quote = |line: &mut Vec<u8>, word: &[u8]| {
        line.push(b'\'');
        for &byte in word {
            if byte == b'\'' {
                line.extend_from_slice(b"'\\''");
            } else {
                line.push(byte);
            }
        }
        line.push(b'\'');
    };
    let mut line = prefix.as_bytes().to_vec();
    quote(&mut line, PROGRAM.as_bytes());
    for arg in args {
        line.push(b' ');
        quote(&mut line, arg.as_bytes());
    }
    line.push(b'\n');
    line
}

#[cfg(test)]
#[path = "launcher_tests.rs"]
mod tests;
