// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! MUSE CODE'S RESUME GRAMMAR: which launches are the interactive TUI, and the argv that
//! brings a session back on a relaunch.
//!
//! Ported verbatim from aterm `crates/aterm-agent/src/harness/upgrade_muse.rs`
//! (5e2c6d0c28): `FLAGS`, `SUBCOMMANDS`, `read`, `rewrite_argv`, `is_tui_argv`,
//! `is_session_uuid` (with `codex_rollout::is_thread_id`) and `ArgvRefusal`
//! (`upgrade.rs`). The flag table was measured on `muse --help` 1.4.10-R6427.1; a flag a
//! newer Muse Code adds is unknown here, which refuses the relaunch (fail closed) rather
//! than guessing whether it takes a value.
//!
//! Every argv here starts with `argv[0]` (the program), as aterm's does.

use std::fmt;

/// Why an argv cannot be carried into a resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    /// A flag the table does not know (its arity is unknown).
    UnknownFlag(String),
    /// A flag or subcommand no resume can carry.
    NotResumable(String),
}

impl fmt::Display for ArgvRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgvRefusal::UnknownFlag(fl) => write!(f, "unknown-flag {fl}"),
            ArgvRefusal::NotResumable(fl) => write!(f, "not-resumable {fl}"),
        }
    }
}

/// How a root option takes its value (`muse --help`, 1.4.10, clap).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arity {
    /// A switch.
    None,
    /// Exactly one value: the next token whatever it looks like, or attached (`--model=x`).
    One,
}

/// What a relaunch does with a root option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fate {
    /// Carried verbatim.
    Keep,
    /// Dropped with its value (the session keeps it).
    Drop,
    /// The relaunch is refused.
    Refuse,
}

/// Muse Code's root options (`muse --help`, 1.4.10-R6427.1), each with its fate on a
/// relaunch.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--agents", Arity::One, Fate::Keep),
    ("--approval-judge", Arity::One, Fate::Drop),
    ("--approval-mode", Arity::One, Fate::Drop),
    ("--base-url", Arity::One, Fate::Keep),
    ("-c", Arity::One, Fate::Keep),
    ("--config", Arity::One, Fate::Keep),
    ("--contributor-mode", Arity::One, Fate::Keep),
    ("--disable-approval", Arity::None, Fate::Drop),
    ("--disable-request-user-input", Arity::None, Fate::Keep),
    ("--disable-sandbox", Arity::None, Fate::Drop),
    ("--disable-shell", Arity::None, Fate::Keep),
    ("--disable-write", Arity::None, Fate::Keep),
    ("--echo-delay-ms", Arity::One, Fate::Keep),
    ("--effort", Arity::One, Fate::Drop),
    ("--enable-shell-tool", Arity::None, Fate::Keep),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--image", Arity::One, Fate::Drop),
    ("--model", Arity::One, Fate::Drop),
    ("--no-session-log", Arity::None, Fate::Refuse),
    ("--no-parallel-tool-calls", Arity::None, Fate::Keep),
    ("--parallel-tool-calls", Arity::None, Fate::Keep),
    ("--permission-profile", Arity::One, Fate::Drop),
    ("--preset", Arity::One, Fate::Keep),
    ("--provider", Arity::One, Fate::Keep),
    ("--reasoning-effort", Arity::One, Fate::Drop),
    ("--sandbox-network", Arity::One, Fate::Drop),
    ("--subagent-worktree-isolation", Arity::None, Fate::Keep),
    ("--trust-workspace", Arity::None, Fate::Keep),
    ("-V", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::None, Fate::Refuse),
    ("--workspace", Arity::One, Fate::Refuse),
    ("--worktree", Arity::None, Fate::Refuse),
    ("--worktree-base", Arity::One, Fate::Refuse),
    ("--worktree-existing", Arity::One, Fate::Refuse),
    ("--yolo", Arity::None, Fate::Drop),
];

/// `resume`'s own option: the most recent session in the workspace, never typed (the
/// witness names the session).
const RESUME_LAST: &str = "--last";

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

/// Muse Code's subcommands (`muse --help`, 1.4.10, and clap's `help`). Only `resume` runs
/// the interactive TUI a relaunch brings back; `exec` is its one-shot face, the rest are
/// tools.
const SUBCOMMANDS: &[&str] = &[
    "resume",
    "exec",
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
    "init",
    "help",
];

/// Whether clap reads `tok` as an option: a leading `-` and more after it.
fn maybe_option(tok: &str) -> bool {
    tok.len() > 1 && tok.starts_with('-')
}

/// An option token's name and whether its value is attached (`--model=x`).
fn option_name(tok: &str) -> (&str, bool) {
    match tok.split_once('=') {
        Some((name, _)) if name.starts_with("--") => (name, true),
        _ => (tok, false),
    }
}

/// One launch's argv, read: the root options it carries and what it asked of `resume`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Read {
    kept: Vec<String>,
    resume: bool,
    reference: Option<String>,
}

fn read(argv: &[String]) -> Result<Read, ArgvRefusal> {
    let mut out = Read::default();
    let mut positional_seen = false;
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_str();
        i += 1;
        if tok == "--" {
            // Everything after is a positional: the first prompt, dropped.
            break;
        }
        if !maybe_option(tok) {
            if !positional_seen {
                positional_seen = true;
                if tok == "resume" {
                    out.resume = true;
                    continue;
                }
                if SUBCOMMANDS.contains(&tok) {
                    return Err(ArgvRefusal::NotResumable(tok.to_string()));
                }
                continue;
            }
            if out.resume && out.reference.is_none() {
                out.reference = Some(tok.to_string());
            }
            continue;
        }
        if out.resume && tok == RESUME_LAST {
            continue;
        }
        let (name, attached) = option_name(tok);
        let Some((arity, fate)) = flag(name) else {
            return Err(ArgvRefusal::UnknownFlag(name.to_string()));
        };
        let mut words = vec![tok.to_string()];
        if arity == Arity::One && !attached {
            let Some(value) = argv.get(i) else {
                return Err(ArgvRefusal::UnknownFlag(name.to_string()));
            };
            words.push(value.clone());
            i += 1;
        }
        match fate {
            Fate::Keep => out.kept.extend(words),
            Fate::Drop => {}
            Fate::Refuse => return Err(ArgvRefusal::NotResumable(name.to_string())),
        }
    }
    Ok(out)
}

/// Whether `argv` launches the interactive Muse Code TUI.
#[must_use]
pub fn is_tui_argv(argv: &[String]) -> bool {
    match read(argv) {
        Ok(_) => true,
        Err(ArgvRefusal::UnknownFlag(_)) => !one_shot_or_tool(argv),
        Err(ArgvRefusal::NotResumable(what)) => {
            !(SUBCOMMANDS.contains(&what.as_str()) || is_one_shot_flag(&what))
        }
    }
}

fn is_one_shot_flag(name: &str) -> bool {
    matches!(name, "-h" | "--help" | "-V" | "--version")
}

fn one_shot_or_tool(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .take_while(|t| t.as_str() != "--")
        .any(|t| {
            is_one_shot_flag(option_name(t).0)
                || (SUBCOMMANDS.contains(&t.as_str()) && t != "resume")
        })
}

/// The arguments (without `argv[0]`) that relaunch `argv` resuming `session`.
///
/// # Errors
/// [`ArgvRefusal`] for an unknown or non-resumable flag or subcommand, or a session that is
/// not a UUID.
pub fn rewrite_argv(argv: &[String], session: Option<&str>) -> Result<Vec<String>, ArgvRefusal> {
    let read = read(argv)?;
    let mut out = read.kept;
    if let Some(session) = session {
        if !is_session_uuid(session) {
            return Err(ArgvRefusal::NotResumable(format!("session-ref {session}")));
        }
        out.push("resume".to_string());
        out.push(session.to_string());
    }
    Ok(out)
}

/// The resume arguments `args` ([`rewrite_argv`] with a session) started on `model`: Muse Code's
/// root option `--model=<model>` (one value, attached so no model word is read as an option)
/// before the `resume` subcommand, as a launch names it. aterm names a model on a restart
/// (`restart … model=<word>`, a word `alab-wire` checked: ASCII, no leading `-`). The option is
/// one a relaunch otherwise drops, since the session keeps the model it ran (the flag table); a
/// later relaunch of this child drops it the same way, the session keeping the model it now
/// runs.
///
/// # Errors
/// The arguments name no `resume` to put the model before.
pub fn with_model(args: &[String], model: &str) -> Result<Vec<String>, ArgvRefusal> {
    let at = args
        .iter()
        .rposition(|word| word == "resume")
        .filter(|at| at + 2 == args.len())
        .ok_or_else(|| ArgvRefusal::NotResumable("model without a resume".to_string()))?;
    let mut out = args[..at].to_vec();
    out.push(format!("--model={model}"));
    out.extend_from_slice(&args[at..]);
    Ok(out)
}

/// Whether `s` is a hyphenated 8-4-4-4-12 hex UUID (a Muse Code session id).
#[must_use]
pub fn is_session_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    s.len() == 36
        && parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// A restart aterm names a model for starts Muse Code on it: `--model=<word>` among the root
    /// options, before `resume`, which the grammar reads back as the launch's model (and a later
    /// relaunch drops, as it drops every `--model`). A launch's own `--model` is gone already.
    #[test]
    fn a_model_restart_names_the_model_before_resume() {
        let launch = argv(&[STORE, "--trust-workspace", "--model", "m1", "hello"]);
        let resume = rewrite_argv(&launch, Some(S1)).unwrap();
        let moved = with_model(&resume, "lab/m2:latest").unwrap();
        assert_eq!(
            moved,
            argv(&["--trust-workspace", "--model=lab/m2:latest", "resume", S1])
        );
        let again: Vec<String> = std::iter::once(STORE.to_string()).chain(moved).collect();
        assert!(is_tui_argv(&again));
        assert_eq!(
            rewrite_argv(&again, Some(S1)).unwrap(),
            argv(&["--trust-workspace", "resume", S1])
        );
        assert!(with_model(&argv(&["--trust-workspace"]), "m2").is_err());
        assert!(with_model(&argv(&["resume", S1, "x"]), "m2").is_err());
    }

    const S1: &str = "01a148f8-9301-7752-96db-fef456f88074";
    // A store build under a home that names no real account (store.rs's tests use the same
    // `/nonexistent-home/u`); argv[0] is opaque to the rewrite.
    const STORE: &str = "/nonexistent-home/u/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";

    // aterm `upgrade_muse_tests.rs`, ported.
    #[test]
    fn a_relaunch_carries_the_provider_and_drops_what_the_session_keeps() {
        let launch = argv(&[
            STORE,
            "--base-url",
            "http://127.0.0.1:60955",
            "-c",
            "provider_retry.max_attempts=3",
            "--yolo",
            "--model",
            "lab-stub",
            "--reasoning-effort=xhigh",
            "--effort",
            "high",
            "--effort=low",
            "--approval-mode",
            "never",
            "--sandbox-network",
            "allow",
            "--disable-sandbox",
            "--disable-approval",
            "--approval-judge",
            "auto",
            "--trust-workspace",
            "resume",
            "my-session-name",
            "--permission-profile",
            "p1",
            "--provider=meta",
        ]);
        assert_eq!(
            rewrite_argv(&launch, Some(S1)),
            Ok(argv(&[
                "--base-url",
                "http://127.0.0.1:60955",
                "-c",
                "provider_retry.max_attempts=3",
                "--trust-workspace",
                "--provider=meta",
                "resume",
                S1,
            ]))
        );
        let prompt = argv(&[STORE, "--base-url", "u", "positional hello"]);
        assert_eq!(
            rewrite_argv(&prompt, Some(S1)),
            Ok(argv(&["--base-url", "u", "resume", S1]))
        );
        let last = argv(&[STORE, "resume", "--last", "--disable-shell"]);
        assert_eq!(
            rewrite_argv(&last, Some(S1)),
            Ok(argv(&["--disable-shell", "resume", S1]))
        );
        assert_eq!(rewrite_argv(&prompt, None), Ok(argv(&["--base-url", "u"])));
        let dashed = argv(&[STORE, "--", "--yolo", "resume"]);
        assert_eq!(rewrite_argv(&dashed, Some(S1)), Ok(argv(&["resume", S1])));
        let again = argv(&[STORE, "--trust-workspace", "resume", S1]);
        let other = "01a148f8-9301-7752-96db-fef456f88075";
        assert_eq!(
            rewrite_argv(&again, Some(other)),
            Ok(argv(&["--trust-workspace", "resume", other])),
            "a relaunch of a relaunch names the session the witness names"
        );
    }

    #[test]
    fn a_launch_no_resume_carries_is_refused() {
        for (words, why) in [
            (&[STORE, "--no-session-log"][..], "--no-session-log"),
            (&[STORE, "-w"], "-w"),
            (&[STORE, "--worktree", "create"], "--worktree"),
            (&[STORE, "--worktree-base", "HEAD"], "--worktree-base"),
            (&[STORE, "--workspace", "/w"], "--workspace"),
            (&[STORE, "exec", "hi"], "exec"),
            (&[STORE, "--base-url", "u", "login"], "login"),
            (&[STORE, "--help"], "--help"),
            (&[STORE, "resume", "-V"], "-V"),
        ] {
            assert_eq!(
                rewrite_argv(&argv(words), Some(S1)),
                Err(ArgvRefusal::NotResumable(why.to_string())),
                "{words:?}"
            );
        }
        assert_eq!(
            rewrite_argv(&argv(&[STORE, "--brand-new", "resume"]), Some(S1)),
            Err(ArgvRefusal::UnknownFlag("--brand-new".to_string()))
        );
        assert_eq!(
            rewrite_argv(&argv(&[STORE, "--model"]), Some(S1)),
            Err(ArgvRefusal::UnknownFlag("--model".to_string())),
            "a value-taking flag with no value"
        );
        assert!(matches!(
            rewrite_argv(&argv(&[STORE]), Some("my-session-name")),
            Err(ArgvRefusal::NotResumable(_))
        ));
    }

    #[test]
    fn only_the_interactive_tui_is_resumable() {
        for words in [
            &[STORE][..],
            &[STORE, "--base-url", "u", "resume", S1],
            &[STORE, "hello"],
            &[STORE, "--yolo", "resume"],
            &[STORE, "--brand-new", "resume"],
            &[STORE, "--no-session-log"],
        ] {
            assert!(is_tui_argv(&argv(words)), "{words:?}");
        }
        for words in [
            &[STORE, "exec", "hi"][..],
            &[STORE, "--base-url", "u", "exec"],
            &[STORE, "skills"],
            &[STORE, "--help"],
            &[STORE, "-V"],
            &[STORE, "--brand-new", "--version"],
            &[STORE, "--brand-new", "x", "login"],
            &[STORE, "help"],
        ] {
            assert!(!is_tui_argv(&argv(words)), "{words:?}");
        }
    }

    #[test]
    fn a_session_id_is_a_hyphenated_uuid() {
        assert!(is_session_uuid(S1));
        assert!(is_session_uuid("01A148F8-9301-7752-96DB-FEF456F88074"));
        for bad in [
            "",
            "my-session-name",
            "01a148f8-9301-7752-96db-fef456f8807",
            "01a148f8-9301-7752-96db-fef456f880744",
            "01a148f8x9301-7752-96db-fef456f88074",
            "01a148f8-9301-7752-96db-fef456f8807g",
        ] {
            assert!(!is_session_uuid(bad), "{bad:?}");
        }
    }
}
