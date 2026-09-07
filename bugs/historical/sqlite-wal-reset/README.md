# SQLite 3.7.0–3.51.2 — committed pages lost when a WAL reset races a checkpoint

## The bug

In WAL mode, a checkpoint copies frames from the WAL file into the database
file and records how far it got in the `nBackfill` field of the shared
wal-index. When a writer starts a transaction and finds the WAL fully
checkpointed with no reader still using it, it resets the WAL: new salt, frame
count zero, `nBackfill` zero, and the new transaction's frames go to the start
of the file. The checkpointer takes the checkpoint lock and reads the wal-index
header before it tests `nBackfill < mxFrame`; the writer's reset takes the
reader locks and never the checkpoint lock, so it can land between those two
steps. The checkpointer then sees `nBackfill` zero against the old
generation's `mxFrame`, copies with the stale frame count, and leaves
`nBackfill` at the old count. Every later checkpoint skips the new
generation's frames below that count, so the transactions written there never
reach the database file. The loss surfaces when the WAL is next reset: the
table pages are gone while the index pages that reference them were copied,
and `integrity_check` fails.

- **Affected**: every release from 3.7.0 (2010-07-21) through 3.51.2
  (2026-01-09), per the SQLite documentation.
- **Fix**: 3.51.3 (2026-03-13). After taking read-lock 0, the checkpointer
  compares the live wal-index salt with the header it read and skips the
  checkpoint when they differ (`walCheckpoint` in `wal.c`). Backports: 3.50.7
  (check-in `c7facf7ac58d2cda`) and 3.44.6 (check-in `863c171b76cd36e0`).
- **Primary sources**: the SQLite documentation's
  [WAL-reset bug section](https://sqlite.org/wal.html#walresetbug) with its
  six-step description; the
  [3.51.3 release log](https://sqlite.org/releaselog/3_51_3.html); Tailscale's
  [account](https://tailscale.com/blog/sqlite-wal-reset-bug) of the outages,
  the VFS tracing shim that caught the race, and the aggressive manual
  checkpointing that exposed it. Upstream could reproduce it only through a
  test-control callback that fires the write at the right instant; a later
  [reproducer](https://theconsensus.dev/p/2026/08/23/another-look-at-sqlite-wal-reset.html)
  and an [Antithesis run](https://antithesis.com/blog/2026/wal-reset-bug/)
  reach it with concurrent writes and checkpoints alone.

## The triple

- **Workload**: SQLite 3.51.2 built from the official amalgamation with no
  source patching. Two processes on one WAL-mode database: a checkpointer
  that runs `PASSIVE` checkpoints back to back, and a writer that alternates
  a large commit with a small one and journals the largest id each commit
  acknowledged. Automatic checkpoints are off, so every checkpoint runs
  against a writer in another process. The sizes alternate because a stale
  checkpoint loses frames only when the generation it copies is shorter than
  the one its frame count came from: a small commit that resets the WAL
  after a large one leaves the frames between the two sizes uncopied.
- **Fault surface**: pause-at-Moment on the checkpointer. A `SIGSTOP` that
  lands between the checkpointer's header read and its backfill test, while
  the writer resets the WAL and keeps committing, is the upstream sequence
  exactly. Kill and restart of either process, and a burst hook that grows
  the WAL past the stale count, widen the schedules the searcher can reach.
- **Oracle**: a verify hook runs a `TRUNCATE` checkpoint, so only the database
  file answers afterwards, then `PRAGMA integrity_check` (`@always 1`) and a
  read-back of committed rows (`@always 2`): the row count must equal the
  largest id, and that id must cover the journal.

## Difficulty / knobs

- The window is a few hundred instructions wide, but the checkpointer's loop
  is mostly window whenever the WAL is fully checkpointed, so a preemption of
  the checkpointer at a random instant lands in it often. Knobs:
  `faultlab.rows` (rows in the small commit, default 20) and
  `faultlab.big_rows` (rows in the large commit, default 2000) set how many
  frames a stale checkpoint leaves behind; `faultlab.gap_us` (pause between
  commits, default 500) is where a checkpoint completes and the next commit
  resets the WAL; `faultlab.burst` (rows in hook 1, default 2000) grows the
  WAL in one step.
- **Nominal control**: the same two processes under the no-fault driver, then
  the verify hook — must never trip the oracle on 3.51.3.
- **Version control**: the same input on SQLite 3.51.3 — must never trip the
  oracle.

## Workload as built

Built by `consonance/harmony-linux/linux/build-faultlab-image.sh` into
`initramfs-faultlab-sqlite-3.51.2.cpio.gz` and, for the control arm,
`initramfs-faultlab-sqlite-3.51.3.cpio.gz`. Each carries one static binary,
`faultlab-sqlite`, compiled from `faultlab-sqlite.c` against that release's
amalgamation. The tarballs are pinned by sha256 in `versions.lock`; the
`sqlite3.c` inside each matches the SHA3-256 its release log publishes.

Boot with `rdinit=/sqlite-init` (3.51.2) or `rdinit=/sqlite-control-init`
(3.51.3). Bundle `/bundle/sqlite-<version>`:

| item | what it runs |
|---|---|
| node 0 `checkpointer` | `PASSIVE` checkpoints without pause |
| node 1 `writer` | `BEGIN IMMEDIATE`, `faultlab.rows` inserts, `COMMIT`, journal the last id, pause `faultlab.gap_us`; then the same with `faultlab.big_rows` |
| ready | the table answers a count |
| hook 1 | one commit of `faultlab.burst` rows |
| hook 2 | `TRUNCATE` checkpoint, `integrity_check`, journal read-back |

The database, its WAL and the journal live on tmpfs. The bug is a race over
the shared wal-index, and nothing in it depends on what reaches a disk.

The guest kernel is `bzImage-faultlab`, built single-processor by
`build-faultlab-kernel.sh` (`x86-faultlab-config-fragment`). This bundle is
what forced that: on the SMP build of the same kernel, every hook the fault
agent started froze the guest. The host delivers the guest's timer interrupt
only at VM exits it services, and the kernel's rwsem writer path spins
until the scheduler tick when it finds the lock reserved for a queued
waiter. Three processes writing one tmpfs file reach that state as soon as
a hook's connection joins the writer and the checkpointer: a holder yields
at the preemption point in the page-cache write loop, the next queues, and
the third spins for a tick that a non-exiting guest never gets. Traced on
the host, the frozen guest exited only for `PAUSE` and host interrupts, its
kernel stack read `pwrite64 → shmem_file_write_iter → down_write`, and the
semaphore was free with its waiter and handoff bits set. Without SMP the
kernel has no
optimistic spinning, the writer queues, and the guest idles to the timer.
Kills and pauses never froze the guest, and the no-fault control driver
ran every hook to completion, which is why campaign runs alone showed it.

## Instrumented arm

The Antithesis run reached the bug with SQLite's own source marked at the
states that matter and a pause that lands on an instruction. This arm
rebuilds that experiment. `initramfs-faultlab-ant-sqlite-3.51.2.cpio.gz`
and its 3.51.3 control carry `faultlab-sqlite-ant`, built by
`build-faultlab-image.sh` the way the Antithesis Dockerfile builds theirs:
`-O1 -g -fsanitize-coverage=trace-pc -DSQLITE_DEBUG
-DSQLITE_ENABLE_ANTITHESIS`, static, from the amalgamation with
`faultlab-sqlite-ant-markers.diff` (their twelve `ANT_REACH` markers in
`wal.c`) and `faultlab-sqlite-ant-header-marker.diff` (two more markers
right after `walIndexReadHdr` in `sqlite3WalCheckpoint`, which is the start
of the window: "checkpoint: read wal-index header" on every checkpoint,
and "checkpoint: header shows WAL fully backfilled" when the copied header
has `nBackfill` equal to a non-zero `mxFrame`, the state a writer's reset
needs). The fix in 3.51.3
rewrote the checkpoint write loop that three of their markers sit in, so
the control carries nine of the twelve.

`faultlab-sqlite-ant.c` is their workload: two identical writer processes,
each looping over a 70% write transaction (a `randomblob` insert, a counter
update, a churn update, and a `progress` row that records the writer's
committed sequence), a 20% checkpoint (`PASSIVE` 70%, `RESTART` 15%,
`TRUNCATE` 10%, `FULL` 5%) and a 10% sweep (count, max, `quick_check` or
`integrity_check`), with `wal_autocheckpoint=200`, `busy_timeout=2000` and
`synchronous=NORMAL`. Their draws come from `/dev/urandom` so their platform
can steer them; here they come from a seed (`faultlab.seed`), so the
schedule is the only thing a run varies. A write that returns
`SQLITE_CORRUPT`, or a sweep that finds the database corrupt, ends the
process with `abort()` and writes the failed check next to the database.

The pause comes from `faultlab-edge.c`. The `-fsanitize-coverage=trace-pc`
build calls `__sanitizer_cov_trace_pc` at every control-flow edge, the
runtime counts the calls, and at the edge named by `faultlab.edge<writer>`,
or at the `faultlab.hdr<writer>`-th passage of the fully-backfilled marker,
it sleeps `faultlab.edge_sleep_us` microseconds (50 ms by default). The sleep
is the pause: the other writer runs while this one stands still between the
header read and the backfill test. Edge and passage counts are a function
of the binary and the schedule, so a number names the same place on every
run of one image and a different place on any other build.
`faultlab.reach_all` logs every marker passage with its process id.

Boot with `rdinit=/ant-sqlite-init` (3.51.2) or
`rdinit=/ant-sqlite-control-init` (3.51.3). Bundle `/bundle/ant-sqlite`:

| item | what it runs |
|---|---|
| node 0 `writer0` | the workload loop, writer id 0 |
| node 1 `writer1` | the workload loop, writer id 1 |
| ready | the `progress` table answers |
| hook 1 | `TRUNCATE` checkpoint, `integrity_check` (`@always 1`), committed rows match each writer's `progress` row (`@always 2`), no writer failed an in-process check (`@always 3`) |

## Status

Natively on the build host with both processes pinned to one core, the way
the single-vCPU guest schedules them, 3.51.2 fails `integrity_check` within
three seconds in three of three trials (`btreeInitPage() returns error code
11` on runs of table pages, rows missing from the count) and 3.51.3 passes
three of three. Across cores the same processes run clean on both: a
`PASSIVE` checkpoint gives up on the reader lock while the writer's
transaction is open, so the stale checkpoint needs the writer to finish its
commit before the checkpointer runs again, which one core gives it for free.

On the single-processor kernel, probes on stock and patched KVM run every
hook input to completion, including six hooks in twelve windows, and two
boots of one input seal at the same virtual time and reach the same moment
and tick count.

Nominal control on the patched KVM (nested L1 guest, single-processor
kernel, no-fault driver, verify hook twice): two runs of each version stop
at the same guest virtual time, produce byte-identical serial output and
the same state hash, and the oracle stays silent (`@always 1 1`,
`@always 2 1`).

| image | stop | serial fingerprint | state hash |
|---|---|---|---|
| 3.51.2 | `Moment(790566426)` | `9c223af6c7261a44` (16284 bytes) | `3e6dfeb3…` |
| 3.51.3 | `Moment(780556426)` | `ae659fc870674e17` (16308 bytes) | `a6e6aa43…` |

A searcher campaign on the patched KVM (250 ms horizon, 8 workers, single-
processor kernel) found nothing in 3200 executions, with every hook
finishing and no guest abandoned, and was stopped once the measurement
below showed why. A
pause is a `SIGSTOP` from the fault agent, which stops the checkpointer at
the point where the scheduler last took the CPU from it. A diagnostic hook
that attaches to the checkpointer with `ptrace`, which stops it the same
way, sampled that point 600 times during the workload and found it inside a
system call every time: `pread64` 251, `pwrite64` 237, `fcntl` 109, the
rest `fstat` and `fsync`. In this guest the scheduler takes the CPU from
the checkpointer only on the way out of a system call. The window between
the header read and the backfill test is user code with no system call in
it, so no pause reaches it, and more executions cannot change that. The
searcher needs a pause that stops a process at an instruction rather than
at a kernel entry, which the VMM does not offer yet. The instrumented arm
supplies that pause from inside the workload.

**Reproduced with the instrumented arm.** Writer 0 paused for 50 ms at its
15th checkpoint header read (`faultlab.hdr0=15`, edge 6008558 of the
3.51.2 image with sha256 `c715d9ff…`), the other writer free to run: both
writers then report `SQLITE_CORRUPT`, `quick_check` finds
`btreeInitPage() returns error code 11` on pages 98 to 100, both abort on
their in-process checks and the fault agent restarts them. Three of three
repeats of that input stop at the same edge with the same failure. The same
input on the 3.51.3 image runs clean three of three. Because the pause
point is an edge count, a hit names one image: rebuilding the workload with
the `@always 3` check moved the 15th header read to another edge, and the
input no longer hit. A fresh sweep of forty header reads on the rebuilt
image found nothing. The header read alone is a poor aim: most
checkpoints find frames to copy, and the race needs one that finds none.

Aimed at the fully-backfilled marker instead, the sweep over the first
forty passages on writer 0 flags 8 of 40 on 3.51.2 (passages 1, 2, 3, 7,
9, 16, 22 and 39) and 0 of 40 on 3.51.3, with every verify hook finishing.
In each flagged run both writers fail their own checks (`SQLITE_CORRUPT`
on a write, a failed `integrity_check`, or a restarted writer finding
committed rows gone), the fault agent restarts them, and the verify hook
then fails `integrity_check` (`@always 1`) and reports the writer failures
(`@always 3`).

This is a reproduction given the window, since the marker the pause aims
at encodes the state the race needs. The searcher has not found that
window on its own: a pause drawn uniformly over edges lands in a
thirty-instruction window about once in a hundred thousand draws, and the
twelve Antithesis markers mark states around the window and none inside
it. Their platform enters it through randomized preemption of the whole
system, so the searcher gets the same thing as a fault: `Jitter` names a
node, a seed, a mean edge count between pauses and a pause length, and for
its window the node's edge runtime pauses itself at seeded-random edges
(`FAULTLAB_JITTER_FILE`, written by the fault agent for a
`Fault::ProcJitter` window). The searcher draws it with the other actions,
over both writers, 256 seeds, three densities and three lengths, and the
markers are not consulted for placement.

A pause is a yield loop timed by the monotonic clock, because the guest
timer rounds a sleep up to its ten-millisecond tick and a pause that long
starves the node. Measured on the instrumented writers, which run about
four million edges a second: a one-millisecond hold ends at the next
scheduling boundary, two to three milliseconds later, and at one pause
every two to five thousand edges a window holds about ninety pauses with
the node still running for a fifth of it.

**Found by the searcher.** A campaign on the instrumented 3.51.2 with the
whole action vocabulary and no knobs (250 ms horizon, 8 workers, campaign
seed 1, stop preregistered at 3000 executions or 90 minutes) found the bug
at execution 1624, 33 minutes in. The input is eight actions: restart
writer 0 twice, run the verify hook, wait, run it again, pause writer 0
for five ticks, run it again, wait. Inside the pause window both writers
die on their own checks, one on `SQLITE_CORRUPT` from a write and one on
`integrity_check`, and the verify hook that follows reports both
(`@always 3`). Two replays of the input with the probe stop at the same
tick with the same failures, and the same input on the 3.51.3 image runs
every hook to completion with no death and no violation. No jitter window
is in the input: the pause is an ordinary `SIGSTOP`. The marker log of the
found run shows where it landed: writer 0, fresh from its second restart,
printed its first "header shows WAL fully backfilled" line and was stopped
on the next tick. That line is a write system call inside the window, made
by the marker this arm adds, and a restarted process prints it again. So
the searcher found the schedule, and the instrumentation's own output
supplied the preemption point. The same input on an image carrying only
the twelve Antithesis markers, whose prints all sit outside the window,
runs clean. The control campaign on 3.51.3 with the same settings ran
2000 executions with no violation. A campaign on the twelve-marker image,
same settings and stop rule, ran 3000 executions in 54 minutes and found
nothing. With their instrumentation alone, the only route into the window
is randomized preemption, and from the pilot's counts a jitter window
lands a pause in it about twice in a thousand tries, so the expected first
hit is near five thousand executions and a run of three thousand is close
to even odds. A second campaign on the twelve-marker image with 24
workers, 512 MiB guests, and a stop preregistered at 9000 executions or
120 minutes ran all 9000 in 64 minutes and found nothing.

The parts of that estimate that had never been measured were then
measured on the instrumented image. A pause at the k-th fully-backfilled
header read of writer 0 (`faultlab.hdr0=k`) hits 3 of the first 12 reads
with a 2 ms hold and 2 of 12 with a 50 ms hold, and around a read that
hits, a pause anywhere from a thousand edges before it to five thousand
after corrupts about half the time (16 of 28 offsets at 50 ms, 12 of 20 at
2 ms). So an instruction-level pause at a random moment corrupts about once
in five thousand tries, and the alphabet has no such pause. A `SIGSTOP`
stops at a kernel entry. A jitter window does not reach the state at all:
in 1600 windows over the instrumented image, each holding writer 0 about
thirteen times for 2 ms and logging every pause against the latest
fully-backfilled read, writer 0 made no fully-backfilled read while its
window was open, though it makes one every few hundred milliseconds
otherwise, and no window corrupted anything. 2760 windows on the
twelve-marker image, at spacings from twenty thousand to two hundred
thousand edges, corrupted nothing either. A third campaign on the
twelve-marker image with 24 workers and a stop preregistered at 100000
executions ran them all in 45 minutes and found nothing, which under these
measurements is the expected outcome.

The instrumented image also corrupts with no fault at all in some boot
configurations. The first print of a marker in each process is a write
system call inside the window, and whether the scheduler switches writers
there depends on the phase the kernel command line sets: with
`faultlab.hdr1=999999`, which never pauses, every run corrupts at 0.98 s,
and the 3.51.3 control is clean under the same command line. The searcher's
find on the instrumented image, where a restarted writer printed its first
line inside the window, and the sweeps around the first read are of that
kind. The pause-only hits at the third, fourth and sixth reads are not: no
print sits near them. Reaching the window on the twelve-marker image needs
a pause that stops the checkpointer at an instruction from outside the
workload.

A hardware execution breakpoint gives that pause without touching the
workload. The guest kernel's task park (`/dev/harmony-park`, patch
`0006-x86-harmony-task-park`) arms a per-thread execution breakpoint on the
node's process group at a user address, counts the hits itself, and makes the
thread that takes the k-th hit sleep on its own return to user mode, before
the instruction runs, until the guest clock passes the hold. The fault agent
arms it for a `Fault::ProcPark` window; the searcher's `Park` action names
the node, the address, the hit and the hold, and draws addresses from the
binary's line table (`faultlab-places.sh`). The place used here is the
instruction after the checkpointer's header read, the return address of the
`walIndexReadHdr` call in `sqlite3WalCheckpoint`.

A first version of the same pause, a `ptrace` helper in the guest holding
the trapped process with a 2 ms yield loop, corrupted 6 of 80 runs on the
twelve-marker 3.51.2 image sweeping k from 1 to 80 (k = 11, 28, 37, 47, 55,
62) and 0 of 80 on 3.51.3. The kernel park under the same run shape, one
6 s window with the park armed then hook 1, corrupts 3 of 80 on 3.51.2
(k = 15 and 70 lose committed writes, k = 73 fails the `walCheckpoint`
assertion) and 0 of 80 on 3.51.3. Every park stops at the armed address,
holds land between 2 and 22 ms because the release waits for the next
system call of another task or the periodic tick, and replaying a k gives
the same result each time. Under the searcher's 250 ms windows the same
park at hits 1 and 2 in each of 24 windows reaches the instruction 36 times
in 48 runs and corrupts nothing on either version: the run history is the
same in every window, and the window the bug needs is open at none of those
checkpoints. Reaching it is the search's job, over windows, hits and holds.
