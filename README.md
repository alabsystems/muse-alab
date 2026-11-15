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
Code runs (under `run --`, the launcher's one yolo line before it starts, below), and no change
to `muse` itself. The only variables it sets are its recursion
guard's two marks on the processes it starts as Muse Code (below).

## Use

```sh
aterm pkg install muse             # the vendor CLI, into aterm's package store
muse-alab [muse arguments…]        # Muse Code, under the harness
muse-alab run -- [muse arguments…] # the launcher's yolo default, then Muse Code as above (below)
muse-alab --version                # the harness's version and the installed Muse Code build
muse-alab --no-harness …           # Muse Code from the store, replacing this process
```

`--no-harness` (or `--no-alab-harness`, before a literal `--`) and `ATERM_NO_HARNESS=1` —
the opt-out aterm's `agents/` twin already uses — run the store's `muse` in place of the
harness with the remaining arguments (under `run --`, the arguments the launcher's blocks left;
below), so nothing is supervised.

### Run by atpkg's launcher: `run --`

Once this harness's record leads the word `muse` (`leads` in `alab-harness.toml`, still empty
until its `lab harness-proof` passes against the real Muse Code), atpkg's launcher for `muse`
keeps only its check that a live build of the harness is installed and then runs
`exec <prefix>/bin/muse-alab run -- "$@"` (aterm's harness interface design §2.1, "The launcher
keeps its other duties"; aterm `crates/atpkg/src/platform/mod.rs:659-690`). Every word after that
`--` is Muse Code's. The launcher's other blocks for `muse` move into `run`, which runs them in
the launcher's order, in its own process, before Muse Code starts (`src/launcher.rs`, citing the
aterm source of each):

- **the self-update intercept**: none. The muse row names no self-update verb, so the launcher
  has no such block and `muse-alab run -- update` hands `update` to Muse Code, as `muse update`
  does today;
- **the sign-in gate**: none. The muse row has none;
- **the yolo default**, byte for byte: with nothing typed ahead of a literal `--` that decides the
  posture, Muse Code starts with `--disable-approval --disable-sandbox --trust-workspace
  --disable-request-user-input`, in front, or right behind a first word `exec`; a typed `--yolo`
  spells the same in its place; a posture flag of Muse Code's own (`--approval-mode`, `-c`, …),
  a first word that takes none (`resume`, `skills`, `login`, …), an option the launcher's scan
  does not know ahead of the first word, or `--model` with no value runs as typed. Before Muse
  Code starts, stderr carries the launcher's own line, e.g.
  `aterm: yolo (default): running 'muse' '--disable-approval' '--disable-sandbox'
  '--trust-workspace' '--disable-request-user-input'` (`aterm: --yolo: running …` for a typed
  `--yolo`). `tests/fixtures/atpkg-muse-yolo.sh` is the launcher's block as atpkg lays it, and a
  unit test holds the harness to it on 22,027 argvs under `/bin/sh`, bash and dash, among them
  every argv of one or two words over every word the block names, each spelled out in the test
  rather than taken from the harness's own tables (another test holds that list to the block's
  `case` patterns);
- **the shim's environment**: none. The muse row sets no variable, and the harness runs Muse Code
  from its store build, never through atpkg's shim.

These run whether or not the harness then supervises: a `run` that is handed `--no-harness`
(stripped first, as the launcher strips it), or runs under `ATERM_NO_HARNESS`, still gets the
yolo default and its line, and then the store's `muse` runs in the harness's place with the
arguments the block left. That is what the launcher does today: its `--no-harness` only sets
`ATERM_NO_HARNESS=1` and its yolo block runs all the same (aterm
`platform/mod_sh_shim_tests.rs:790-793`), so turning the harness off skips the supervision and
nothing else. A `run` without the launcher's `--` is refused with exit 2: no launcher writes it,
and Muse Code has no `run` of its own. `muse-alab` typed by its own name runs no launcher block,
as before.

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
| `SIGINT`, `SIGQUIT` | Ignores them while Muse Code runs: the terminal sends them to the whole job, and Muse Code decides what they mean. While a crash relaunch waits and no child runs, one ends the session (below). |
| `SIGTSTP` (^Z) | Stops with the job (the default action). |
| child stopped | Stops itself with the same signal, so the shell sees the job stopped. |
| `SIGCONT` | Continues the child too. |
| `SIGHUP`, `SIGTERM` | Passes them on; the session is ending, so nothing is relaunched (and a crash relaunch that waits is given up). |
| one the caller left ignored | Leaves it ignored: `nohup` ignores `SIGHUP`, a shell without job control ignores `SIGINT` and `SIGQUIT` for a `&` job. Among `SIGINT`, `SIGQUIT`, `SIGHUP` and `SIGTERM`, such a signal is not caught, so Muse Code inherits the ignore exactly as it would without the harness. |

When the session really ends the harness ends **the way the child did**: the same exit
code, or death by the same signal (with core dumps off, so the harness leaves no core of
its own). The terminal's mode is put back after a child that a signal ended, and before
every relaunch, so a crashed TUI's raw mode is never taken for the mode to restore.

A child that **crashed** — not a clean end (`0`, `130`, `143`) and not a startup refusal
(`1`, `2`), aterm's measured Muse Code exit table, and not a job or terminal signal's end
(`SIGINT`, `SIGQUIT`, `SIGHUP`, `SIGTERM`, or their `128+n` codes: aterm's own exit rule,
`alab_wire::relaunch::abnormal_exit`) — is resumed on its session after aterm's back-off
ladder's pause: 1 s before the first relaunch, 10 s before the second, 60 s before the third,
then 10 minutes before each one after; a child that ran 10 minutes since its last crash
relaunch starts the ladder over. Its session log saying it ended clean (`session.end` with
`exit_reason: clean`) vetoes it. It comes back on the newest build when that build is proved to
start (below) and, under aterm, the claim takes `upgrade`; else on the build it ran. A crash
without a witnessed session, or whose launch cannot be resumed, ends the harness as the child
ended; so does a `SIGINT`, `SIGQUIT`, `SIGHUP` or `SIGTERM` reaching the harness while the
relaunch waits (whoever sends one wants the session over). Under aterm the relaunch also waits
on aterm's word (below).

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
| the composer is empty | under an aterm that serves `composer=` in `status`, aterm's screen reader, read fresh at the act: `empty` or `placeholder`; elsewhere no terminal input since the last prompt was submitted | `draft`, `composer-unproven` (aterm's `-`); `draft-unproven` |
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

## Under aterm: alab-wire

In an aterm tab (`TERM_PROGRAM=aterm`, `ATERM_CHILD=1` or a set `ATERM_SESSION_ID`) the harness
links to aterm through `alab-wire`, the ALab harnesses' shared aterm client
(`vendor/alab-harness/crates/alab-wire`, the vendored copy of alab-harness `v0.7.1`, pinned by
its git tree id in `vendor/alab-harness.pin`; see Decisions). `src/aterm.rs` holds only what is
this harness's own: who it is (`muse-alab`, vendor `muse`, duties `relaunch,upgrade`, contract 4
alone), the clock its link runs on (one that stops while the Mac sleeps), what it claims for a
child, aterm's word on a move, and how the link's notes reach the journal. The start sequence (aterm's harness design,
`docs/DESIGN-harness-repos-2026-10-24.md` §3.2, §8, and its interface design §2.2):

1. It finds the tab's control socket (`ATERM_PARENT_SESSION_ID`, the rendezvous directory's
   `graph/<sid>`, the token beside the socket) and reads `harness policy`.
2. It claims both duties at contract 4, the only contract it claims under (`contracts = [4]`
   in `alab-harness.toml`, `CONTRACTS` in `src/aterm.rs`): `meta set supervisor
   muse-alab@<pid> contract=4 scope=relaunch,upgrade version=<version> vendor=<vendor version>`,
   whose reply is aterm's grant, read back with `status` (`supervisor_proven=1`, a
   `supervisor_scope=` taking both). After every child start (the first included) it announces
   the claim again with the child it now runs (`vendor=`, and `child=<pid>:<kernel start>`), so
   aterm can tell this child from the next. It never claims contract 3: it declares 4 alone to
   `alab-wire` (`Config::with_contracts`), whose every claim keeps to it, so an aterm whose
   policy lists no 4 is sent no claim at the launch (below), nor later: a launch whose first
   read went unanswered, or a claim lost to an aterm restart into one serving `2,3`, is claimed
   at 4 once aterm serves it and not before (`UNCLAIMED policy contract=2,3 declared=4`).
3. Where aterm refuses — a launch that is not Muse Code's interactive TUI, no session to locate,
   no socket that answers, a window that cannot host a harness (`why=unhosted`), a policy that
   serves no contract 4 (`2,3`, aterm main before HX-06, or 2 alone), no registry row that
   proves the holder, a claim aterm did not prove, or a grant of less than both duties (given
   back) — the harness gets out of the way: it `exec`s the store's `muse` in its own place, and
   the tab runs Muse Code under its own name, supervised by aterm's built-in Muse Code lanes as
   if no harness existed (`UNCLAIMED`, then `PASS`). An aterm that is busy (it answered the handshake and then late, or `ERR` while it
   shows a dialog) or switched off for now (`autopilot-off`, `disabled`, `agent-off`) is not
   away: Muse Code runs as the harness's child and the claim is made every 5 s until aterm
   admits it (`UNCLAIMED`, then `CLAIMED`).

The launch gives each socket operation 2 s; every exchange from the supervisor's loop gives aterm
at most 500 ms. The claim follows aterm's policy and events from then on (`alab-wire`'s
`Link`): given back while the policy is off by a setting (`RELEASED`), made again once it is on,
narrowed or widened as a person's cap moves (`NARROWED`, `WIDENED`), and made again every 5 s
once its connection fails (`LOST`), as when aterm restarts.

- **A crash** is relaunched only on aterm's word (`Link::relaunch_word`): one fresh policy and
  status read, asked when the crash is seen and again when the ladder's pause is over, so the
  relaunch stands on a fresh read; aterm's `[harness] relaunch = false`, a person who typed
  within aterm's grace, a halt or another driver's hand passes the crash on (`NO-RELAUNCH
  aterm's word: …`). While aterm is away (a lost link) or answers only later, the crash waits
  (`CRASH-WAIT`), asking every second, up to aterm's own 45 s grace in all: in a tab this
  harness leads no built-in lane of aterm's brings the child back, so a crash passed on at once
  would leave the session to nobody. Past it the crash is passed on (`NO-RELAUNCH
  claim-not-proven-again`). The wait sleeps between its asks. In a tab where aterm never admitted
  a claim of this harness's (busy or off for now at the launch, not serving its contract or
  refused since), the crash is passed on at once (`NO-RELAUNCH no-claim`): aterm never let the
  harness lead the tab, so there is no lost claim to wait for. A lost link whose claim made
  again aterm answered serving no contract this harness declares (`2,3` after an aterm restart)
  is not away either: that answer is aterm's last word on the link, and the crash is passed on
  at once (`NO-RELAUNCH aterm's word: policy contract=2,3 declared=4`). Keys typed at the dead screen
  while a crash waits are dropped before the relaunched child starts or the crash is passed on,
  so they reach neither the resumed Muse Code nor the parent shell.
- **A restart aterm asks for** (`restart cause=asked`, a person's `aterm ctl restart`, or
  `cause=stall`, the input stall's remedy; it comes on aterm's event stream) ends the child with
  `SIGTERM` at once, no safe point waited for, and resumes the same session (`RESTART`); it is
  no crash and leaves the ladder alone. A child still running 5 s after a stall's `SIGTERM` (30 s
  for any other cause, aterm's restart act's deadline) gets `SIGKILL` (`RESTART-KILL`), as
  aterm's own restart does. A restart that names a model (`model=<word>`) starts Muse Code on it,
  `--model=<word>` among the root options before `resume`. One onto another account
  (`cause=account to=…`) is refused and the child runs on: Muse Code takes no account home this
  harness can set. A restart waits, asked again every second, for the child's session to be
  witnessed and for aterm's word, at most 45 s from its arrival (`RESTART-WAITS`, then
  `RESTART-REFUSED … not made within`, judged before aterm is asked again): a restart made much
  later would end a turn for a cause long gone. One still waiting when a new child starts, or sent as the old child ended, is
  passed over (`RESTART-PASSED-OVER`); one sent while a crash relaunch waits is refused (no child
  runs to restart).
- **A move** (onto a newer Muse Code build, or of this harness onto a newer build of itself) is
  aterm's `upgrade` duty, so in an aterm tab it is weighed only while the standing claim was
  granted it: a link being claimed again, one aterm has not admitted yet, or one aterm refused
  after the launch holds it (`no-claim`), so does a claim given back (`policy-off`), and a grant a
  person's cap narrowed to `relaunch` (`upgrade-not-granted`); a crash relaunch or a restart then
  stays on the build the child ran. Only outside aterm do the harness's own readings decide
  alone. At the act, after the harness's own safe point
  (below), one fresh policy and status read must still show the claim this harness's, proven,
  taking `upgrade`, no halt, no other driver's hand, `upgrade=true` and `upgrade_ask=false`
  (`alab-wire`'s rule for a move a harness judges by its own readings), no person's key within
  the longer of aterm's `human_grace_s` and the harness's own grace (`human_ms`), and the
  composer as aterm reads it: `empty` or `placeholder` lets the move go, `draft` holds it, and
  `-` (aterm's reader cannot tell) holds it as `composer-unproven`. Only an aterm that serves no
  `composer=` leaves the composer to the terminal's input clock, with its conservative holds
  (above). Each hold is a `WAIT` line (`aterm-hold (aterm <field>=<value>)` for aterm's).

The claim's connections are close-on-exec and stay open for the image's life, so the claim ends
with the harness (at its own re-exec, below, the new image claims again). The harness never
types into the session (`relaunch` and `upgrade` type nothing). Outside aterm it supervises with
no claim, as before: moves on its own readings, crashes relaunched on the ladder with no word to
wait for.

### It moves itself onto a newer build of itself

When atpkg installs a newer build of the harness itself (`<prefix>/store/muse-alab/current`
naming a larger build number than the image running, which is that store's
`<n>/bin/muse-alab`), the harness waits for the same safe point a move waits for, under aterm's
word as a move is, and proves the new image first: `<new>/bin/muse-alab --alab-adopt-probe` must
exit 0 within 5 s and print exactly `muse-alab-adopt 2`, the format of the state it is about to
be handed. Then it `execve`s the new image in its own process (aterm's harness design §4.5,
`src/adopt.rs`): the same pid, the same child, which is never signalled and never notices. What
crosses, as text in a pipe whose read end is the one descriptor the new image inherits
(`--alab-adopt <fd>`): the child's pid, build and launch argv, its session and lock, the move
still in its startup grace, whether it outlived a `SIGTERM`, the crash ladder's streak and its
last relaunch, the builds probed, the terminal's starting mode, the store prefix, and whether the
old image had a link to aterm. The caught
signals are blocked across the exec, so a `^C` cannot end the harness between the two images, and
the new image acts on what arrived meanwhile; a `SIGHUP`, `SIGTERM` or `SIGCONT` already waiting
holds the exec. The claim's connections are close-on-exec: aterm sees them close, and the new
image claims again under the same pid, with its own `version=` and the child it took over,
within aterm's adopted-claim grace (`alab_wire::Link::adopt_with`: a claim it cannot make is made
again every 5 s, never given up, since an adopted child can only be supervised). A build that does not answer
the probe is never exec'd into; an exec that fails leaves the old image running and that build
unproved for good. A harness run from outside the store (a checkout's `target/`) has no build to
order against and never re-execs. The journal says `REEXEC` (old image) and `ADOPTED` (new).

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
- **The composer witness is aterm's screen reader where aterm serves it, else the terminal's
  input clock** (2026-11-12; aterm `docs/DESIGN-harness-repos-2026-10-24.md` §8.1 item 9). The
  harness reads no screen and puts no pseudo-terminal between, so the terminal stays Muse Code's
  own. The live harness proof showed the clock alone holding every move after a person's typed
  submission (a focus report or a terminal's answer after the Enter moves it), so a move waits on
  aterm's `composer=` from the same fresh read that judges the act; an aterm that predates it
  leaves the clock's holds as they were. aterm's `-` holds: it means aterm cannot tell a draft
  from a placeholder.
- **The harness updates itself by `execve`, never by exiting** (2026-11-12; §4.5). A harness
  that exited would end the job the shell waits on and orphan the session; a re-exec keeps the
  pid the shell, the terminal and aterm's claim know. The state is versioned text
  (`muse-alab-adopt 1`; 2 since 2026-11-14, below) that the probe proves the new image reads, so a build that changes the
  format answers another line and is never exec'd into; atpkg's build number orders the builds,
  so a downgrade (`current` moved back) never re-execs. The exec uses `execv(3)` directly:
  std's `Command::exec` empties the signal mask first, which would deliver the held signals to
  the old image's handlers and lose them. The verified lane refuted nothing in it, but failed
  two of its rows closed, each fixed at its cause: a `stat` built with `mem::zeroed` (the
  inherited descriptor's type is now read through std, on a duplicate), and the pointer to a
  `sigset_t` that `sigprocmask` reads, whose source Trust could not prove live (on a stack
  local and on a static alike). On macOS the mask is now set through the BSD integer calls
  `sigblock` and `sigsetmask`, which take no pointer; a unit test holds their mask to the set
  `sigaddset` makes over the same signals, and blocks and unblocks it.
- **The safe point is aterm's**: its Muse Code gates G-M1/G-M2 over the session log, its
  120 s person grace, plus a 30 s settle after the turn and a no-descendants check. The stop
  is aterm's: one `SIGTERM`, never `SIGKILL` (a restart aterm asks for is not a move, and
  escalates as aterm's own restart does; see 2026-11-14 below).
- **A clean exit ends the session**, even with an update pending; the next launch starts
  the newest build.
- **Crash resume follows aterm's exit table and aterm's ladder** (the budget of three
  relaunches per ten minutes was replaced on 2026-11-14, below); a clean `session.end` in the
  child's own log part vetoes it, and nothing ever causes one.
- **The journal is a file, never the terminal**; the only terminal output is one line before
  a relaunch, the refusals, and under `run --` the launcher's yolo line before Muse Code starts.
- **No `harness.json`, no environment knobs.** A harness file would advertise a contract
  this release does not speak. Debug builds read `MUSE_ALAB_TEST_TIMING` so the tests run in
  seconds (`<tick>,<look>,<person grace>,<settle>,<stop wait>[,<away budget>]` ms; the stop
  wait is a restart's term grace too, and aterm's ladder pauses are never shortened); release
  builds read nothing.
- **Linux reaches no safe point** (its tty clock is too coarse to be sound).
- **Linux code is type-checked and linted, never run**: on 2026-11-14 stable `cargo clippy
  --all-targets -- -D warnings` (debug and release) passed for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu` from macOS; no test has run on Linux.
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

- **The claim asks aterm's roster first and reads its scope back** (2026-11-13; ported from
  kimi-alab 14b1c0f and claude-code-alab). The harness sends no claim to an aterm whose
  `native-act info` `own-image=` list lacks the exact row `muse-alab:upgrade`: aterm stores a
  contract 3 claim from a holder off its roster as a foreign claim that takes every duty from its
  built-in engine. An `ERR`, an unknown verb, a missing or repeated `own-image=` all read as no
  row, so the launch passes through. After the claim, `status` must name this harness, proven,
  with a `supervisor_scope=` that takes every duty claimed; anything less is let go and passed
  through, and an act whose fresh read finds the claim no longer taking its duty treats the claim
  as lost and makes it again. (Since 2026-11-14 `alab-wire` makes the claim, and this harness
  claims at contract 4 alone, where `alab-wire` asks no roster: aterm's proof reads the registry
  row instead. The roster guard is contract 3's, so this harness never reaches it: since
  alab-harness `v0.7.0` `alab-wire` makes it no contract-3 claim at all; see "The contracts
  are declared to `alab-wire`", below.)
- **aterm's silence never holds the loop, and a busy aterm is not an absent one** (2026-11-13;
  kimi-alab 0dd6fe6, claude-code-alab's `CONNECT_TIMEOUT`/`REQUEST_TIMEOUT`). The loop that reaps
  the child and passes on its signals used to wait up to 5 s per socket operation on aterm, a
  claim made again from the loop included. Now the launch's first claim has 2 s per operation and
  every later exchange 500 ms; one that outlives it drops the connection (a late reply would
  answer the next request), which drops the claim, and it is made again later. A slow `status`
  can therefore lose a claim for one reclaim period; that is fail-closed (moves hold, a crash
  waits for the claim) and kept. A launch whose first claim aterm answered the handshake of and
  then answered late, or not at all, used to `exec` Muse Code in the harness's place for the
  whole session; that is aterm busy. The child now runs supervised with the claim lost, made
  again on the 5 s clock. (This entry also said an `ERR` reply to `harness policy` passes
  through, and that aterm says `ERR` only after a 5 s stall; both were wrong, corrected in the
  next entry.)
- **An `ERR` to `harness policy` or `status` is aterm busy, not a refusal, and a claim that
  could not be given back drops its connection** (2026-11-13; kimi-alab's `read_policy`
  `Failure::Unread`; aterm `crates/aterm-gui/src/control_media.rs` `main_thread_refusal`). aterm
  answers both reads through its main thread and says `ERR` at once while it shows a macOS dialog
  (`ERR aterm is showing a dialog; answer it, then retry`) or while a stall is already detected,
  so passing through on one left Muse Code without this harness for the whole session over a
  dialog. The launch now keeps the child supervised and claims again every 5 s, as for a late
  reply. A refusal is only an `OK` reply whose words admit no claim (no contract 3, the policy
  off) or that cannot be read (a `status` that is not schema 1), a roster with no row, or a claim
  aterm refused. An aterm too old to know the verb answers `ERR` too and so keeps refusing the
  claim while the child runs supervised, which is harmless. A claim that did not read back whole
  is given back; when that release fails (no reply within the bound, or `ERR`), the claim may
  still stand and a late reply would answer the next request, so the connection is dropped,
  which ends both, and the claim is made again on a new one (`LOST`). Before, the loop kept that
  connection as a given-back claim and its next request read the release's reply. (On
  2026-11-14 a policy switched off and a contract this harness does not claim at stopped being
  refusals too; below. The same day an aterm serving 3 and not 4 became one again at the
  launch, since this harness claims 4 alone, and with alab-harness `v0.7.0` so did one serving
  2 alone; see "The contracts are declared to `alab-wire`", below.)
- **Time stopped is not grace** (2026-11-13; kimi-alab 0dd6fe6 `Reap::Continued`). The 15 s a
  child has to end after the harness's `SIGTERM` runs on std's `Instant`, which on macOS stops
  while the machine sleeps, and starts again whenever the child is continued, since a stopped
  child acts on no signal but `SIGKILL`: when the harness continues it after following its stop,
  and when `waitpid(WCONTINUED)` reports a continue the harness never saw a stop for (a whole-job
  `^Z` and the shell's `fg`). Before, a child stopped past the wait was judged to have outlived
  its `SIGTERM` the moment it was continued, and never moved again. A restart aterm asks for
  (2026-11-14, below) waits on the same clock before its `SIGKILL`, and after it.
- **A harness continued alone does not spend its child's stop wait** (2026-11-13). A child
  still stopped is not judged to have outlived its `SIGTERM`: when only the harness is continued
  (`kill -CONT` on its pid), it continues the child on its next pass and the wait starts again
  then. Before, the pass the harness was continued in judged the wait with the time stopped on
  the clock and logged `STOP-UNHONOURED`, and the child was never moved again.
- **No event subscription, back-off ladder or typing grace on a crash relaunch yet**
  (2026-11-13; superseded on 2026-11-14 by the `alab-wire` port, below). The harness never subscribes to aterm's events, so it neither parses a
  `subscribe` ack nor acts on `restart cause=`; a crash is relaunched at once on aterm's fresh
  word, at most three times in ten minutes, whoever typed. aterm's crash judgement, its ladder
  (1 s, 10 s, 60 s, 10 min), no relaunch within the person's typing grace and the
  `restart cause=` escalation (`SIGTERM`, then `SIGKILL` after 5 s for a stall, 30 s otherwise)
  belong to the shared library's tracker packages HX-09, HX-10 and HX-13 and are not built here
  first.
- **The aterm client is `alab-wire`, the harnesses' shared one** (2026-11-14; aterm tracker
  HX-13, after kimi-alab `hx09-alab-wire` and agy-alab `hx12-alab-wire`). `src/link.rs` and the
  socket, claim, policy, roster and read-back code in `src/aterm.rs` (2,090 lines with their
  tests, now 480) are gone; `alab_wire::Link` does all of it, its fake aterm (feature `fake`) is the
  integration tests' aterm, and every fix one harness finds in it reaches all of them.
  `src/aterm.rs` keeps what only this harness knows: its identity (`muse-alab`, vendor `muse`,
  duties `relaunch,upgrade`), its clock (`CLOCK_UPTIME_RAW` on macOS, which stops in sleep as
  std's `Instant` does there; `CLOCK_MONOTONIC` on Linux), the child it announces, the move
  rule below and how notes reach the journal. The dependency is the vendored, pinned copy of
  alab-harness (the next decision).
- **alab-wire is the vendored copy of alab-harness `v0.1.0`, pinned by tree id** (2026-11-14;
  alab-harness decision 19 and its README, "How a harness pins it"). Before, the dependency was
  a path to a sibling checkout (`../alab-harness`), to become a git tag dependency once
  alab-harness had a remote and a tag; aterm's `deny.toml` refuses git sources, and a sibling
  path builds nowhere but on the lab's Mac. Now `vendor/alab-harness/` is that tag's tree, byte
  for byte (alab-harness `tools/pin`), and `vendor/alab-harness.pin` names the tag and its git
  tree id. Both the dependency and the dev-dependency (`fake`) are paths into the copy, and the
  workspace excludes `vendor` and lists no `members`, so the copy stays its own workspace
  (cargo counts every path under a listed member as explicit, so the `members = ["."]` that
  alab-harness's README shows voids the exclude and alab-wire then fails to inherit `publish`
  from this one). `tests/pin.rs` fails when the committed copy is not the pinned tree or the
  working copy changes any of it; alab-harness `tools/check-pin` also compares the pin with
  the tag and is the release check.
  `publish/manifest.txt` lists `vendor`, so a public clone builds alone. To move to a newer tag
  (a `v0.1.x` fixing what HX-06's measurement finds), run `tools/pin` again and commit; the copy
  is never edited here, and a fault in it is fixed in alab-harness and taken as a new tag.
  (The copy is `v0.7.1` since the entry "alab-harness `v0.7.0`, with the session stream off",
  below, and its last line.)
- **`alab-harness.toml` restates aterm's seed row for this harness** (2026-11-14; aterm tracker
  HX-RECORDS-RESTATE-SEEDS). The record aterm's registry reads from the signed package manifest
  says what aterm's compiled seed row already says: id and tool `muse-alab`, no vendor command it
  leads, name `Muse Code`, and no environment prefixes to strip (aterm's default strip list holds
  no Muse prefix, and a record never widens it). Its contracts, 3 and 4 then, are 4 alone since
  the entry below. It ships in the public snapshot.
- **The harness claims contract 4 alone, never 3** (2026-11-14; the harness proofs' plan, D11:
  "Every harness declares `contracts = [4]`. A record-bearing build never claims 3"). Contract 3
  is aterm's compiled seed rows' alone (aterm's interface design §2.2: `read_v3` refuses a row a
  verified record or a link makes), and a 3 in this harness's record would keep aterm serving
  contract 3 for it once aterm stops serving a contract no installed build claims (HX-22). So
  `alab-harness.toml` says `contracts = [4]`, and the claim is held to the same list
  (`aterm::CONTRACTS`; a unit test reads the record and compares). `alab-wire` v0.1.0 builds the
  contract a claim names itself, the highest both sides list (`Contract::SPOKEN`, 4 then 3), and
  takes no list from a harness; its pinned copy is never edited here. Against every aterm that
  serves 4 it claims 4 and nothing changes. Against one that serves 3 and not 4 (aterm main
  before HX-06 serves `2,3`) the harness reads `harness policy` once itself before the claim and
  passes Muse Code through with no claim sent, so aterm's built-in Muse Code lanes keep the tab,
  as for any refusal; where that read goes unanswered, or a lost link is claimed again from an
  aterm that has stopped serving 4, `alab-wire` can still claim at 3, and the harness gives that
  claim back at once and closes the link for the session (a closed link in an aterm tab holds
  every move and passes every crash on, so no duty is done under it). The guard goes once
  `alab-wire` takes the harness's list (an alab-harness change, taken here as a new pin).
  (alab-harness `v0.7.0` closed that gap, and the guard is gone; see "The contracts are
  declared to `alab-wire`", below.)
- **alab-harness `v0.7.0`, with the session stream off** (2026-11-14). The copy moved from
  `v0.1.0` to `v0.7.0` (tree `e4c1f87`) for `Config::with_contracts` (alab-harness Decision 33,
  next entries). The tags between bring the session stream's records and client (`v0.3.0`,
  `v0.4.0`), the relaunch grammar (`v0.4.0`), `alab-policy` (`v0.2.0`, `v0.5.0`), the re-exec
  module (`v0.6.0`), and on the v0.1 path this harness runs on the give-back of a contract-4
  claim aterm may still keep for 45 s after its connection closed (alab-harness Decision 30:
  `Link::release` on a lost link, and a claim attempt aterm refuses inside that grace, send
  `meta unset supervisor`). From `v0.4.0` a contract-4
  grant opens the session stream unless the config says otherwise; this harness says
  `Config::with_stream(false)` (`aterm::config`), so its claim stays on the v0.1 path, renewed
  with `meta set supervisor` and woken by the event subscription, as it was: taking the stream
  is its own package (HX-19). It takes neither `alab-policy` nor the shared re-exec here: its
  own re-exec (`src/adopt.rs`, protocol 2) stays until the package that moves it onto
  `alab_wire::reexec`. `Policy::contract` now takes the declared contracts; its one caller
  here, the launch's own contract read, passed `Contract::SPOKEN` to keep its meaning until the
  next entry deleted that read.
  `v0.7.1` (tree `f336fe1`) followed the same day: no library change, three `alab-wire` test
  races fixed, and `docs/HX-06-aterm-map-2026-11-13.md`, a planning page the copy shipped, gone.
- **The contracts are declared to `alab-wire`, and the harness's own guard is gone**
  (2026-11-14; alab-harness `v0.7.0`, its Decision 33, aterm tracker HX-08b). `alab-wire`
  `v0.7.0` takes the contracts a harness declares (`Config::with_contracts`) and keeps every
  claim to them: the launch, an adoption, each claim made again after a loss and one after a
  policy switched back on. An aterm that serves none of them is `Failure::Unserved` (`policy
  contract=<served> declared=4`), sent no claim and read again every 5 s, and `Link::unclaimed`
  gives a lost link's last answer. That closes the gap "The harness claims contract 4 alone"
  (above) worked around, so `aterm::config` declares `CONTRACTS` (`[4]`, still held equal to
  the record by its unit test) and the workaround is deleted: `aterm::unserved`, the launch's
  own `harness policy` read before the claim, and the give-back in `aterm::settle`, which
  released a claim found at 3 after any link step and closed the link for the session
  (`RELEASED contract=3 …`). `settle` is `aterm::journal` again and only writes the link's
  notes. What stays and what changes:
  - No claim at 3 is ever sent. Before, `alab-wire` made one where the launch's own read could
    not see it coming (a launch whose read went unanswered and then found `2,3`, or a claim
    lost to an aterm restart into `2,3`) and the harness gave it back; now none is made.
  - An aterm whose policy at the launch serves no 4 passes Muse Code through, as one serving
    `2,3` did: `start_under_aterm` reads `Failure::Unserved` from `Link::unclaimed` (nothing
    was claimed, so nothing is given back), and the tab is left to aterm's built-in Muse Code
    lanes. One serving 2 alone now passes through too, where since the port it kept Muse Code
    supervised. `alab-wire` answers the two alike, the same built-in lanes take either tab, and
    telling them apart would mean reading `alab-wire`'s journal text. "The harness claims
    contract 4 alone" split them only because keeping the child under `2,3` meant a contract-3
    claim, a reason that went with the gap. Main passed 2 alone through before the port, and
    agy-alab passes through on `Failure::Unserved` at launch. An aterm updated to serve 4 during
    such a session is taken up at the next launch, as the `2,3` case already was.
  - After the launch, a link that finds no 4 (a busy launch answered `2,3` later, or a claim
    lost to an aterm restart into `2,3`) stays lost and is claimed at 4 once aterm serves it,
    where before the given-back claim closed the link for the session and nothing was claimed
    again. Meanwhile every move holds (`no-claim`); a crash on a link that never held a claim
    is passed on at once, and so is one on a link that lost its claim once aterm has answered a
    claim made again that it serves no 4 (`NO-RELAUNCH aterm's word: policy contract=2,3
    declared=4`; see "A crash once aterm serves no 4 is passed on at once", below).
  - Tests: the unit test `no_contract_3_claim_is_ever_sent`, against `alab-wire`'s fake (a
    launch, its reclaims and an adoption against `2,3` and against 2 alone send no claim and no
    roster read; the claim is made at 4 once served and again at 4 alone after a restart into
    `2,3`; `alab-wire`'s default contracts claim at 3 in the same scene, as the control),
    replaces `the_claim_is_held_to_contract_4`; the integration test
    `under_aterm_no_contract_3_claim_is_ever_sent` (a busy launch answered `2,3`, then a
    restart into `2,3`, each claimed at 4 only once served) replaces
    `under_aterm_a_contract_3_claim_is_given_back_at_once`; and the pass-through test's
    contract cases are `2,3` and 2 alone, the second moved from the busy-or-off test.
- **A crash once aterm serves no 4 is passed on at once** (2026-11-14, from the review of the
  entry above, 9b4ecfc). With the guard gone, a claim at 4 lost to an aterm restart into one
  serving `2,3` leaves the link lost and claimed (`Link::claims`), so a crash went to the crash
  wait, where `relaunch_word` answers every lost link `Away`. It waited the whole 45 s away
  budget, journalled `CRASH-WAIT aterm did not answer (link-lost)` and then `NO-RELAUNCH
  claim-not-proven-again link-lost: aterm did not answer within 45000 ms`, though aterm
  answered `policy contract=2,3 declared=4` every 5 s: the fault that "In an aterm tab, a link
  that never held a claim moves nothing and relaunches nothing" fixed for an `off` answer on a
  link that never claimed. Before,
  that scene closed the link and the crash was passed on at once (`NO-RELAUNCH no-claim`). The
  crash wait now reads `Link::unclaimed` when aterm's word is `Away`: `Failure::Unserved` is
  aterm's last word on a lost link (no hold it stated stands over it, in `alab-wire`'s own
  words), and no claim can be made until an aterm update, which comes as another restart, so
  the crash is passed on at once (`NO-RELAUNCH aterm's word: policy contract=2,3 declared=4`).
  A crash in the 5 s after the loss, before any claim was made again, waits until the wait's
  own reclaim brings that answer, then is passed on. A refused or `off` answer to a claim made
  again keeps the wait, unchanged here: the away tests read a refusal of a link that held a
  claim (no registry row proving it yet) as aterm not yet back, and `alab-wire` keeps such a
  link lost and claims it again, unlike one that never claimed. A policy-off release had no
  such fault: its link stays held with no standing claim, which `relaunch_word` denies
  (`no-claim`). Test: `under_aterm_a_crash_once_aterm_serves_no_4_is_passed_on_at_once` (a
  crash after aterm's `2,3` answer and one before it; with the check removed the harness does
  not exit within the test's 30 s).
- **`run` does what atpkg's launcher did for `muse`** (2026-11-14; aterm harness interface design
  §2.1, package HX-13a). The launcher will hand the word to this harness as `run -- "$@"`, so
  `run` takes exactly that form: a first word `run` is always the harness's (Muse Code has no
  `run`), and one without the `--` right after it is refused with exit 2 rather than guessed at,
  since no launcher writes it. Of the launcher's blocks for `muse` only the yolo default exists
  (the row has no self-update verb, no sign-in gate and no shim variable), and it is carried
  byte for byte from the launcher's shell block, not from atpkg's Rust `yolo::decide`: the two
  differ where a flag of the default is typed in its `=value` form after the first word
  (`exec --disable-approval=1`), on which the block adds the flags and `decide` runs as typed,
  and the block is what the launcher runs. The differential test against the laid block found
  that difference. The block runs after the recursion guards, which refuse before anything
  else, and before the store is read, as the launcher prints its line before it execs. A `run`
  with the harness turned off (`--no-harness`, `ATERM_NO_HARNESS`) runs the blocks all the same
  and then passes Muse Code through with the arguments they left: the launcher's `--no-harness`
  only sets `ATERM_NO_HARNESS=1`, and its yolo block runs regardless (aterm
  `platform/mod_sh_shim_tests.rs:790-793`). The design's list says the default stands "unless
  the launch says `--no-harness`", but the launcher's code is what a launch gets today, and the
  design's own "`--no-harness` gets its own variable, which skips only the harness" agrees with
  it. The launcher hands off every `ATERM_NO_HARNESS` but `1`, which this harness reads as off,
  so `ATERM_NO_HARNESS=true muse` would otherwise have lost the default once atpkg drops its
  block (the review of e99d64b found it; this branch's first commit ran no block when off).
  `muse --no-harness` itself never reaches `run` (the launcher sets the variable and runs Muse
  Code), so it keeps the default only if atpkg, when it drops its block, hands such a launch to
  `run` as well, which then runs the block and passes through; that is aterm's half to decide.
  `muse-alab` typed by its own name runs no block, as before: the blocks belong to the
  word `muse`. While atpkg still carries its block (until the signed index pins the release that
  ships this one, and aterm deletes it), the harness sees a launch the block already decided, and
  the flags it added are posture markers, so nothing is applied twice; the unit test checks that
  on every argv of its corpus.
- **A crash follows aterm's ladder, not a budget of three in ten minutes** (2026-11-14). The
  pauses are aterm's own (`alab_wire::relaunch::Ladder`: 1 s, 10 s, 60 s, then 10 min; a child
  that ran 10 minutes since its last crash relaunch starts over), so a crash loop slows to one
  try in ten minutes instead of handing the session back after the third, and aterm's built-in
  lane and this harness pace a crashing Muse Code alike. A crash is also judged by aterm's exit
  rule as well as the measured table (`abnormal_exit`): an end by `SIGINT`, `SIGQUIT`, `SIGHUP`
  or `SIGTERM`, or their `128+n` codes, is no crash. The ladder runs outside aterm too, with no
  word to wait for. The pauses are never shortened, even in tests: the one integration test
  that climbs it waits the real 1 s and 10 s.
- **Under aterm a crash waits on aterm's word within aterm's own away grace** (2026-11-14). It
  asks `relaunch_word` once at the crash and again when the pause is over; an away or
  answers-later aterm is asked again every second for at most 45 s from the crash
  (`AwayBudget`), then the crash is passed on. In a tab this harness leads, no built-in lane of
  aterm's brings the child back, so passing it on at once would leave the session to nobody;
  waiting forever would keep a tab whose person closed aterm's claim. A harness that is under
  aterm but never held a claim (a link aterm has not admitted yet, or one it refused) relaunches
  nothing and passes the crash on at once: aterm never let it lead the tab. A `SIGINT`, `SIGQUIT`, `SIGHUP` or `SIGTERM` reaching the harness during the wait
  ends it (no child runs to take one, so it means the person wants out).
- **A restart aterm asks for is made, and escalates as aterm's does** (2026-11-14). `cause=asked`
  and `cause=stall` end the child with `SIGTERM` at once and resume the same session; `SIGKILL`
  follows after 5 s for a stall (aterm's stall remedy) and 30 s otherwise (aterm's restart act's
  deadline), on the clock that does not count sleep or time stopped. A move's `SIGTERM` is never
  followed by one: a move is the harness's own choice, a restart is aterm's order. A restart that
  names a model resumes on `--model=<word>`, Muse Code's root model option in aterm's measured
  flag table (`Arity::One`, dropped from the resume argv and so never doubled), placed before
  `resume`; this was not measured on a live Muse Code. An account move is refused and the child
  runs on: Muse Code has no account home or environment this harness could set. A restart waits
  at most 45 s for a witnessed session and aterm's word, and one still waiting when a child
  starts is passed over before the new child is announced: aterm asked about a child that is
  gone.
- **The claim names each child it starts** (2026-11-14). After every start (first launch, crash
  relaunch, move, restart) and at adoption the harness announces the claim again with
  `vendor=<build>` and, under contract 4, `child=<pid>:<kernel start time>`, so aterm's restart
  and stall acts aim at the child that runs, not at a pid the kernel may have reused.
- **A move needs the `upgrade` grant** (2026-11-14). A person can cap this harness to `relaunch`
  (`NARROWED`); its vendor moves, its own re-exec and relaunching onto a newer build then all
  wait (`WAIT upgrade-not-granted`), and a crash or restart stays on the build the child ran.
  The move's word is `alab-wire`'s `judge_own_move` for a harness judging by its own readings,
  then this harness's own two: `human_ms` against the longer of aterm's `human_grace_s` and its
  own grace (aterm's rule alone would let a shorter aterm grace cut the harness's 120 s), and aterm's
  composer reading where it serves one.
- **An aterm switched off for now, or not serving this harness's contract, keeps the child
  supervised** (2026-11-14; `alab-wire`'s `Failure::Off` and `Failure::Unserved`). Before, a policy
  off by a setting (`autopilot-off`, `disabled`, `agent-off`) or with no contract 3 passed the
  launch through, so switching Autopilot back on found no harness in the tab for the session's
  life. Now Muse Code runs as the child with the claim lost and made again every 5 s, which
  aterm admits once it is on. Only what no later read can change passes through: a launch that
  is not the TUI, an unhosted or headless window, no registry row or roster row, a claim aterm
  did not prove, and a grant of less than both duties (given back at once, so aterm's built-in
  engine keeps the duty the grant left out). Since "The harness claims contract 4 alone"
  (above) one more passes through though a later read could change it: an aterm whose
  `harness policy` at the launch serves 3 and not 4 (aterm main's `2,3`). Kept as the child,
  Muse Code would gain nothing: the claim `alab-wire` makes there is at 3, which the harness
  gives back at once, closing the link for the session, while passing through leaves the tab to
  aterm's built-in Muse Code lanes, which serve that aterm. An aterm updated to serve 4 during
  the session is therefore taken up only at the next launch. "Not serving this harness's
  contract" in this entry's title is now an aterm serving no contract `alab-wire` speaks (2
  alone); a contract-3 claim made later in the session (a lost link claimed again from an aterm
  that stopped serving 4) is given back and closes the link for the rest of the session, as that
  entry says. (With alab-harness `v0.7.0` an aterm serving 2 alone at the launch passes through
  as well, and no contract-3 claim is made later; see "The contracts are declared to
  `alab-wire`", above.)
- **The adopt state is protocol 2, with no reading of protocol 1** (2026-11-14). It carries the
  ladder (`ladder <streak> <since ms|->`) and whether the old image had a link (`link
  claims|absent`) where protocol 1 carried crash times and aterm's socket directory and session;
  the new image finds aterm again from its own environment. A protocol 1 image's probe answers
  another line, so neither side execs into the other; the first move onto a build of this format
  happens at the next launch, as for any format change.
- **The claim is not given back at exit, and the version is not bumped** (2026-11-14), as in
  kimi-alab and agy-alab: aterm sees the connection close with the process and its own
  adopted-claim grace covers a re-exec; the port changes no published interface.
- **In an aterm tab, a link that never held a claim moves nothing and relaunches nothing**
  (2026-11-14, from the review of the port, 290a8e5). The port judged the link by `claims()`
  alone, so a launch aterm answered busy, off for now or not serving a contract this harness
  claims at (a lost link with no claim yet), and one aterm refused after that (absent), counted
  as outside aterm: with Autopilot off, atpkg's newer Muse Code was moved onto, and a newer
  muse-alab re-exec'd into, with no grant and no read of aterm. The move rule now takes whether
  the harness runs in an aterm tab. There, only a standing claim with `upgrade` lets a move be
  weighed, and every lost link and an absent one hold it (`no-claim`); outside aterm the
  harness's own readings decide as before. A crash on such a link waited aterm's 45 s away grace
  (`relaunch_word` answers any lost link `Away`), journalled as aterm not answering, though aterm
  answered `off` every 5 s. It is now passed on at once (`NO-RELAUNCH no-claim`), as kimi-alab
  passes on a crash on `!claims()`. An aterm serving contract 2 alone still runs Muse Code
  supervised (the entry above), so its crashes are passed on by this harness where main passed
  the launch through to aterm's built-in Muse Code lane; whether that lane brings the tab back
  after the harness ends is aterm's to say and was not measured here. (Since alab-harness
  `v0.7.0` such a launch passes through again, as main's did; see "The contracts are declared
  to `alab-wire`", above.)
- **The crash wait sleeps until its next ask** (2026-11-14, same review). Each pass's wait was
  capped at the ladder's pause even after the pause was over, so with aterm away (asked again
  each second) it waited zero and spun until aterm came back, up to 45 s: the review measured
  0.16 s of CPU a second on a loaded machine, and it would take a core on an idle one. The cap
  now holds only while the pause runs (`crash_sleep`, unit-tested), and the end-to-end test
  measures the harness's CPU over 2 s of the wait (under a tenth of it).
- **Keys typed while a crash waits are dropped** (2026-11-14, same review; kimi-alab's
  `tty::discard_input`). The crash wait puts the terminal back in its starting cooked, echoing
  mode for up to the ladder's 10 minutes or aterm's 45 s, and a line typed at the frozen screen
  stayed queued: on a relaunch it reached the resumed Muse Code as a prompt, and on a crash passed
  on the parent shell read it as a command (keys typed in the pause also make aterm's `human_ms`
  deny the relaunch, so typing "retry⏎" could itself pass the crash on and run `retry`). The
  harness now flushes the terminal's input (`tcflush(TCIFLUSH)`, only while its job owns the
  foreground) just before a crash relaunch starts and before it ends for a crash it waited on. A
  crash passed on at once, before any wait, leaves type-ahead alone, as kimi-alab does.
- **A waiting restart's limit is judged before aterm is asked again** (2026-11-14, same review).
  The 45 s limit was checked only after aterm answered that the restart still waits, so the first
  pass after a long gap (the job stopped with `^Z`, which `Instant` counts) asked aterm first, and
  a permitted restart ended the child for a request minutes old.

## Testing

`tests/harness.rs` runs the built binary on a pseudo-terminal in its own session over a
sandboxed store whose `muse` is a fake Muse Code — the test binary itself, which keeps a
session the way Muse Code does (lock, session log, run records), reads the terminal in raw
mode only when it is readable, and takes commands from a control file. It covers argument
and terminal passthrough, exit codes and signal deaths, signal forwarding, ignores the
caller set (`nohup`), following a stop, the move onto a newer build with the resume argv in
the same harness process (after the new build's probe), the holds (open turn, running tool,
draft, typing left unread, unresumable launch, a build that does not start, a child that
ignores `SIGTERM`), crash resume on aterm's ladder (the real 1 s and 10 s pauses) and its
fallback to the build it ran, a broken stderr during a relaunch, the refusals, and the
recursion guard (a store `muse` that runs the harness again, by `exec`, as a shell's child, and
under `--no-harness`). Its aterm is `alab-wire`'s fake (`alab_wire::fake::FakeAterm`, the
dev-dependency's `fake` feature), the one kimi-alab and agy-alab test against, answering real
aterm's shapes on a private socket. Against it the tests cover: the pass-through where no claim
is proven (no socket, no registry or roster row, an unproven claim, a narrower grant given back,
an unhosted window, an aterm serving no contract 4);
an aterm busy (`ERR` while it shows a dialog, a late reply) or off for now keeping the child
supervised and claiming later; no contract-3 claim ever sent, after a busy launch answered `2,3`
or a restart into `2,3`, and the claim made at 4 once aterm serves it; a crash passed on at once
once aterm, restarted, serves no 4; the claim given back while
the policy is off and made again once it is on; the claim held with each child announced
(`child=`) and a crash relaunched on the ladder; a crash passed on when aterm says relaunch is off, waited out while aterm is away and
relaunched once the claim is proven again, and passed on once aterm stays away past the budget;
a crash passed on at once when aterm never admitted the claim; the crash wait's CPU cost while
aterm is away; keys typed while a crash waits reaching neither the resumed child nor the parent
shell (the harness run as a shell's command); a restart aterm asks for resuming the session, a
stall restart killing a child deaf to `SIGTERM`, a restart that waits a bounded time and is
refused, one never made past its limit after the harness was stopped, and one passed over while
its child is being ended, refused when its child crashes, or passed over when a moved child
refuses to start and the harness falls back; a move waiting for the `upgrade` grant, while the
claim is lost, given back, not admitted yet or refused, and on aterm's `composer=` (and on the
input clock where aterm serves none); and the
harness's own re-exec onto a newer build of itself: the same pid running the new image, the
child untouched, the episode carried (a later move resumes the session on a terminal put back to
the first image's mode), a build that does not answer the probe never exec'd into, and under
aterm the move held by aterm's word and the claim made again under the same pid. A child stopped
past its `SIGTERM` wait gets the whole wait again once continued.

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
  Rerun on 2026-11-13 after the roster, scope, bound and stopped-grace changes: 67 unit and 36
  integration tests pass; `targo tippy --locked --all-targets -- -D warnings` and
  `targo fmt --check` pass. Each new integration test fails with its fix removed. The verified
  lane was not rerun on that change.
  Rerun on 2026-11-13 after the busy-`ERR`, dropped-release and continued-alone changes: 69 unit
  and 39 integration tests pass, `targo tippy` and `targo fmt --check` pass, and each of the two
  new unit tests and three new integration tests fails with its fix removed. The verified lane
  was not rerun on that change.
  Rerun on 2026-11-14 after the `alab-wire` port: 64 unit and 43 integration tests pass (three
  full runs in a row; one earlier run lost `refuses_an_incomplete_build` once to an empty
  pseudo-terminal read, a path that never reaches aterm), `targo tippy --locked --all-targets --
  -D warnings` and `targo fmt --check` pass, and `alab-wire`'s own 131 tests pass at alab-harness
  97aff86. The verified lane was not rerun on that change.
  Rerun on 2026-11-14 after the review's four fixes (never-claimed links, the crash wait's sleep,
  dropped keys, the restart limit): 66 unit and 50 integration tests pass in three full runs in a
  row at load 8 to 15, `targo tippy --locked --all-targets -- -D warnings` and `targo fmt --check`
  pass. With `src/` at 290a8e5 and the new tests, the crash wait used 300 ms of CPU in 2 s, the
  shell read the line typed during the wait (`shell read [retry]`), the resumed child submitted
  it (`submit retry`), a crash before aterm admitted the claim waited past the test's 30 s, a
  move with Autopilot off at the launch was made, and a restart was made after its limit; each
  test fails so, and the new `crash_sleep` unit test fails on the old cap. alab-wire is
  unchanged. The verified lane was not rerun on that change.
  Rerun on 2026-11-14 on the vendored alab-harness `v0.1.0` (tree `f3b718d`), which carries
  alab-harness c6db9af, 97aff86, d64ada4 and de80de5 (a release aterm will not take ends the
  connection, a test build may shorten the away budget, the policy read on the clock while the
  event stream is down, placeholder paths): 66 unit, 50 integration and 1 pin test pass with no
  harness change, `targo tippy --locked --workspace --all-targets -- -D warnings` and
  `targo fmt --check` pass, and alab-harness `tools/check-pin` says the checkout carries
  `v0.1.0` exactly. The pin test fails on a byte appended to the copy. The verified lane was
  not rerun on that change.
  Rerun on 2026-11-14 after `run --` and the launcher's blocks (HX-13a): 69 unit, 52
  integration and 1 pin test pass; with contract 4 alone on top: 71 unit, 53 integration and 1
  pin test pass, `targo tippy --locked --workspace --all-targets -- -D warnings` and
  `targo fmt --check` pass, and alab-harness `tools/check-pin` says the checkout carries `v0.1.0`
  exactly. `run_takes_the_launchers_double_dash` and `run_sets_the_yolo_default` fail on the
  previous `src/main.rs`; the byte-for-byte test fails when a default flag's `=value` form is
  read as a posture marker, as atpkg's `yolo::decide` reads it; the contract tests fail with
  the guard removed. The verified lane was not rerun on that change.
  Rerun on 2026-11-14 after the review of those two commits (`run` runs the yolo block with the
  harness off; the corpus names every word of the block): 72 unit, 53 integration and 1 pin
  test pass, `targo tippy --locked --workspace --all-targets -- -D warnings` and
  `targo fmt --check` pass, and alab-harness `tools/check-pin` says the checkout carries `v0.1.0`
  exactly. `run_sets_the_yolo_default` fails on the previous `src/main.rs`; the byte-for-byte
  test fails with `--sandbox-network`, `logout` or `--disable-shell` misspelled in
  `src/launcher.rs`, and `the_table_names_every_word_of_the_block` with `logout`, or the value
  form of `--permission-profile=`, dropped from its list. The verified lane was not rerun on
  that change.
  Rerun on 2026-11-14 on the vendored alab-harness `v0.7.0` (tree `e4c1f87`): with the pin
  alone (the stream off, the launch's contract read passing `Contract::SPOKEN`) 72 unit, 53
  integration and 1 pin test pass; with contract 4 declared to `alab-wire` and the guard
  deleted 72 unit, 53 integration and 1 pin test pass, `targo tippy --locked --workspace
  --all-targets -- -D warnings` and `targo fmt --check` pass, and alab-harness
  `tools/check-pin` says the checkout carries `v0.7.0` exactly. Without
  `Config::with_contracts` in `aterm::config`, `no_contract_3_claim_is_ever_sent`,
  `the_record_and_the_claim_name_contract_4_alone`,
  `under_aterm_passes_through_where_no_claim_is_proven` and
  `under_aterm_no_contract_3_claim_is_ever_sent` fail; without the pass-through on
  `Failure::Unserved` in `start_under_aterm`, `under_aterm_passes_through_where_no_claim_is_proven`
  fails (`aterm-contract-3: Muse Code replaced the harness`). The verified lane was not rerun
  on that change.
  Rerun on 2026-11-14 after the review of that change (a crash once aterm serves no 4 is passed
  on at once): 72 unit, 54 integration and 1 pin test pass, `targo tippy --locked --workspace
  --all-targets -- -D warnings` and `targo fmt --check` pass, and alab-harness
  `tools/check-pin` says the checkout carries `v0.7.0` exactly. Without the `Failure::Unserved`
  check in the crash wait, `under_aterm_a_crash_once_aterm_serves_no_4_is_passed_on_at_once`
  fails (the harness does not exit within 30 s; with the away budget shortened to 12 s the
  journal says `CRASH-WAIT aterm did not answer (link-lost)` and `NO-RELAUNCH
  claim-not-proven-again link-lost: aterm did not answer within 12000 ms`). The verified lane
  was not rerun on that change.
- `targo tippy --locked --workspace --all-targets -- -D warnings` (which rejects a planted
  `len_zero`) and `targo fmt --check` pass.
- Every review fix has a test that fails on the first commit (07dd78f) and passes here, and
  each was mutation-checked: removing the fix fails its test.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
