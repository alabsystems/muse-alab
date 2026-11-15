// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;
use std::process::{Command as Process, Stdio};

use super::*;

/// atpkg's yolo block for `muse` as its launcher lays it (the fixture's header says whence).
const BLOCK: &str = include_str!("../tests/fixtures/atpkg-muse-yolo.sh");

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// `words` with [`FLAGS`] spliced in at `at`.
fn with_flags(words: &[&str], at: usize) -> Vec<OsString> {
    let mut out = os(words);
    out.splice(at..at, FLAGS.iter().map(OsString::from));
    out
}

#[test]
fn the_launchers_form_is_read_and_nothing_else() {
    assert_eq!(read(os(&["run", "--"])), Ok(Command::Run(vec![])));
    assert_eq!(
        read(os(&["run", "--", "--version"])),
        Ok(Command::Run(os(&["--version"])))
    );
    assert_eq!(
        read(os(&["run", "--", "--", "run", "--"])),
        Ok(Command::Run(os(&["--", "run", "--"]))),
        "only the launcher's own `--` is taken"
    );
    for refused in [&["run"][..], &["run", "x"], &["run", "--version", "--"]] {
        assert!(read(os(refused)).is_err(), "{refused:?}");
    }
    for direct in [&[][..], &["--trust-workspace", "run", "--"], &["--", "run"]] {
        assert_eq!(read(os(direct)), Ok(Command::Direct(os(direct))));
    }
}

/// aterm's own expectations of the block it lays for `muse` (aterm
/// `crates/atpkg/src/platform/mod_sh_shim_tests.rs:717-788`), and the stderr line, quotes
/// included.
#[test]
fn the_yolo_block_answers_aterms_own_muse_cases() {
    let behind = with_flags(&["exec", "x"], 1);
    for typed in [
        &["exec", "x"][..],
        &["--yolo", "exec", "x"],
        &["exec", "--yolo", "x"],
    ] {
        assert_eq!(yolo(os(typed)).args, behind, "{typed:?}");
    }
    for typed in [
        &["skills", "list"][..],
        &["--approval-mode", "never"],
        &["--new-option", "value", "--yolo", "exec", "x"],
        &["resume", "s"],
        &["--model"],
        &["--disable-sandbox", "x"],
        &["-cmodel=m"],
    ] {
        assert_eq!(
            yolo(os(typed)),
            Yolo {
                args: os(typed),
                echo: None
            },
            "{typed:?}"
        );
    }
    let flags = "'--disable-approval' '--disable-sandbox' '--trust-workspace' \
                 '--disable-request-user-input'";
    assert_eq!(
        yolo(vec![]),
        Yolo {
            args: with_flags(&[], 0),
            echo: Some(format!("aterm: yolo (default): running 'muse' {flags}\n").into_bytes()),
        }
    );
    let behind_model = with_flags(&["--model", "exec", "exec", "x"], 3);
    for typed in [
        &["--model", "exec", "exec", "x"][..],
        &["--yolo", "--model", "exec", "exec", "x"],
        &["--model", "exec", "--yolo", "exec", "x"],
    ] {
        assert_eq!(yolo(os(typed)).args, behind_model, "{typed:?}");
    }
    assert_eq!(
        yolo(os(&["--yolo", "it's", "--", "--yolo"])),
        Yolo {
            args: {
                let mut out = with_flags(&[], 0);
                out.extend(os(&["it's", "--", "--yolo"]));
                out
            },
            echo: Some(
                format!("aterm: --yolo: running 'muse' {flags} 'it'\\''s' '--' '--yolo'\n")
                    .into_bytes()
            ),
        }
    );
    assert_eq!(
        yolo(os(&["--trust-workspace", "--yolo", "x"])).args,
        os(&[
            "--trust-workspace",
            "--disable-approval",
            "--disable-sandbox",
            "--disable-request-user-input",
            "x"
        ]),
        "a flag already typed is not typed twice"
    );
}

/// One argument as a POSIX shell script spells it: single quotes, a `'` as `'\''`.
fn sh_word(word: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &byte in word {
        if byte == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(byte);
        }
    }
    out.push(b'\'');
    out
}

/// Tokens chosen to reach each branch of the block.
const BRANCHES: [&[u8]; 23] = [
    b"--yolo",
    b"--",
    b"exec",
    b"resume",
    b"skills",
    b"--model",
    b"--model=m",
    b"x",
    b"--disable-sandbox",
    b"--trust-workspace",
    b"--approval-mode",
    b"--approval-mode=never",
    b"-cfoo",
    b"-c",
    b"-",
    b"",
    b"it's",
    b"--new",
    b"--config=k=v",
    b"--disable-write",
    b"run",
    b"--disable-approval=1",
    b"\xff'",
];

/// EVERY WORD THE LAUNCHER'S BLOCK NAMES, spelled out here and never built from [`FLAGS`],
/// [`MARKERS`], [`AFTER`], [`SKIP`] or [`VALUES`], so a word those constants misspell or leave out
/// is caught against the block: each default flag bare and with a value (which matches no
/// pattern of the block's); each other posture marker bare and with a value, and a near miss its
/// pattern must not match; `--model` with and without its value; the first words the default
/// goes behind and those it skips; and words reaching the block's other branches (`update`,
/// which the muse row has no self-update verb for). [`the_table_names_every_word_of_the_block`]
/// holds this list to the fixture's `case` patterns.
const TABLE: [&[u8]; 57] = [
    b"--yolo",
    b"--yolo=1",
    b"--",
    b"--disable-approval",
    b"--disable-approval=1",
    b"--disable-sandbox",
    b"--disable-sandbox=1",
    b"--trust-workspace",
    b"--trust-workspace=1",
    b"--disable-request-user-input",
    b"--disable-request-user-input=1",
    b"--approval-mode",
    b"--approval-mode=",
    b"--approval-mode=never",
    b"--approval-modes",
    b"--permission-profile",
    b"--permission-profile=p",
    b"--approval-judge",
    b"--approval-judge=j",
    b"--sandbox-network",
    b"--sandbox-network=n",
    b"--disable-write",
    b"--disable-write=1",
    b"--disable-shell",
    b"--disable-shell=1",
    b"--config",
    b"--config=k=v",
    b"-c",
    b"-ck=v",
    b"--model",
    b"--model=m",
    b"exec",
    b"resume",
    b"config",
    b"export",
    b"trace",
    b"model-profile",
    b"skills",
    b"voice",
    b"plugins",
    b"sandbox",
    b"schema",
    b"serve",
    b"session-message",
    b"mcp",
    b"auth",
    b"login",
    b"logout",
    b"usage",
    b"init",
    b"update",
    b"run",
    b"x",
    b"-",
    b"",
    b"it's",
    b"\xff'",
];

/// `words` as arguments.
fn tokens(words: &[&[u8]]) -> Vec<OsString> {
    words
        .iter()
        .map(|word| OsString::from_vec(word.to_vec()))
        .collect()
}

/// Every argv of up to `depth` words over `tokens`, the empty one included.
fn every(tokens: &[OsString], depth: usize) -> Vec<Vec<OsString>> {
    let mut out: Vec<Vec<OsString>> = vec![vec![]];
    let mut last: Vec<Vec<OsString>> = vec![vec![]];
    for _ in 0..depth {
        let mut next = Vec::new();
        for prefix in &last {
            for token in tokens {
                let mut case = prefix.clone();
                case.push(token.clone());
                next.push(case);
            }
        }
        out.extend(next.iter().cloned());
        last = next;
    }
    out
}

/// `count` argvs of `shortest` to `shortest + 4` words drawn from `tokens` by a fixed generator.
fn drawn(tokens: &[OsString], count: usize, shortest: usize) -> Vec<Vec<OsString>> {
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut draw = |n: usize| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(seed >> 33).unwrap_or(0) % n
    };
    (0..count)
        .map(|_| {
            let len = shortest + draw(5);
            (0..len)
                .map(|_| tokens[draw(tokens.len())].clone())
                .collect()
        })
        .collect()
}

/// Every argv of up to three words over [`BRANCHES`] and 3,000 longer ones drawn from them;
/// every argv of up to two words over [`TABLE`] and 3,000 longer ones drawn from it.
fn corpus() -> Vec<Vec<OsString>> {
    let branches = tokens(&BRANCHES);
    let table = tokens(&TABLE);
    let mut out = every(&branches, 3);
    out.extend(drawn(&branches, 3000, 4));
    out.extend(every(&table, 2));
    out.extend(drawn(&table, 3000, 3));
    out
}

/// The literal words of the fixture's `case` patterns: each alternative of a line whose text
/// before its first `)` is only `'word'`, `word` or either with a trailing `*`, separated by `|`.
/// A word with a trailing `*` is a prefix (`true`).
fn block_words() -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for line in BLOCK.lines() {
        let Some((pattern, _)) = line.trim().split_once(')') else {
            continue;
        };
        let alternatives: Option<Vec<(String, bool)>> = pattern
            .split('|')
            .map(|alt| {
                let (alt, prefix) = alt
                    .strip_suffix('*')
                    .map_or((alt, false), |alt| (alt, true));
                let word = alt
                    .strip_prefix('\'')
                    .and_then(|w| w.strip_suffix('\''))
                    .unwrap_or(alt);
                word.bytes()
                    .all(|b| b == b'-' || b == b'=' || b.is_ascii_lowercase())
                    .then(|| (word.to_string(), prefix))
            })
            .collect();
        for (word, prefix) in alternatives.unwrap_or_default() {
            if !word.is_empty() && !out.contains(&(word.clone(), prefix)) {
                out.push((word, prefix));
            }
        }
    }
    out
}

/// [`TABLE`] holds every word the block's `case` patterns name: the word itself, and for a
/// prefix pattern a longer token it starts. So the byte-for-byte test meets each of them, and a
/// fixture taken from a later atpkg that names a new word fails here until the word is added.
#[test]
fn the_table_names_every_word_of_the_block() {
    let words = block_words();
    // 2 (`--`, `--yolo`) + 4 flags + 7 markers + their 7 `=` prefixes + `-c` + `--model` and its
    // `=` prefix + the `-` of `-*` + `exec`, `resume` + the 17 other skipped words.
    assert_eq!(words.len(), 43, "the fixture's case words: {words:?}");
    for (word, prefix) in &words {
        let word = word.as_bytes();
        if *prefix {
            assert!(
                TABLE
                    .iter()
                    .any(|t| t.len() > word.len() && t.starts_with(word)),
                "no token in TABLE extends the block's prefix {:?}",
                String::from_utf8_lossy(word)
            );
        } else {
            assert!(
                TABLE.contains(&word),
                "TABLE lacks the block's word {:?}",
                String::from_utf8_lossy(word)
            );
        }
    }
}

/// What `shell` makes of each argv of `cases` under the launcher's block: the stderr line (empty
/// for none) and the argv it leaves.
fn launcher(shell: &Path, cases: &[Vec<OsString>]) -> Vec<(Vec<u8>, Vec<OsString>)> {
    let mut script = b"__yolo() {\n".to_vec();
    script.extend_from_slice(BLOCK.as_bytes());
    script.extend_from_slice(
        b"printf '\\036'\nfor __w in \"$@\"; do printf '%s\\037' \"$__w\"; done\nprintf '\\035'\n}\n",
    );
    for case in cases {
        script.extend_from_slice(b"__yolo");
        for word in case {
            script.push(b' ');
            script.extend_from_slice(&sh_word(word.as_bytes()));
        }
        script.extend_from_slice(b" 2>&1\n");
    }
    let mut child = Process::new(shell)
        .arg("-s")
        .env_remove("ATERM_NO_HARNESS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the shell starts");
    let mut stdin = child.stdin.take().expect("its stdin");
    let feed = std::thread::spawn(move || stdin.write_all(&script));
    let out = child.wait_with_output().expect("the shell ends");
    feed.join()
        .expect("the feed")
        .expect("the script is written");
    assert!(
        out.status.success(),
        "{}: {:?}",
        shell.display(),
        out.status
    );
    let records: Vec<&[u8]> = out.stdout.split(|&b| b == 0x1d).collect();
    let (last, records) = records.split_last().expect("one record a case");
    assert!(last.is_empty(), "the output ends with a record");
    records
        .iter()
        .map(|record| {
            let at = record
                .iter()
                .position(|&b| b == 0x1e)
                .expect("a record's argv mark");
            let (echo, rest) = record.split_at(at);
            let words = rest.get(1..).unwrap_or_default();
            let mut args: Vec<OsString> = words
                .split(|&b| b == 0x1f)
                .map(|w| OsString::from_vec(w.to_vec()))
                .collect();
            assert_eq!(
                args.pop(),
                Some(OsString::new()),
                "each word ends with its mark"
            );
            (echo.to_vec(), args)
        })
        .collect()
}

/// THE BLOCK, BYTE FOR BYTE: on every argv of the corpus, under each POSIX shell here (the
/// launcher's `#!/bin/sh`, bash, and dash where installed), [`yolo`] leaves the argv the
/// launcher's block leaves and prints the line it prints. Its own result is a launch that has
/// decided its posture, so a harness launched by a launcher that still carries the block (the
/// two overlap until atpkg deletes it) never applies it twice.
#[test]
fn the_yolo_block_is_the_launchers_byte_for_byte() {
    let cases = corpus();
    let ours: Vec<(Vec<u8>, Vec<OsString>)> = cases
        .iter()
        .map(|case| {
            let made = yolo(case.clone());
            let again = yolo(made.args.clone());
            assert_eq!(
                again,
                Yolo {
                    args: made.args.clone(),
                    echo: None
                },
                "twice: {case:?}"
            );
            (made.echo.unwrap_or_default(), made.args)
        })
        .collect();
    let mut shells = 0;
    for shell in ["/bin/sh", "/bin/bash", "/bin/dash", "/usr/bin/dash"] {
        let shell = Path::new(shell);
        if !shell.exists() {
            continue;
        }
        shells += 1;
        let theirs = launcher(shell, &cases);
        assert_eq!(theirs.len(), cases.len(), "{}", shell.display());
        for ((case, mine), block) in cases.iter().zip(&ours).zip(&theirs) {
            assert_eq!(
                mine,
                block,
                "{}: {case:?}: ours {:?} / the launcher's {:?}",
                shell.display(),
                String::from_utf8_lossy(&mine.0),
                String::from_utf8_lossy(&block.0)
            );
        }
    }
    assert!(shells > 0, "no POSIX shell to run the launcher's block");
}
