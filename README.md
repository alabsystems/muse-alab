# muse-alab

The ALab harness for Muse Code. `muse-alab` runs the **unmodified** `muse` command-line
program from aterm's package store as its child, on your terminal, with every argument
passed through. When aterm's package manager installs a newer Muse Code build while a
session is running, the harness waits for a safe point and moves the session onto the new
build — the same session, resumed with Muse Code's own `resume` argv — without the harness
process ever exiting.

The harness has its own version (`[workspace.package] version` in `Cargo.toml` is the
authority, and this README names none); the vendor's is read at run time:

```text
$ muse-alab --version
muse-alab <version> (muse <vendor version>)
```

It is opt-in and changes nothing for anyone who does not run it: no configuration file, no
environment knob of its own (release builds), nothing written to the terminal while Muse
Code runs, and no change to `muse` itself. The only variables it sets are its recursion
guard's two marks on the processes it starts as Muse Code (below).

## Use

```sh
aterm pkg install muse        # the vendor CLI, into aterm's package store
muse-alab [muse arguments…]   # Muse Code, under the harness
muse-alab --version           # the harness's version and the installed Muse Code build
muse-alab --no-harness …      # Muse Code from the store, replacing this process (no harness)
```

`--no-harness` (or `--no-alab-harness`, before a literal `--`) and `ATERM_NO_HARNESS=1` —
the opt-out aterm's `agents/` twin already uses — run the store's `muse` in place of the
harness with the remaining arguments, so nothing is supervised.

## What it does

### It finds Muse Code in the atpkg store, and only there

The binary it runs is `<prefix>/store/muse/<build>/bin/muse`, where `current` points
(`src/store.rs`, ported from aterm's `crates/atpkg`). The prefix is atpkg's: `[packages]
prefix` in `$XDG_CONFIG_HOME/aterm/aterm.toml` (else `~/.config/aterm/aterm.toml`), read
conservatively — a spelling it cannot read with certainty is a refusal, not a guess — and
otherwise the default (`~/Library/Application Support/aterm/pkg` on macOS). The build must
be complete: its install record names the same version as its directory, its ready marker
says `ok`, and `bin/muse` is an executable regular file.

It never runs a `muse` found on `PATH` and never aterm's `agents/` twin; it refuses a store
`muse` that is this harness (by name, as the same file, or as a byte-for-byte copy); and
every process it starts as Muse Code carries `MUSE_ALAB_PID` (the harness's pid) and
`MUSE_ALAB_DEPTH` (its nesting depth plus one). A harness that finds `MUSE_ALAB_PID` naming
its parent or itself was started as Muse Code by a harness — a store `muse` that `exec`s
`muse-alab` again, or another harness build the byte check cannot see — and refuses at
once; one nested eight deep (a store `muse` that runs `muse-alab` as a child of a shell)
refuses too. So it can never recurse into itself (exit 127, as below). A harness started
from a Muse Code tool's shell is one level deeper and runs normally. When Muse Code is
missing it says so and exits 127:

```text
muse-alab: Muse Code cannot be run from the atpkg store: it is not installed (…/store/muse/current is missing). Install it with `aterm pkg install muse`.
muse-alab: Muse Code was not started.
```

### It runs Muse Code as its child, on your terminal

The child is started in the harness's own process group on the inherited terminal (no
pseudo-terminal in between, no input or output proxied), so the terminal, its size and
its job control are Muse Code's exactly as without the harness.

| Signal | What the harness does |
|---|---|
| `SIGINT`, `SIGQUIT` | Ignores them: the terminal sends them to the whole job, and Muse Code decides what they mean. |
| `SIGTSTP` (^Z) | Stops with the job (the default action). |
| child stopped | Stops itself with the same signal, so the shell sees the job stopped. |
| `SIGCONT` | Continues the child too. |
| `SIGHUP`, `SIGTERM` | Passes them on; the session is ending, so nothing is relaunched. |
| one the caller left ignored | Leaves it ignored: `nohup` ignores `SIGHUP`, a shell without job control ignores `SIGINT` and `SIGQUIT` for a `&` job. Among `SIGINT`, `SIGQUIT`, `SIGHUP` and `SIGTERM`, such a signal is not caught, so Muse Code inherits the ignore exactly as it would without the harness. |

When the session really ends the harness ends **the way the child did**: the same exit
code, or death by the same signal (with core dumps off, so the harness leaves no core of
its own). The terminal's mode is put back after a child that a signal ended, and before
every relaunch, so a crashed TUI's raw mode is never taken for the mode to restore.

A child that **crashed** — not a clean end (`0`, `130`, `143`) and not a startup refusal
(`1`, `2`), aterm's measured Muse Code exit table — is resumed on its session, at most three
times in ten minutes, unless its session log says it ended clean (`session.end` with
`exit_reason: clean`). It comes back on the newest build when that build is proved to start
(below), else on the build it ran. A crash without a witnessed session, or past the budget,
ends the harness as the child ended.

The harness's own lines go to stderr; a closed or broken stderr is ignored, so a failed
write can never stop a relaunch.

### It moves the session onto a newer build at a safe point

Every two seconds the harness reads the store's `current` link. When it names a newer
build than the one running (atpkg's version order), the harness looks for a **safe point**,
proves the new build starts, and only then sends the child `SIGTERM` — as aterm's own Muse
Code quiet restart does. When the child has ended it starts the proved build with Muse
Code's resume argv for the same session and says so on one line:

```text
muse-alab: moving Muse Code 1.4.10-R6427.1 -> 1.4.13-R6996.1, resuming session 01a148f8-9301-7752-96db-fef456f88074
```

The resume argv is aterm's Muse Code grammar, ported verbatim (`src/muse/argv.rs`, from
aterm `crates/aterm-agent/src/harness/upgrade_muse.rs`): flags the session keeps are
dropped (`--model`, `--effort`, `--yolo`, …), the rest are carried, the first prompt is
dropped, and `resume <session>` is added. A flag the table does not know, or one no resume
can carry (`--workspace`, `--worktree`, `exec`, …), means the session is never stopped for
an update.

The safe point is every one of these, read fresh; anything unread or unknown holds:

| Check | Witness | Holds with |
|---|---|---|
| the launch can be resumed | Muse Code's resume grammar | `not-resumable` |
| this job owns the terminal | `tcgetpgrp` | `no-tty`, `not-foreground` |
| no person is typing | the terminal's input clock is at least 120 s old | `person-typing`, `no-input-clock` |
| no typing waits unread | the terminal's input queue is empty (`FIONREAD`): keys a busy Muse Code has not read yet never moved the clock | `input-pending`, `no-input-queue` |
| no tool is running | the child has no descendant processes | `child-process`, `process-table` |
| the session is known | exactly one Muse Code session lock naming the child, written since it started | `no-session` |
| the turn ended | Muse Code's session log: no open run, task or queued item in this process's part, nor in its subagents' logs (aterm's gates G-M1/G-M2) | `turn-open`, `open-task`, `background-work`, `queued-work`, `log-unproved`, `log-unreadable` |
| the composer is empty | no terminal input since the last prompt was submitted | `draft-unproven` |
| the turn has settled | the last turn ended at least 30 s ago | `turn-settling` |
| the new build starts | `<new build>/bin/muse --help`, with no terminal and in its own process group, exits 0 within 5 s (aterm's build probe, `upgrade_build_start.rs`, which its Muse Code quiet restart requires before its signal); a build that fails is probed again no sooner than 15 minutes later | `build-unproved` (and `build-proved` for the look that ran the probe: the next look reads everything again) |

Just before the signal the terminal and the process table are read once more. A busy child
is never stopped, and a child is never killed: one that does not end within 15 s of the
`SIGTERM` keeps running on its build and is never signalled for an update again. What the
harness waited on, and why, goes to its journal, `$XDG_STATE_HOME/muse-alab/journal` (else
`~/.local/state/muse-alab/journal`), never to the terminal.

### The terminal's input clock (measured)

The harness never reads the terminal; the child owns it. It reads the terminal device's
access time, which the kernel moves when a process reads input. Measured on a pty on macOS
(Darwin 27, 2026-10-24):

- output, `stat`, `poll` timeouts and the other side's writes never move it;
- a read in a different wall-clock second from the stored time sets it to the read's time,
  to the microsecond;
- a read in the same second only marks the node, and the next `stat` (by anyone) stores a
  time later than the read — never earlier.

So the clock is never earlier than the last input, and "no input since the prompt was
submitted" is sound. It can be later than the last input, which only holds the update
longer: input after the submission (a key, a paste, a mouse or focus report, a terminal
query's answer), or a submitting Enter typed within the same second as the key before it,
holds the update until the next turn — or until the session ends, since every launch
starts the newest build anyway. A prompt that arrives in one read (a paste, or aterm typing
it) is timed exactly. Muse Code links crossterm, whose Unix event source reads the terminal
only when `poll` says it is readable, so its reads are its input.

Linux keeps a tty's times only to an 8-second bucket, which can be earlier than the last
input, so there the harness reads no input clock and never reaches a safe point (crash
resume and everything else still work).

## Under aterm: contract 3

In an aterm tab (`TERM_PROGRAM=aterm` or `ATERM_CHILD=1`) the harness follows the start
sequence of aterm's harness design (`docs/DESIGN-harness-repos-2026-10-24.md` §3.2, §8),
`src/aterm.rs`, the grammar grok-alab already speaks:

1. It finds the tab's control socket (`ATERM_PARENT_SESSION_ID`, the rendezvous directory's
   `graph/<sid>`, the token beside the socket) and reads `harness policy`.
2. Only when the reply lists `contract=3` and says `effective=on` does it claim:
   `meta set supervisor muse-alab@<pid> contract=3 scope=relaunch,upgrade version=<version>
   vendor=<vendor version>`. aterm proves it (the connection's kernel peer is the pid, which
   leads the tab's foreground group, and its image is a `muse-alab` build in the atpkg store
   or its dev link), and the harness confirms it with `status` (`supervisor_proven=1`).
3. Anything else — a launch that is not Muse Code's interactive TUI, no socket that
   answers, an aterm that predates contract 3, a policy that is off, a claim aterm did not
   prove — and the harness gets out of the way: it `exec`s the store's `muse` in its own
   place, and the tab runs Muse Code under its own name, supervised by aterm's built-in Muse
   Code lanes as if no harness existed. The reason goes to the journal (`PASS`).

Under a proven claim every act waits on one fresh read of aterm (`src/link.rs`): the claim
is still this harness's and proven, the policy is on with the duty's own switch
(`[harness] relaunch`, `upgrade`), and for a move no person typed within the person grace by
aterm's `human_ms`. A policy found off gives the claim back (made again once it is back on,
read every 5 s); a claim aterm no longer shows, or a connection aterm closed, is made again,
and the act is judged again once aterm proves it; a move re-announces it with the new build's
`vendor=`. A crash that comes while the claim is away (aterm restarting or updating, or no
longer showing it) waits up to 45 s, aterm's own adopted-claim grace, for aterm to prove the
claim again, because no built-in lane of aterm's can relaunch a child this harness leads; then
it is relaunched, or, if the claim stays away, passed back as an intended end (`HOLD`, then
`NO-RELAUNCH`). The claim's connection is
close-on-exec and stays open for the harness's life, so the claim ends with the harness. The
harness never types into the session (`relaunch` and `upgrade` type nothing). Outside aterm
it supervises with no claim, as before. aterm's screen reader (`composer=`, not served yet)
is the witness that will lift the composer check's conservative holds.

## Rulings this follows

- Every harness is its own repository with its own version, starting at v0.1.0, written in
  Rust and distributed through atpkg.
- A harness runs the unmodified vendor CLI as its child; it never ships, forks or patches
  the vendor's bytes, and reports the vendor's version at run time.
- "For the harnesses, there is nothing preventing updating the underlying ai cli underneith
  the harness as the harness is running as a program in aterm": the harness updates the CLI
  underneath itself at a safe point (turn ended, composer empty, no person typing),
  relaunching with the vendor's resume argv, without exiting.
- Under Autopilot the harness never blocks on a person: every choice is made fail-closed
  and recorded (below).

## Decisions

- **License: Apache-2.0**, with a NOTICE, as aterm and the first-party repositories it
  ports from; nothing here is MIT-only code.
- **Fail closed everywhere.** Every witness that cannot be read holds the update; a store
  that cannot be read with certainty is a refusal (exit 127); a configured prefix outside
  the home directory (a shared store whose admission rules the harness does not reproduce)
  is refused.
- **The composer witness is the terminal's input clock**, measured above; no screen reading
  and no pseudo-terminal proxy, so the terminal stays Muse Code's own. Its conservative
  holds are accepted until aterm's screen reader (contract v3).
- **The safe point is aterm's**: its Muse Code gates G-M1/G-M2 over the session log, its
  120 s person grace, plus a 30 s settle after the turn and a no-descendants check. The stop
  is aterm's: one `SIGTERM`, never `SIGKILL`.
- **A clean exit ends the session**, even with an update pending; the next launch starts
  the newest build.
- **Crash resume follows aterm's exit table** (three relaunches per ten minutes); a clean
  `session.end` in the child's own log part vetoes it, and nothing ever causes one.
- **The journal is a file, never the terminal**; the only terminal output is one line before
  a relaunch and the refusals.
- **No `harness.json`, no environment knobs.** A harness file would advertise a contract
  this release does not speak. Debug builds read `MUSE_ALAB_TEST_TIMING` so the tests run in
  seconds; release builds read nothing.
- **Linux reaches no safe point** (its tty clock is too coarse to be sound).
- **The flag table is aterm's, measured on Muse Code 1.4.10**; a flag a newer build adds is
  unknown and refuses the update for that session rather than guessing its arity.
- **A new build must be proved to start before a session is stopped for it** (review,
  2026-10-24): aterm's quiet restart runs `<build> --help` and requires exit 0 before its
  `SIGTERM`; without it, a build atpkg installed that cannot start (damaged, wrong
  architecture, killed by code signing) would end an idle session instead of moving it. A
  crash relaunch takes the newest build only when it is proved, else the build it ran. The
  probe is the harness's own process: a probe that outlives 5 s has its own process group
  killed, and nothing of the session is ever touched. Measured on the installed Muse Code
  1.4.13-R6996.1: its first `--help` took 3.98 s on a loaded machine (load average 67),
  later ones 0.02 s; a cold probe past the bound holds the update for 15 minutes, never the
  session.
- **The recursion guard is two environment marks** (review, 2026-10-24), `MUSE_ALAB_PID`
  and `MUSE_ALAB_DEPTH` (at most 8 deep), the same guard the claude-code-alab harness
  carries. The name, same-file and byte checks alone let a store `muse` that runs
  `muse-alab` again (a wrapper script) recurse without bound: measured on the first
  commit (07dd78f), 349 processes in 2 s through an `exec` wrapper and 477 through a plain one.
- **A signal the caller ignored stays ignored** (review, 2026-10-24), so `nohup muse-alab`
  keeps Muse Code running past a hangup as `nohup muse` does.
- **Typing that waits unread holds the update** (review, 2026-10-24): the input clock moves
  only when a key is read, so a busy Muse Code's unread keys are visible only in the
  terminal's input queue.
- **The review did not bump the version**: the first version was never tagged, staged or
  released, so the review's fixes are part of it.
- **The tree passes the publication engine's guards at their root** (2026-10-25): a test's
  store path sits under `/nonexistent-home/u`, as `src/store.rs`'s tests do, not under a
  macOS home (the engine refuses any `/Users/<name>` path, a test constant's included), and
  `.gitignore` names `/publish/.out/`, the engine's report directory, so a stage leaves the
  checkout clean for the next one. No guard is allowlisted or weakened.
- **The README states no version of its own** (2026-10-25), the owner rule the engine's
  guard enforces as absence, not agreement: the `--version` example shows
  `muse-alab <version> (muse <vendor version>)`, the release-relative remarks say "this
  release", the recursion measurement names its commit, and the version decision quotes no
  number. The guard's patterns miss a colon-less "Version X." label and a fenced example,
  but both go stale at the next bump all the same. The ruling's own words, "starting at
  v0.1.0", name where the series began and stay. The doc comments in `src/lib.rs` and
  `src/aterm.rs` say "this release" too. The `--version` tests read the version from
  `Cargo.toml` (2026-10-27), so a bump edits no test.
- **The verified lane refutes nothing, each row fixed at its cause** (2026-10-25): with the
  atpkg store's Trust (`targo 0.11.0+g20565c325`) the advisory lane refuted 81 rows, and
  none was excused, allowed or moved out of the workspace:
  - the JSON reader steps its cursor only through one guarded step, reads literals and
    `\u` escapes byte by byte, and slices with `get`, so no cursor sum can overflow and no
    slice can pass the end; a surrogate pair is decoded inside match ranges;
  - on macOS the caught signals' handler does nothing: a kqueue `EVFILT_SIGNAL` filter
    records every signal posted to the process and the loop sleeps in `kevent`, so no
    handler saves and restores `errno` through a raw pointer and no wake byte can be lost
    to a full pipe (Linux keeps the self-pipe);
  - C structs (`sigaction`, `termios`, `proc_bsdinfo`) are built field by field instead of
    with `mem::zeroed`; the terminal's access time is read through std (`File::metadata`
    of a duplicate of the descriptor, which moves no access time; the input-clock tests
    pass) and `isatty` is std's `IsTerminal`;
  - the child-pid listing sizes its buffer behind a bound checked in the same function;
    the byte comparison reads through std's `read_to_end` over a `take`; the relaunch
    message is formatted by the caller instead of by a closure passed to a generic; the
    aterm client is a type parameter instead of `dyn`; the drained-inbox multiset is a
    count map instead of a `Vec::remove`; the probe checks its group's sign where it
    negates it, and `128 + signal` saturates; a raw borrow sits in the block of the call
    that takes it.
- **`--version` names Muse Code's version only from its install record, and otherwise says
  why** (2026-10-27; aterm `docs/DESIGN-harness-repos-2026-10-24.md` §5 and D43). The line is
  `muse-alab <version> (muse <vendor version>)`. The vendor version is the `version` that
  the `current` build's `.vendor` record names, and only when it names that build's own
  directory (the check `store::current` makes before any start). With no `current` link the
  line says `(muse not installed)`. For every other reason it says `(muse unknown)`: no
  readable store, a link that names no build, a record that is missing, garbage or names
  another version, or a build that is not complete. Before, every reason printed
  `not installed`, which the store did not show. `--version` never runs `muse` (the tests
  check the fake's start log and the `PATH` decoy), and a version is never decoded from a
  directory name alone. The claim will carry the same two facts, `version=` and, only when
  known, `vendor=`.
- **No git tag before a release** (2026-10-27). The version is `[workspace.package] version`
  and the `--version` line. A `v<version>` tag marks a released version only: the
  publication engine's `pub tag` pushes it once `pub promote` has proved the release
  public, and a promote stops at an occupied dev tag that names another commit. A hand tag
  now would either block that release or name a release that never happened, so the
  repository carries no tag until the owner directs the first promote. `pub tag` adds it
  then.
- **A bounded read handles the largest representable bound** (2026-11-05).
  Add the one-byte growth check with checked arithmetic. At `u64::MAX` there is no
  larger representable byte count, so retain that bound. The release gate found
  the former addition overflow; a regression covers empty, exact, oversized,
  maximum-bound and invalid-UTF-8 inputs. File-type and no-follow checks remain.
- **A packed build prints the version its pack signs** (2026-10-27; aterm
  `docs/DESIGN-harness-repos-2026-10-24.md` D46). `atpkg-publish pack` mints
  `MAJOR.MINOR.0+g<sha9>` for the commit it builds and hands it to the build as
  `ALAB_HARNESS_VERSION`; `HARNESS_VERSION` is that value, else the package's
  `MAJOR.MINOR.0`. So a packed binary's `--version` line and its claim's `version=` say what
  the signed manifest says, and a hand build says the bare line, as before. A value that is
  not this package's line alone or followed by `+g` and 9 to 40 lowercase hex digits
  (`is_pack_version`) fails the build, so a wrong one is never printed. Reversible: unset, it
  is `CARGO_PKG_VERSION`.
- **Its first signed pack waits for the owner before any index pins it** (2026-10-31; aterm
  `docs/DESIGN-harness-repos-2026-10-24.md` D50). atpkg build 7, packed from `dd52467` by
  `atpkg-publish pack --upload` and signed by roster machine `incumbent-head`, is staged as
  the dev repository's prerelease `atpkg-muse-alab-7`. No public index pins it: the pin's
  `--upload` mirrors the pack to `alabsystems/muse-alab` first, which does not exist, and
  creating it and the promote that fills it are the owner's. Until then a machine runs it as a
  dev link. Reversible: the prerelease is deleted, or later builds supersede it.
- **A moved build that passes `--help` but fails on interactive startup falls back and is
  marked blind** (2026-11-03; aterm `docs/DESIGN-harness-repos-2026-10-24.md` §4.3 step 6).
  When a newly-moved build exits non-zero (excluding signal-derived codes `129`, `130`, `131`,
  `143`) or dies by a crash signal within the 15 s startup grace before writing a session lock
  for its pid, the harness restores the saved terminal modes, removes the target build's proof,
  marks the target build blind so `probe_gap` never re-proves it, and resumes the session on
  the previous build (reporting `NO-RELAUNCH` and finishing if that fallback cannot start). If
  a target build is missing at relaunch, its cached proof is invalidated and any substitute
  newest build must both be proved and move forward from the fallback build.

## Testing

`tests/harness.rs` runs the built binary on a pseudo-terminal in its own session over a
sandboxed store whose `muse` is a fake Muse Code — the test binary itself, which keeps a
session the way Muse Code does (lock, session log, run records), reads the terminal in raw
mode only when it is readable, and takes commands from a control file. It covers argument
and terminal passthrough, exit codes and signal deaths, signal forwarding, ignores the
caller set (`nohup`), following a stop, the move onto a newer build with the resume argv in
the same harness process (after the new build's probe), the holds (open turn, running tool,
draft, typing left unread, unresumable launch, a build that does not start, a child that
ignores `SIGTERM`), crash resume, its budget and its fallback to the build it ran, a broken
stderr during a relaunch, the refusals, and the recursion guard (a store `muse` that runs
the harness again, by `exec`, as a shell's child, and under `--no-harness`).

## Verification

Measured on this tree, 2026-10-25, on macOS (Apple silicon), with the atpkg store's Trust
15698 (`targo 0.11.0+g20565c325`, the build public users get), each step of the
publication gate spelled with that `targo`:

- `targo trust check --allow-l0-gaps --locked --workspace`: **no refuted row**. "Gate:
  COULD-NOT-RUN [advisory lane, exit 3] (198 proved, 0 failed, 243 runtime-checked, 225
  assumed, 32 mandated, 0 contract-panic, 1595 unknown / 2293 total)". Exit 3 is no verdict
  either way: Trust gives 1870 of the rows no classified stop cause, and an unclassified
  stop makes the run "not-reproducible" (the publication engine admits exactly this
  census, `GATE_NO_VERDICT`). The unknown rows are, in the main, the standard library's
  formatting and dispatch and the FFI calls a process supervisor cannot do without
  (`sigaction`, `kevent`, `waitpid`, `kill`, `ioctl`, `tcsetattr`, `proc_listchildpids`,
  …): unproved, never refuted, so the strict lane (not the gate) still fails on them
  ("Gate: FAIL [strict lane, exit 1] (195 proved, 0 failed, …, 1508 unknown / 2191
  total)"). The previous commit refuted 81 rows ("Gate: FAIL [advisory lane, exit 1] (188
  proved, 81 failed, …)"); the decision above says how each was fixed. These runs used a
  private trustd memory domain (`TRUSTD_RUNTIME_DIR`): another toolchain's daemon held the
  host's socket; the domain changes the memory budget's sharing, not a verdict.
- `targo --unverified test --locked --workspace`: 42 unit and 26 integration tests pass.
- `targo tippy --locked --workspace --all-targets -- -D warnings` (which rejects a planted
  `len_zero`) and `targo fmt --check` pass.
- Every review fix has a test that fails on the first commit (07dd78f) and passes here, and
  each was mutation-checked: removing the fix fails its test.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
