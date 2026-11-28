# Faults workload

`faults-workload` searches a distributed workload for bugs that only appear
when nodes die, stall, or restart at the wrong moment. The workload runs
unmodified inside one deterministic VM; every fault is enforced from inside the
guest by the platform supervisor, and the whole run is a Consonance session, so
a bug reproduces from its action list alone.

## The image contract

A workload image is an OCI image carrying one extra file, `/etc/harmony/bundle`,
in the platform supervisor's bundle format:

| line | meaning |
|---|---|
| `node <name> <argv...>` | one workload process the supervisor supervises |
| `hook <id> <argv...>` | a command the search can run at any moment |
| `heal <id>` | names the declared hook that lifts every fault the other hooks leave in force, such as a network partition |
| `setup <argv...>` | runs once, before any node starts |
| `ready <argv...>` | must pass before setup is sealed and before new hooks launch after a supervised node start |
| `workload <argv...>` | one long-lived workload driver started after initial readiness |
| `check <argv...>` | a short-lived oracle command run continuously after initial readiness |
| `disk <node> <path> <size> [checksummed]` | gives that node an ext4 filesystem at `path`, sized `16M` through `1G` in whole 4 KiB pages, which power-loss and disk actions act on |

A `disk` line names a node declared by a `node` line, and each node has at most
one disk. The image must contain `mkfs.ext4`. The `checksummed` flag declares
that the node's software checksums the data it stores there and claims to
detect a corrupted block. Only a checksummed disk draws `DiskCorrupt`. A flipped
bit breaks a durable-write check on any storage that does not checksum its
data, so a finding from it on an unflagged disk says nothing about the software. The
[supervisor README](../../consonance/harmony-linux/supervisor/README.md#node-disks)
describes how the disk is built, replayed and faulted.

[`prepare`](src/prepare.rs) stages that image, packs its rootfs, reads the packed
bundle for the action alphabet, and selects `/etc/harmony/bundle` for structured
supervision. The platform control
member contains the read-only execution specification and mounts the pinned
supervisor, SDK devices, and bundle path. The supervisor runs setup and
readiness commands with the resolved image credentials before it publishes the
setup point, then owns the node process groups and hook launches.

After setup, readiness probes run asynchronously while standing-fault polling
continues. The configured command decides readiness, including whether it can
operate with some nodes down. Already-running hooks continue reporting their
assertions across restarts; readiness only checks new launches. Bundles without
a readiness command keep immediate hook launches.

## Image admission

Preparation scans every executable and shared library in the exact packed
rootfs segment that the guest receives ([`admission`](src/admission.rs)). It
derives the bundle and event attestation from those same archived bytes. A
staged file changed between scanning and packing cannot make admission name
different executable or bundle bytes from the guest image.
The scan decodes each distinct executable digest once, largest first, on every
CPU the process may use, and reports files in path order.

Packaging temporarily grants host read and directory search permission while
copying staged files, then restores their original modes. The archived scan
reads those copied bytes regardless of mode, so images containing mode-zero
files and directories receive the same admission result on privileged and
unprivileged hosts. The separate image-inspection command still inspects the
staged rootfs directly.

The scan rejects:

- a writable executable segment or an executable stack;
- an ELF file that is not 64-bit little-endian x86-64 or arm64;
- a hardware entropy or CPU-identity instruction (RDRAND, RDSEED, RNDR,
  RNDRRS, RDPID, LSL) without a review in `/etc/harmony/instruction-allowlist`
  ([format](../languages/reviewed/README.md));
- an arm64 monitor-based LL/SC instruction, even if listed for review;
- an arm64 executable load that contains an allocated non-executable section;
- a review entry that matches no instruction in the image.

Native arithmetic and standard math libraries are admitted for both VM and UML
images without instruction reviews. Reciprocal/reciprocal-square-root estimates
and x87 transcendental operations may produce different result bits on different
CPU implementations. Image admission does not prove a common numerical
transition rule across CPUs. Saved CLI replay warns about changed or unknown CPU
implementations through [CPU provenance](../../cli/README.md); exact agreement
still needs evidence for the particular implementation
pair, artifacts and inputs.

For User-mode Linux, preparation rejects the scanned hardware entropy and
CPU-identity instructions even when reviewed. UML runs guest user instructions
natively, so the VM's hidden feature bits do not protect a guarded call site.
It also refuses `XBEGIN` and valid `XACQUIRE` instructions before execution,
including uninstrumented images and reviewed sites. RTM and HLE can abort for
unmodeled cache, resource and asynchronous conditions; one guest CPU does not
make their outcome deterministic. The rule applies to both fault-search
preparation and ordinary application preparation from the archived rootfs.
It does not add an instruction-review requirement for these sites in VM mode.

The entry-only rule follows Intel's
[transactional instruction definitions](https://cdrdv2-public.intel.com/671143/334569-sdm-vol-2d.pdf)
and [asynchronous-abort guidance](https://www.intel.com/content/www/us/en/developer/articles/technical/software-security-guidance/technical-documentation/intel-tsx-asynchronous-abort.html).
`XEND`, `XABORT`, `XTEST`, `XRELEASE`, `XSUSLDTRK` and `XRESLDTRK` do not start
a transaction and remain admitted. Outside a transaction, their supporting
CPUs respectively fault, perform no abort, report inactive, release normally,
or leave tracking unchanged. This is not a claim that unsupported CPUs execute
them instead of raising an instruction exception. `XACQUIRE` classification
requires a valid memory operation, including implicit-lock `XCHG` or a supported
explicit `LOCK` form; ordinary REP, PAUSE, SSE and `XRELEASE` prefixes remain
admitted. The scan still requires cooperative control flow and no generated code.

Production preparation applies the x86 xstate policy to the exact packed ELF
bytes for both VM and UML execution, including ordinary uninstrumented runs.
FXSAVE/XSAVE-family saves are refused independently of digest/address review.
Only the complete straight-line glibc eager-resolver and unused-TLSdesc shapes
receive the loader exception; the save operand, matching restore, mask/reload sequence,
predecessors, alternate entries and overlapping decodes all participate in the
check. TLSdesc and text relocations anywhere in the scanned x86 ELF closure are
refused. XGETBV requires a unique nonoverlapping straight-line proof that ECX
is zero, bounded to 32 predecessor instructions. The proof follows full-width
register copies, respects partial-register and CPUID clobbers, and permits
conditional branches only when no observed entry bypasses the proof. LOOP-family
instructions, calls and unknown operations stop the proof. Legacy resolver
FXSAVE/FXRSTOR requires its complete 16-byte-aligned, 0x240-byte frame; the
XSAVE variants retain the matching mask and 64-byte alignment rules. Restore operations remain
admitted, and native standard math is unchanged. The Linux fixture compares
compiled FXSAVE, XSAVE, selector-1, proven selector-0, restore, math and ordinary
statically linked libc programs
through both production preparation modes and the Python checker, retaining
exact ELF bytes and both decisions. The ordinary libc positive runs in VM mode;
installed glibc can contain transaction entry already refused by UML admission.

This is a static cooperative-code boundary. It does not enforce future
executable mappings, mutable files, generated code or instruction preservation
under engine WriteMemory/XorMemory effects. Those effects can change code even
when the original image was admitted. Ordinary logical XSAVE/cache identity
continues to require the documented instruction-domain assumptions; the scan
is not a runtime domain-preservation or exact-replay qualification certificate.
A retained development-host KVM probe passed static admission, decoded FXSAVE
into an RW mapping, changed the mapping to RX and executed it successfully.
That is a confirmed instruction-domain escape, not a replay-divergence witness.
Arbitrary RAM effects and dynamic execution remain outside the maintained
static contract even after the production/checker alignment.

The stub's TSC trap does not trap RDPID or LSL. RDPID returns the host CPU's
identity, and LSL can read the CPU number from the host GDT segment limit.
This admission check walks executable loads in the packed image, including
file bytes mapped before a segment's virtual start and beyond its declared
file size in the first and last executable pages. It also rejects loads whose
file offset and virtual address disagree within a page. Section headers do
not determine what the guest loader executes.
The x86 scan follows instruction streams from each mapped section and gap
across section and adjacent executable-load boundaries. It starts at each
load's declared address, restarts at direct near-branch targets in any
executable load of the same ELF, including across unmapped gaps, and rejects
overlapping executable mappings, which could combine instruction bytes from
different loads. The scan still requires
cooperative workloads: dynamically generated code and indirect transfers into
another instruction's operand bytes are outside its guarantee.
The checked-in x86 Bookworm images carry reviewed libstdc++ and libgcrypt
entropy sites, so UML refuses those images even if a particular workload
would not call them. Build a UML image without the sites before searching or
running it; the review entries remain valid for VM execution. Saved UML runs
created before this admission rule cannot be replayed or branched because their
guest initramfs has no admission evidence.

Each line of `/symbols/harmony-instrumented-events` holds a SHA-256 and an
absolute image path, and must match that file. Event actions are enabled when
this attestation passes, `/usr/lib/libvoidstar.so` exists, and `/symbols` holds
a non-empty `*.sym.tsv`. An image without an attestation still runs the other
fault actions.

The scan catches entropy and CPU-identity instructions that compilers and
assemblers emit. On x86 it decodes each allocated section inside an executable
load from its start, regardless of the section's executable flag, and scans
the gaps separately. Each decode continues through later ranges when their
virtual addresses are contiguous. This preserves instruction alignment when a
segment begins with headers or other data while finding an instruction reached
by falling through a boundary. Direct near jumps, calls, and conditional
branches also start fresh decodes at their targets, including targets within
an operand that the section-start stream treats as data. At each mapped-section
or adjacent executable-load boundary, it also decodes the first complete
instruction from every candidate
start within the preceding 14 bytes. Entropy and CPU-identity instructions
whose prefixes, operands, SIB bytes, or displacements cross a boundary become
exact-address review sites. A VM image can review such a site against its
executable digest; UML rejects native host-input sites even when reviewed.
Executable pages can contain data that resembles an instruction at a boundary,
so those bytes may need explicit VM review. Overlapping x86 executable mappings
are rejected. On arm64 it checks aligned
instruction words across the complete 4 KiB page-rounded file mapping of each
executable load, including bytes beyond the declared load size, and rejects
another load segment that aliases an executable memory page. Images with
read-only data in executable loads must link with separate code pages. A
trailing partial word with no mapped bytes to complete it cannot hold an
instruction. The LL/SC mask excludes LSE CASP and ordinary acquire/release
loads, which do not use the reservation monitor. A binary built to hide an
x86 instruction inside another instruction's operand bytes can still pass.

`harmony prepare IMAGE` prints the scan, the attestation, and the
instrumented files, and exits nonzero unless the image is a complete
instrumented target.

## Actions

Every action records its own duration in 10 ms guest ticks, from 10 ms through
10.24 seconds, and the next action starts when that duration ends
([`target`](src/target.rs)). Each action's window lasts its duration, except a
`Hook`'s, which can outlast it, as described below. The
search draws one duration per suffix from the adaptive duration policy, and
every action in the suffix takes it. Half of those draws repeat the parent's
last duration while jobs from the parent still keep states, so a lineage keeps
one fault rate across jobs. Job length follows the suffix reach in the
[searcher README](../../dissonance/searcher/README.md). It starts at 64
actions and stays there while most actions open slots no state has held
before. Fault actions often do, because the edge bucket is part of the holder
identity. With every fault kind, later actions of long jobs open fewer novel
slots, and most jobs return to one action. A worker ends its job at the
search's wall-time limit, as the searcher README describes.
An action's planned cost is its guest ticks plus 256 ticks for the host work
around it, so planned finishes follow wall time. On etcd with KVM, a job takes
about 0.18 seconds of wall time per action plus 0.076 seconds per guest second,
so an action's host work costs as much as about 240 ticks of guest time.
Each action also records a coverage
quantum: the number of coverage callbacks an instrumented thread makes between
VM exits during the action's window. It is drawn log-uniform over powers of two
from 1 through 32768, and it is part of the action's archive key. Virtual time
per callback is the same at every quantum, so the quantum sets how soon after a
timer deadline a busy thread exits, and leaves CPU speed unchanged. A thread
with no scheduling choice exits only near its next timer deadline, so a small
quantum costs no more wall time than a large one. Replay
serves the same quantum, so busy-loop scheduling replays along with fault
timing:

| action | effect |
|---|---|
| `Wait(ticks)` | the workload runs undisturbed |
| `EndFaults(ticks)` | every standing window still open at the action's start closes there, including each `EventKill` window, fired or not, which disarms a kill that has not fired; the workload then runs undisturbed; never drawn, only applied by the settle phase |
| `EventKill(node, rarity, ticks)` | an instrumented runtime kills the node at a selected event, reporting the claimed site before termination |
| `EventPark(node, edges, hold, ticks, target)` | an instrumented runtime holds the thread whose instrumented edge brings the count since arming to `edges`, for the hold, then counts again from zero until the window closes; each edge counts one over its site's visit count, or one under a target one offset wide, and an optional target range of module offsets limits the count to edges whose site falls in it; `edges` is drawn log-uniform from 1 through `(1 << 14) - 1`, and the hold is drawn log-uniform in whole ticks from one tick through the window |
| `Kill(node, ticks)` | the node is killed when the window opens and started when it closes |
| `KillAll(ticks)` | every node goes down, as in a host crash, and every node starts when the window closes; drawn only for bundles with more than one node |
| `PowerLoss(node, ticks)` | the node goes down for the window as in `Kill`, and its disk keeps only the writes that reached it before its last cache flush, plus a prefix of the later writes chosen by the action's `choice`; drawn only for a node with a `disk` line |
| `PowerLossAll(ticks)` | every node goes down for the window as in `KillAll`, and every disk loses its unflushed writes the same way, each with its own prefix; drawn only for bundles with a `disk` line and more than one node |
| `DiskError(node, ticks)` | every read, write and flush on the node's disk fails with `EIO` for the window; drawn only for a node with a `disk` line |
| `DiskStall(node, ticks)` | every request to the node's disk waits until the window closes; drawn only for a node with a `disk` line |
| `DiskCorrupt(node, ticks)` | the node is killed, one bit flips in a block it wrote since its disk was last cut, and it comes back after a quarter of the window, as in `Restart`; drawn only for a node whose `disk` line has the `checksummed` flag |
| `Pause(node, ticks)` | the node is stopped for the window, then continued; a role pause's window starts with the role lead |
| `PauseParked(node, edges, hold, ticks, target)` | a parked pause (ours): the node is stopped for `ticks`, and from the resume a park counts for another `ticks`, so the action's window is twice `ticks`, plus the role lead for a role pause; the park counts one per edge from the resume, so the thread whose `edges`-th counted edge after the resume comes is held for the hold, with the optional target as in `EventPark`; never drawn on its own, only in place of a `Pause` under `resume_park` |
| `Restart(node, ticks)` | the node is killed and comes back after a quarter of the window, at least one tick |
| `Hook(id, ticks)` | the supervisor runs that hook once when its window opens; the window lasts the action's duration, or with the `hold_hooks` option its duration times 1, 2, 4 or 8, set by the top two bits of the action's `choice`, so it can overlap the actions after it; in a bundle with a `heal` line, every other hook is a cut, and when the last open cut window closes the supervisor runs the heal hook if a cut is still in force |
| `Clock(node, offset_ms, rate_ppm, ticks)` | the node's wall clock steps by the offset and then moves by the rate in parts per million of guest time for the window, then returns to guest time; drawn only for VM execution |
| `ClockAll(offset_ms, rate_ppm, ticks)` | every node's wall clock shifts as in `Clock`, each by a factor in [-1, 1] that the action's `choice` sets per node; drawn only for VM execution with more than one node |

Every fault that takes a node down brings it back when its window closes, so
one action holds the fault, its hold and the heal, and the next action starts
with every such node running or starting. The hold is
the action's duration, which the adaptive duration policy draws. The package
installs a kill as a process restart window over the whole action window, and
a power loss as a power-loss window plus a restart window over the same span.
Two adjacent kills of one node join into one longer hold.

With the `hold_hooks` option, a network cut is the exception: it can stay in
force while later actions run. Clients commonly give up on a request after tens
of seconds, such as rqlite's 30-second forward timeout. A cut shorter than such
a timeout delays the reply, and only a longer one makes the client retry. A
held `Hook` window therefore lasts up to eight action durations, at most 81.92
seconds, while the timeline still advances by each action's own duration. A cut
stays in force while any cut window is open. A drawn heal hook lifts every cut at once. When the last cut
window closes, the supervisor heals as before.

This follows Antithesis's documented fault model
(https://antithesis.com/docs/environment/fault_injection/fault_types/):
- each fault is a window that expires on its own;
- faults are scheduled independently of one another and can overlap;
- a restore fault ends every network fault at once.

The parts that are ours:
- the window is the action's duration times 2^j, with j from 0 through 3;
- j comes from the top two bits of `choice`;
- the longest window is 81.92 seconds.

The hold is part of the action through `choice`, so a search that redraws
retained actions redraws their holds with them.

Each drawn job also takes a draw profile from its mutation seed, as in swarm
testing. The profile keeps each offered fault kind with probability one half,
or every offered kind when it keeps none. It weighs `Wait` against each kept
kind by a power of two from 1 through 64, drawn uniformly over the exponents.
Every action of the job draws its kind from that profile. A job with a heavy
`Wait` weight leaves quiet time between faults, so the check can run while the
cluster recovers. A job with weight 1 draws `Wait` as one more kind.

With the `hooks_replace_waits` option, in half of the jobs, chosen by the top
bit of the profile's kind word, the `Wait` weight draws `Hook` instead, so the
job draws no `Wait`. Every other kept kind keeps its one share of the same
total, so its rate per action does not change. Only how often a network fault
stands while it runs changes. This follows Antithesis's fault model, in which
faults are scheduled independently and overlap. A vocabulary that offers no
hook draws the same with the option or without it. A single duration serves
every action of a suffix, and most guest time is spent in actions of 5 to 10
seconds, so without these jobs a hook is rare. A network
cut long enough to outlast a client timeout then seldom stands while a request
is in flight.

The cost is quiet time. With rqlite's 13 offered kinds, `Wait` falls from 54%
of the drawn actions to 27%, and hooks rise from 4% to 31%, so more checks
run while a cut is in force. A search execution's last checks run at the
endpoint of its last action, with no settle. A replay settles until a check
starts and finishes in one disturbance generation with no pending process
fault. An unhealed cut is not a pending process fault, so the settlement does
not wait for hook windows and can end while a held cut still stands; this is
tracked in [#993](https://github.com/pH14/harmony/issues/993). A check
that starts under a standing cut sees `HARMONY_FAULTS_IN_FORCE=1` (see the
[supervisor README](../../consonance/harmony-linux/supervisor/README.md)), and
the harness kit then skips its replica comparison and does not fail it, so a
divergence that only that comparison shows is checked only in the stretches
between cuts. Even between cuts, the comparison misses a write that one node
applied alone once its key is written again, for a target whose copies carry
no per-key metadata; this is tracked in
[#1032](https://github.com/pH14/harmony/issues/1032).

A drawn clock action changes either the offset or the rate. The offset is a
power of two from 1 ms through 2^18 ms (about 4.4 minutes) with a random sign.
The rate is 1,000 ppm times a power of two from 1 through 4,096, with a
random sign. A negative rate at or below -1,000,000 ppm becomes a stopped
clock. The
shift covers CLOCK_REALTIME, CLOCK_REALTIME_COARSE and CLOCK_TAI, which Go's
`time.Now` wall reading, `gettimeofday` and `time` use. CLOCK_MONOTONIC stays
on guest time, so Go's monotonic durations and timers are unchanged. The
platform supervisor applies the shift through a per-node time namespace with a
realtime offset (see [`patches`](../../consonance/harmony-linux/linux/patches)).
UML kernels have no time namespaces, so a UML vocabulary has the clock
capability off and the search never draws a clock action there.

A search can leave out fault kinds. The faults package option
`exclude_faults` (in `[workload.options]` of `harmony.toml`) lists them by
name: `kill`, `kill_all`, `pause`, `restart`, `hook`, `power_loss`,
`power_loss_all`, `disk_error`, `disk_stall`, `disk_corrupt`, `event_kill`,
`event_park`, `clock` and `clock_all`. The search then never draws them. `Wait` is always drawn. The
search records the excluded kinds in its vocabulary identifier, so a resumed
search keeps them. A run of recorded actions records the same vocabulary, so a
search from a branch of the run leaves them out too. A replay of recorded
actions runs every action it is given.

Two more options in `[workload.options]` change how hooks are drawn and how
long they stand. Each is off unless the workload sets it to `true`:
- `hooks_replace_waits` gives the `Wait` weight to `Hook` in half of the jobs,
  as described above.
- `hold_hooks` lets a `Hook` window last 1, 2, 4 or 8 times its action's
  duration.

The search records each option that is on in its vocabulary identifier, and a
run records the same vocabulary, so a resumed search and a search from a
branch of a run keep it. The identifier names no option when both are off.
The target identity includes `hold_hooks`, so held and unheld executions share
no cached snapshot. A replay holds hooks as its config says, so a held
search's actions replay as searched only under the search's config.

The option `untried_first_nodes` turns on the `fault.node` choice site
described below. It is off unless the workload sets it to `true`, the
vocabulary identifier names it when it is on, and with it off a search draws
exactly what it draws without the site.

Both hook options are off by default because both take away the quiet time that a
readiness property needs. Garage's `workload becomes ready within 30 seconds
after faults end` fails only when the workload stays not ready through 30
seconds with no fault in force, and the supervisor counts an open hook window
as a fault in force. With `hooks_replace_waits`, the drawn part of half of the
jobs has no `Wait`. With `hold_hooks`, each cut window lasts up to seven more
action durations. Three Garage seeds with both options on found that failure
once, after 21 executions and 1,780.5 guest seconds, and a fault or hook
window was open for 86% of the finding's input. Three seeds with both off
found it twice, after 2 and 9 executions (122.9 and 777.0 guest seconds),
with windows open for 58% and 65% of the inputs. This is a mechanism read
from the code and those three findings, not a measured effect. All six seeds
ran with `ram_mib = 1024`, below the 2048 that the
[harness README](../harness/README.md#garage) requires, and guest
out-of-memory kills ([#992](https://github.com/pH14/harmony/issues/992))
confound one seed in each arm. The runs also do not separate the two options.
rqlite's searches turn both on
([harness README](../harness/README.md#rqlite)).

The option `resume_park`, a list of sites, is off when absent. When it is
present, every `Pause(node, ticks)` in a drawn suffix on a node whose runtime
announced readiness is drawn as a `PauseParked` on that node with the same
`ticks`, coverage quantum and `choice`, and a retained `PauseParked` on a node
whose runtime has not announced readiness becomes a plain `Pause`. With sites
listed, the park aims at one of them, picked uniformly, with `edges` 1. With
an empty list it counts every edge, with `edges` `2^j` for `j` drawn uniformly
from 0 through 10. Its hold is drawn as for `EventPark`. These draws use their
own generator seeded from the job's mutation seed, so every other action of
the suffix is the one drawn without the option. The option needs an image
with the instrumented event runtime and `pause` among the drawn kinds, and the
vocabulary identifier records it with its sites.

A parked pause is one action, so an expansion still returns one action per
draw and the search's suffix limit counts it once: no limit can separate the
pause from its park. Its standing windows are the pause for the first half of
its window and an event park with the at-resume flag for the second half; a
role pause's first window also holds the lead before its pause.
Under `resume_park` the host adds a `park-page` line to the supervisor bundle
at preparation, unless the bundle already has one, so every node process gets
the park page. The line goes into the bundle the host was given or, when it was
given none, into a copy of the image's own, which costs one more preparation
pass; without `resume_park` preparation is unchanged. A `run` or `branch`
whose action input holds a `PauseParked` is refused unless its bundle has the
line. The supervisor writes the park to the page while the node is stopped and
continues it after the write, so the first instrumented visit after the resume
counts toward the park
([supervisor README](../../consonance/harmony-linux/supervisor/README.md#parked-pause),
[runtime README](runtime/README.md#park-page)). A replay repeats it. Shrinking
offers three simpler forms of a `PauseParked(t)`: a `Wait` over its whole
window (`2t`, plus the lead for a role pause) when that fits a wait's tick
count, the plain `Pause(node, t)`, and a `PauseParked(t/2)`. The shrink cost
counts a parked pause's whole window.

A parked pause can still run as a plain pause: the supervisor writes no record
when the node has not stopped within its wait or has exited, and a node whose
instrumented process is not the spawned process never adopts one. Each parked
pause therefore ends with one supervisor SDK line naming its outcome (`fired`,
`expired`, `unadopted`, `cleared_in_batch`, `not_stopped`, `not_running` or
`retired`, defined in the supervisor README). The host keeps each line with
the observation it arrived in, trimmed to the action like park landings, and
`parked_pauses` in `campaign-summary.json` and the archive report counts them
by outcome; the map is absent when no parked pause ran.

A bundle with a `role` line draws role pauses (ours) in place of node-indexed
ones. Every drawn `Pause`, and so every `PauseParked` made from one under
`resume_park`, targets the reserved node id `ROLE_NODE` (`u16::MAX - 1`, shown
as `Pause[65534, …]`) instead of the node the draw picked; the draw still
consumes the same random words, so every other drawn action of the suffix and
the pause's length are the ones a bundle without the line draws. Parks and holds
come from a separate stream that each pause made parked consumes, so where a
node-indexed pause would be parked and the role pause is not (some node not
yet ready), the later parks and holds of that suffix differ.
A role pause action starts with a lead of `ROLE_LEAD_TICKS` (128 supervisor
ticks) before its drawn pause, so its duration is the lead plus what the same
node-indexed action takes. Its pause window stands from the action's start and
carries the drawn length, so the supervisor resolves the node in the guest
during the lead, by running the line's command, and pauses it at the end of the
window for the drawn length, less any time the answer took past the lead. An
`EndFaults` that cuts the window shortens the carried length by the same
amount, so a cut inside the lead leaves no pause. A pause the supervisor cannot
resolve runs as a wait of the whole action's length ([supervisor README](../../consonance/harmony-linux/supervisor/README.md#role-pause)).
The duration policy identifier is `adaptive_action_ticks_v4`.
A role `Pause` becomes a `PauseParked` only when every node's runtime has
announced readiness, because the host cannot know which node the guest will
name. Kills, restarts, disk and clock faults keep their node index. The
vocabulary identifier records the line as the option `role_pauses`. Each
resolution's SDK line is kept with its observation as `role_pauses`, the
resolved node or none, trimmed to the action like parked-pause outcomes.

Without `resume_park` the host draws what it drew before the parked pause:
the draw-path tests pin the default draw path's suffix digests and a pause's
key and encodings, and a default search with this host on a given guest and
image writes the same job and settle lines as one with the previous host. A
default search's guest timeline on a guest that carries the park page code is
not shown unchanged, and comparing searches cannot show it: a guest and image
rebuilt from unchanged sources in another directory already move a search's
timeline from its first execution (#1141).

Every action also records a 64-bit `choice`, drawn when the search draws the
action and kept when a retained action is reused. Each action starts with a
branch, and the branch reseeds the host-served SDK entropy stream
(`fuzz_get_random`, read through `/dev/harmony`) from the campaign's guest
seed mixed with that choice. A workload driver that takes its decisions from
that stream therefore makes different decisions in each continuation of a
snapshot, and replay installs the same choices. The choice is part of the
action's key, so two choices never share a cached snapshot. A choice of zero
keeps the unmixed seed. A power-loss action also uses its choice to pick how
many unflushed writes each disk keeps; a choice of zero keeps none. A
`DiskCorrupt` action uses its choice to pick the write, byte and bit it
corrupts. The guest kernel's `getrandom` state is restored with
the snapshot and is not reseeded, so decisions drawn from it repeat across
continuations.

A guest driver's `sdk.Below(site, n)` asks a choice under fault-policy's
`CHOICE_NAMESPACE` ([harness kit](../harness/README.md)), and the package
answers it (ours). The faults workload offers the searcher the choice site
`guest`, and with `untried_first_nodes` a second site, `fault.node`, so every
drawn job carries the searcher's `ChoicePlan` with its CONTRARY rate, hold
probability and untried ordinal. The `guest` site declares
no rate of its own (`ChoiceSite::with_contrary`), so the plan's `declared`
list is empty. Because a guest asks at
moments and with option counts only the guest knows, and an action must stay
`Copy`, an action stores the plan rather than its answers: `execute_job`
stamps each freshly drawn suffix action with `GuestChoices` (the plan's two
rates, an answer seed mixed from the plan's seed and the action's index, and
the untried ordinal on the first action it stamps). A suffix action copied from
the archive, by an empirical step or a splice, keeps its recorded
`GuestChoices` as it keeps its 64-bit `choice`, so it replays its recorded
answers and still matches its archived twin when the search skips a duplicate
draw. The question is not a local
service, so it stops at an explicit decision. `run_action` answers it with the
searcher's `Chooser` built from the action's `GuestChoices`, numbering the
guest's sites in the order the action first asks them, and hold memory starts
empty at each action. An action without `GuestChoices`, such as one from a
recording made before this field existed, answers every choice with the guest's
own draw, so old recordings replay unchanged. Replay asks the same `Chooser`
again, and a deterministic guest asks the same questions, so it regenerates the
same answers. The standing handler answers the namespace `Nominal`, so the
debug terminal takes every default. In text, an action without `GuestChoices`
omits the field; the binary encoding that history digests and snapshot records
use always carries it.

The `fault.node` site (ours) is the target node of a fault, asked on the host.
Its labels are node ids, as in the action text form `Pause[1, …]`, because
the vocabulary keeps no node names. A freshly drawn `Kill`, `Pause`, `PauseParked`,
`Restart` or `Clock` action offers every node, `PowerLoss`, `DiskError` and
`DiskStall` offer the nodes with a disk, and `DiskCorrupt` offers the
checksummed ones. A role pause offers no node, so it is never asked and writes
no row. An event kind keeps its drawn node, because the node comes
from runtime readiness. A `PauseParked` offers every node, and its park moves
with the answer; after the node is resolved, a `PauseParked` on a node whose
runtime had not announced readiness at the parent runs as a plain `Pause`, as
does one a splice brings to such a node. `execute_job` resolves each such action
before it stamps `GuestChoices`, with one host `Chooser` per job built from
the plan with hold zero. It calls `Chooser::draw` over the offer with the
lowest offered node as the default and weight one on the node the draw
picked and zero on the others. The draw is uniform over the offer, so the
lowest node is the default only because `Chooser::draw` takes one, and the
site is declared with `ChoiceSite::without_default`. The first ask of a job
therefore takes the node at the plan's untried ordinal in its parent's
permutation of the offer while one remains untried: the first such job at a
parent takes a uniform node, as the draw does, and jobs at one parent whose
first actions have the same offer take distinct nodes while their ordinals
are below the offer's size. Kinds with different offers share the parent's
count and each walks its own permutation, so a `Pause` and a `DiskError` can
take the same node before either offer is exhausted, and ordinals spent on
disk faults can leave later `Kill` jobs past the end of their offer, where
they keep their drawn node. Every other ask answers the drawn node, so the
draw's random word and arithmetic decide it as they do with the site off.
When the suffix's first action asks this
site, `InputPolicy::untried_site` names `fault.node`, so the job's untried
ordinal counts only the parent's jobs that started at `fault.node` and the
host takes it. Otherwise it names site 0, and the ordinal comes from the
parent's shared count and goes to the guest's first stamped action, as with
the site off. The stored action holds the answer, so replay asks nothing.
The offer depends only on the kind and the vocabulary. At resolve time the
host also has the parent's `alive` mask from the supervisor, so `Kill` and
`Pause` could offer running nodes and `Restart` stopped ones; the offer does
not use it. The faults workload resolves in its own `execute_job`, not
through `TargetExecution::resolve_action`, because the offer needs the run's
vocabulary.

A recorded input can pin answers: `{"actions": [...], "answers": [{"step": 3,
"choice": 12, "label": 1}]}` replaces the answer to guest choice 12 (counting
from 0 within the action) of step 3 (counting actions from 1, as replay steps
do), and an answer without `label` takes the default. Each answer is a
fault-policy `RecordedEnv` override, one `Answer` per `Moment`, where a guest
choice's moment is its ordinal within the action. The `Chooser` is still asked
at an answered choice, so the answers after it keep their stream. Answers are
part of the target identity, `branch` moves them onto its continuation,
`search --from` keeps those within its prefix, its `bug-N.json`, `hang-N.json`,
`failure-N.json` and `first-bug-input.json` carry the answers within their
actions, and `shrink` refuses a run that has them. Each replay step counts its guest
choices (`choices`, absent when zero), and `debug run` and `replay` write every
choice of the run, and `branch` every choice of its continuation, to
`choices.jsonl` beside `report.json`: the run, step,
who asked (`asker`), choice ordinal, decision moment, guest clock, site,
option count, default, what the `Chooser` took, the label applied, and
`answered` when an input answer replaced it. Host and guest choices share
this row. `asker` is `action` for a host site and `{"process": pid}` for a
guest draw, the pid being the upper half of the guest's request id. A host
row has no choice ordinal, lists its `offered` labels and counts them as its
options, has guest clock zero, and comes first among its step's rows; the
run writes one from the stored action for every applied action of a kind
with a `fault.node` offer, whether or not a `Chooser` picked the node. Event
kinds have no offer and no row. Pins address guest rows only, and a host answer changes when the
input's action names another node. A search writes no per-choice records
unless the environment variable `HARMONY_FAULTS_CHOICE_RECORD` names a file:
each freshly drawn node fault of a job with a plan, a `Pause` under
`resume_park` included, then appends its host row there, with the parent's
path length (`depth`) and, when the first ask took the untried node, the
untried ordinal, like the settle record below. With `untried_first_nodes`
off the row holds the drawn node, so both arms of a comparison write the same
rows. An action served from the snapshot cache has no guest
rows.

For the searcher's stall report, `Reporting::state_record` and
`Reporting::observation_record` give a snapshot's or an action's record
through `FaultKeys::record`, and `Reporting::choice_records` rebuilds the host
`fault.node` row the job wrote for an action, from the stored action. A job
writes that row only with a choice plan and only for an action that targets a
node and had no guest choices when drawn. The job stamps every such action
with `GuestChoices::from_plan` for its plan and index, so an action whose
guest seed matches that stamp is one the job asked; an action copied with its
earlier guest choices is not. Guest rows stay in the target: a guest site's index is the
order of its first ask inside one action, so it names no declared site.

Unrecorded standing-service questions stop at an explicit decision. The fault
client derives an answer from the action's configuration and resumes through a
recorded resolution before the guest sees it. Worker and UML sessions carry
the same resolution. Each branch also records a compact schedule of coverage
quantums from its action windows. The SDK selects that quantum directly, so a
small quantum does not create one answer-tape entry per callback.
When reconstructing an earlier prefix, the resolver uses that prefix's installed
service configuration for requests before its deadline. A request at or beyond
the deadline remains pending at the action snapshot. The next action's branch
installs its service configuration before resolving that request.

Before executing an uncached prefix, the client compiles its action timeline
in one checked cumulative pass. One immutable plan supplies the branch's exact
standing-service configuration, coverage schedule, local decision handler and
final deadline. Reconstruction compiles each prefix separately so earlier
branches retain their original configuration bytes. Compilation and full-prefix
history materialization remain linear in that prefix's depth; plans are not
retained across actions.

Each fault action becomes a standing-fault window on the shared
[`fault-policy`](../fault-policy) wire form. The package answers the platform supervisor's
standing poll with the windows whose half-open span contains the polling
moment, plus the windows that closed after the previous poll moment the question
carries. An input is fully described by its encoded window list and one
branch installs it. A hold still running when an event-park window closes
continues after the window, so the next actions can overlap the held thread.
The park counts as a pending fault until that hold ends.

## Execution

[`consonance`](src/consonance.rs) drives one session per evaluator thread. A
search started by the `harmony` CLI runs each session in a child process
(`WorkerSession`, started as the hidden `harmony session-worker` command), so
macOS runs one VM per worker process; library callers without a launcher run
`Session` in the thread. Each portable action prefix maps to a real whole-VM
snapshot: the session branches its parent under the prefix's window list, runs
to the action's horizon deadline, and snapshots the exact stopped endpoint. A terminal stop is recorded
with its original stop and has no successor. If a continuable endpoint cannot
be snapshotted, the session is abandoned and the control diagnostic is
reported. Each session keeps only its current chain of prefix snapshots, at
most 32 past the setup snapshot. The replay that confirms a finding and
writes its timeline runs on a linear target (`FaultTarget::linear`), which
keeps only the setup snapshot and the newest prefix. That replay never returns
to an earlier prefix, and each action branches from its parent, so the replay
is unchanged. It runs in the search's own process while the search's snapshot
cache is still resident, and its retained prefixes each hold the pages that
their action dirtied and a copy of the event log, so keeping the whole chain
could exhaust the search's memory. A session publishes sealed prefixes to the
search's shared snapshot cache (see [consonance-client](../../consonance/client/README.md)).
For its first 64 captures it publishes every prefix. After that it publishes
only prefixes whose depth is a multiple of a spacing `n`, a power of two up to
16. A larger `n` saves publish time, and each later import reruns about `n / 2`
more actions. The session doubles `n` while `(2n)^2` stays at most
`2 * publish * captures / (lookups * action)`. Here `publish` is the mean publish
time, `action` the mean host time per captured action, `captures` the captured
prefixes, and `lookups` the imports and misses plus one. Each published prefix
stores its pages as a delta from the nearest published ancestor in the chain.
Publishing an action prefix already cached with a different target state
quarantines its namespace before the fault controller seals a link. A shared
integrity check invalidates the campaign, including work already running from
local cache links; it cannot count the contradiction as an ordinary execution
failure or return a completed campaign report. Existing leases, subsequent
lookups and publications, and dependent descendants cannot reuse that domain.
The error path writes `campaign-invalidation.json` with an invalidated verdict
and `determinism-witnesses/` with both exact anchor-first extent chains, their
checksums, namespace, action-prefix bytes and commitments. It also retains the
kernel, initramfs, optional root and UML profile, running host executable, and
setup configuration. Extents remain sparse relative to setup: reconstruction
must rebuild setup from those artifacts and verify the setup identity recorded
in their sidecars. These files preserve disputed cache evidence; UML whole-state
identity remains unavailable. An incomplete evidence write keeps an invalidated
marker and reports the persistence failure while retaining the typed original
error and its witnesses.
A prefix that the chain does not hold is imported from the longest cached
prefix in the shared cache, and only the remaining actions run again. Chain
bookkeeping lives in [`chain`](src/chain.rs). Without the shared cache a
missing prefix is rebuilt from the chain's longest matching link.
One memory budget covers the shared cache and every worker's local snapshot
store. It is the free memory left after a 1 GiB reserve, the coordinator's 512 MiB
search state, one guest RAM for a worker's import (its decompressed pages and
the pages it adds to the store before the next report), and each worker's guest
RAM plus 256 MiB for the rest of the worker process. The worker count drops
until each worker also has room for a store as large as its guest RAM. With a
1 GiB guest and four workers on etcd in a 14 GiB cgroup, a store holds about
330 MiB once it has the setup snapshot, and a median of 0.85 to 1.2 GiB during a
search. On macOS a search runs at most four workers.
During the search the cache measures free memory again. The budget falls when
memory it does not count, such as the pages a snapshot import or export holds,
grows past this plan, and rises back toward the plan when that memory is free
(see the [client README](../../consonance/client/README.md)).
Before each execution and after each sealed prefix, a worker reports its
store's page, VM-state and index bytes to the cache. While any worker's store
is larger than the guest RAM, a report over budget evicts no shared entry, and
a reporting worker whose store is above the guest RAM drops its own prefix
snapshots instead. Once every store is at or below the guest RAM, the cache
evicts entries to keep the total in budget. When eviction
cannot, the worker with the largest store drops its
oldest prefix snapshots one at a time until the total fits, keeping setup and
its newest prefix, and then drops back to the setup snapshot. While a settle
phase runs, the chain holds the job's endpoint: it drops the prefixes after the
endpoint, oldest first, then the prefixes before it. After a push it keeps
setup, the endpoint and the newest prefix even when the store is still over
budget, so the store can exceed the budget by one prefix until the phase ends;
the restore at the end of the phase may also drop the newest prefix, but not
the endpoint. The same holds while the store shrinks toward its guest RAM: a
report only asks the worker to shrink, and the chain chooses which prefixes go.
When the chain has nothing left to drop, the worker reports again, and the
cache then counts its store as at the guest RAM, so shared entries go once no
other store is above it. The hold keeps at most one link beyond what the chain
keeps without it, and releasing it frees nothing until the next fit. While the total
is over budget, every other worker also drops prefix snapshots until its store
is no larger than at its last report that fit the budget, so the stores stop
growing while the largest store's worker is busy. The run prints the worker count and budget, and
`campaign-summary.json` records the cache's counters, including `store_bytes`
and `shrinks`, under `snapshot_cache`. It samples the shared index once under
`snapshot_cache_locks` for per-operation acquisition attempts, acquired/completed
counts and wait/hold nanoseconds; worker/session totals never sum this shared
owner. `stats` reads have their own site. These host-time diagnostics do not affect
budget policy or certificates.
A fault search keeps one reservation per worker in its admission window, the
same window as every other workload. Each selection sees the archive as of the
last admission, so a wider window makes each new state wait more admissions
before a job can start from it, and the deepest chain grows that much slower.
The search stops at the `--for` limit and on the first SIGINT or SIGTERM. A
search that does not resume a checkpoint also stops at its first admitted
finding. It drops the jobs in flight and writes its report.
While a job's session waits for a worker reply, it checks the job's cancel flag
every 20 ms. On a stop it retires that session worker, so the search exits
within a few seconds of the stop. Each finding's confirmation replay still runs
before `report.json` is written. `report.json` records `jobs_cancelled` and
`interrupted`. A second SIGINT or SIGTERM exits at once with status 128 plus
the signal number and writes no report. Session workers ignore SIGINT and
SIGTERM, so only the search process decides the stop.
Each worker reports its time through the campaign `telemetry`: boot, new
action runs, prefix rebuilds, and replayed actions, each in host and virtual
time. It also reports session restore, branch, seal, observation, and drop
time; the chain's exact hits, ancestor hits, misses and evictions; shared cache
imports and publications with their time, and refused publications; and the VMM's exit, guest-run, and snapshot counters under
`vmm.`. `campaign-summary.json` includes the work telemetry record. The campaign uses
`thread_work_telemetry`, which collects cumulative work without querying snapshot
ownership or store size. `thread_residency_diagnostics` explicitly samples the
live chain's entries, page bytes and observation-checkpoint charge. These typed
gauges are absent after the live session retires and are never accumulated into
work counters; unavailable ownership or arithmetic overflow makes page and total
charge unavailable. Each live point is queried once, so worker mode performs
real guarded ownership RPCs. Collect these diagnostics separately from phase
counter snapshots. Actual store-budget enforcement continues to query store
bytes and report them to the shared index independently of either telemetry API.

Work-counter reads in worker mode still perform one guarded TELEMETRY RPC. Its
framing and guard work remain observable, and parent/child snapshots occur at
different points around response publication. These reads are not atomic or free.
Counter getters do not estimate or subtract their own cost. Host timing, gauges
and physical request counts do not affect logical selection or certificates.

Physical worker assignment uses opaque captured-history root keys. A matching
live session advertises its retained chain endpoints, with a better rank when
the corresponding observation decoder checkpoint has been verified. Missing,
foreign or abandoned sessions advertise nothing. Extracting these hints does no
session RPC, hashing, import or restore, and retains no extra snapshot lease.
Actual restoration still validates the captured receipt, machine state and
decoder binding. A stale hint can affect host work, never certify an endpoint.

The campaign summary records the requested processor assignment, detected
topology and inherited host memory policy as diagnostics.
These fields do not enter the deterministic stream or campaign checkpoints.

Optional job locality observations are fixed-size workload-owned values, separate
from logical fault observations and endpoint certificates. The origin restore
records its actual selected source: local exact/ancestor or shared import
exact/ancestor, selected/requested depths, session generation and final outcome.
A link imported during a previous job is local when subsequently reused.
Reconstruction is orthogonal to starting source; attempted/completed rebuilt
steps and archived parent-tail replay are separate counts. Lookup/import counters
separate origin, action-parent and cached-child work. Source selection preserves
the existing depth-first search: a shared exact match can precede a local ancestor.

An origin restore succeeds only after the existing backend-specific endpoint,
event-history and observation checks complete. UML retains its explicit
whole-state-unavailable contract. Failed/cutoff restores retain their failure stage
and attempted source when known, without treating a watchdog-converted Ok as
success. Session generations are checked, per-thread runtime identities paired
with the actual worker; they are unavailable after exhaustion and are not
persistent cache identities. Bookkeeping survives Live retirement in a job-local
TLS value and drains before panic recovery. It retains no snapshot or lease,
issues no telemetry or ownership RPC, and supplies no restore authority. These
measurements do not describe host NUMA page placement or change dispatch policy.

`origin.generation` identifies the selected starting session. `end_generation`
comes from an in-memory TLS-presence census at drain, so retirement through
current-state or event-history accessors is visible even when it bypasses the
normal Live-operation wrapper. A retired session does not erase its attempted
source generation. Generation values are observational, paired with the actual
worker, and grant no handle or restore authority.

Candidate-path lookup counts exclude the certified-local fast path. Parent and
cached-child source counts describe validated selections independently of the
origin summary; cached-child searches are exact. Parent sources count
action-parent selections for newly executed prefixes, not reserved archive
parents or parent-tail replay
actions. They are selection diagnostics, not separate whole-state certificates.
Import completion means the existing import/binding/install path succeeded; a later restore failure is still
reported separately. `NotAttempted` on a failed job means execution rejected the
job before origin restore; it is not a zero-cost restore measurement. The generic
256-byte bound is checked on campaign entry. Consumers must retain unavailable/
overflow and sink-discard coverage when aggregating results.

Consumers namespace worker/generation pairs by the observed invocation; thread
pools, resumed invocations and processes can restart those values. The sink's
caller-owned run identity supplies that namespace, outside logical campaign data.

The shared session watchdog follows deterministic virtual-time progress, so a
slowly advancing instrumented guest can finish a long action while one stuck
at a virtual moment still times out. Replay can apply explicit, bounded Wait
actions to obtain current quiescent check evidence; these actions and ticks
are reported separately from the recorded input actions.

A campaign never encodes the virtual-time trace, so the session is configured
to defer sparse checkpoint hashing. Each due checkpoint would otherwise hash
all of a gigabyte-class guest's RAM inside the run that reached it, starting
with the boot that reaches setup.

[`campaign`](src/campaign.rs) implements the game-neutral campaign interface
over that target, and [`archive`](src/archive.rs) supplies the endpoint key,
which captures assertion, liveness, in-flight work, event-firing state, faults
still in effect, and bucketed edge coverage. The edge-digest register sums a hash of every
(edge, hit-count bucket) pair that instrumented nodes have entered. It is
part of the holder identity, so an execution that drives any edge into a new
bucket opens a new slot inside its lifecycle place.
Workloads without the C runtime report a digest of zero. The
raw instrumented site reported by an event kill remains diagnostic evidence; it
is not archive novelty because a large instrumented binary can report a
distinct address at nearly every endpoint.

Assertions arrive as Antithesis SDK JSON records written to `/dev/harmony`.
The host keys each assertion by its `id` (the message when the id is empty)
and keeps its kind, message, and source location. An Always or
AlwaysOrUnreachable assertion evaluated false, or an Unreachable assertion that
was reached, is a violation and marks the execution as a bug. The host keeps
the `details` of the record that first violates an assertion as that
assertion's `failure_details`, so a finding names the values the guest saw;
passing records keep nothing, and a first violation with null details keeps
nothing even when a later violation has some. The details are stored as their
compact JSON text, a string in every format, so a float or a deeply nested
value reads back byte for byte and adds no nesting to `report.json`. Details
that the JSON reader cannot parse (nesting past its depth limit) become
`{"unreadable": "<reader error>"}` and the violation is still recorded.
Details whose text is longer than 4096 bytes (`FAILURE_DETAILS_LIMIT_BYTES`)
become `{"truncated_bytes": N, "prefix": "..."}` holding the first 1024 bytes
(`FAILURE_DETAILS_PREFIX_BYTES`). The set of
Sometimes and Reachable assertions that passed enters the archive key as a
count plus a digest of the sorted ids, so the key has no limit on how many
distinct assertions a workload declares. Every process's records feed the key.
`campaign-summary.json` lists every assertion the campaign saw under
`assertions`. A Sometimes or Reachable assertion that was declared but never
passed is a campaign failure: it appears under `never_satisfied` in both
`campaign-summary.json` and `report.json`, and the search prints one
`FAIL: assertion never satisfied` line for each.
The binary hypercall SDK (`consonance/harmony-linux/sdk`) declares its points
once in an `SDKC` catalog event and then emits an assertion event only when a
point is hit or violated. The host registers each catalog Always, Sometimes,
Reachable, and Unreachable point as declared and not yet passed, keyed by its
numeric local id with the catalog name as its message, and decodes later hits
and violations with the catalog kind. A Sometimes or Reachable point that never
fires is therefore reported under `never_satisfied`. Buggify and state points
are not assertions and are not registered. An assertion event for a point the
catalog did not declare is recorded as Sometimes when hit and Always when
violated. A catalog with a bad version, an unknown point kind, or a truncated
or overlong body fails the decode. The archive key uses the same numeric ids as
before, so recorded streams still replay.
A harness check that stops checking some keys at its step budget writes
`{"harmony_check":{"undecided_keys":N}}`. The host keeps the count from the
process that ran the check, and the completed check's evidence carries it as
`undecided_keys`, so a replay's `check` object shows it. `undecided_checks` in
`campaign-summary.json` and `report.json` has `executions`, the number of
executions whose latest completed check left at least one key undecided, and
`keys`, the sum of those checks' counts. An execution that starts from an
archived state and completes no check of its own counts that state's latest
check. `harmony show` prints `undecided keys: K in E executions` when `keys`
is above zero.
`park_sites` in `campaign-summary.json` counts event-park landings by site.
`park_reads` counts, by landing site, the holds after which the held thread
read shared memory that another process changed during the hold.
The campaign turns these counts into draw feedback: each site with a read
weighs `1024 * (reads + 10 * r) / (landings + 10)`, at least 1, where `r` is
the campaign's reads per landing across all sites. A site with few landings
weighs close to the campaign rate, so one read in one landing counts for less
than several reads in a few dozen landings.
The runtime also lists the sites each node process crosses for the first time
(see the runtime README). The host keeps at most 4,096 of them per action: when
an action lists more, it keeps the 4,096 whose hash of site and report moment
is smallest, so every listed site has the same chance to be kept and a site
listed in many actions is kept in about that share of them. Each action the
guest ran adds one to `site_actions`, and each site any of its observations
kept adds one to that site's `site_incidence`; both are in
`campaign-summary.json`. `site_incidence` holds at most 262,144 sites: once it
is full, sites already in it keep counting and a site not yet in it is not
added and gets no incidence weight, so the map stays within a few megabytes in
memory, in checkpoints and in the summary. Each site with an incidence weighs
`1024 * (A + 1) / (incidence + 1) * 10 / (landings + 10)`, at least 1, where `A`
is `site_actions` and `landings` is the site's count in `park_sites` (ours).
This is the rarity weighting that libFuzzer's default Entropic schedule and
AFLFast's FAST schedule use to pick inputs, applied here to pick a park site: a
site crossed in one action out of a thousand weighs about 500 times a site
every action crosses. The second factor halves a site's weight after ten
landings, so a site that holds parks without changing what the search finds
gives way to others, as AFLFast lowers an input's energy the more often it is
chosen. A site that adds both weights gets their sum. Sites that never enter
their first hit-count bucket in any process, because they share a counter slot
with a hotter site, are never listed and get no weight. The weights change at
each draw-table update. Once any site has a weight, half of the drawn
parks aim at a site picked by weight. An aimed park has `edges` 1 and a target
range that is the site alone or the site plus or minus `2^k` bytes for `k` from
6 through 12, each of the eight widths equally likely.
`park_aims` in `campaign-summary.json` and `report.json` covers only the
`EventPark` actions drawn with a target: `armed` counts those the guest ran,
`landings` their landings, and `sites` those landings by site. An unaimed park
that draws `edges` 1 is not counted there. Each job record's
`draw_checkpoint_before.weighted_sites` is the number of sites with a weight
when the job was drawn, so the stream shows the record from which fresh draws
could aim. A job or skip that reuses a recorded tail, a continuation or a
splice, keeps that tail's targets whatever the count, and replay does not
check its count.
`park_thresholds` counts, for each `floor(log2(edges))`, the `EventPark`
actions the guest ran at that threshold and their landings, so the landings
per action at each threshold show which part of the drawn range a workload's
executions reach. A park re-arms after each hold, so one action can land more
than once. On
the SQLite WAL reset and etcd cases no park with a threshold of `1 << 14` or
more fired, which sets the top of the drawn range.

The generic `execution_work` counter and `report.json`
`execution_ticks` count the guest ticks requested by successfully applied actions
after setup. This logical counter is monotonic across target reset and snapshot
restore; longer waits cost more even when an endpoint is cached. The separate
`guest_horizons` diagnostic counts action evaluations that enter the guest;
cache reuse can change the count and reset clears it. Setup, prefix
reconstruction, and failed actions are outside the logical counter. Replay
continues an enabled continuous checker with waits of 10 ms through 40.96 s,
doubling only while its completed generation is stale or faults remain pending.
`actions_applied` remains the recorded input prefix, while `settle_actions` and
`settle_ticks` account for that deterministic validation tail.

A campaign run with `HARMONY_FAULTS_SETTLE_RECORD=PATH` ends every job with a
settle phase that observes the endpoint after faults end. After the suffix, the
target snapshots its endpoint and applies, outside the recorded input, a 16-tick
`EndFaults`, a 16-tick `Restart` for each node whose alive bit is clear, the
heal hook named by `HARMONY_FAULTS_SETTLE_HEAL` for 16 ticks when that is set,
and then 50-tick `Wait` actions. An `EventKill` window stays in the supervisor's
active set for the rest of the execution, whether or not its kill fires, and the
supervisor reports a fault in force while any window is active, so without
`EndFaults` no check after the first `EventKill` would run the replica
comparison. The faults-in-force, recovery-pending and fault-free-check registers
of the [supervisor](../../consonance/harmony-linux/supervisor/README.md) end it:
the workload is ready once neither faults nor recovery are pending, and the
outcome is `compared` when a check that started after that has exited
successfully. The phase also ends 6,000 ticks after the restarts and heal end,
with outcome `not ready within the settle limit` when the workload never became
ready and `no check without a fault in force finished` when no such check
passed, or at a new violation, a terminal stop, an endpoint the strategy rejects
(ours; outcome `rejected`), a hang or a cancellation. A workload that stays not
ready can violate either of the supervisor's two recovery properties,
`workload becomes ready within 30 seconds after faults end` and its per-node
form (ours),
`every workload node answers a pinned check read within 30 seconds after faults end or is still applying entries`.
Both count from the end of the last fault in force, which for a cut is the
heal hook's successful exit rather than the end of the heal action; a heal
that fails leaves the cut in force, and then neither property can fail. The
ready property fails 30 seconds after the later of that end and the moment the
workload was first seen not ready. The per-node property fails only when a
check without a fault in force exits successfully after 30 quiet seconds, with
no disturbance since it started, and names a node. The harness kit's check
retries each pinned read, from the moment that read begins, for the longer of
its timeout and the part of the 30 seconds still to run when the check
started, so a read can retry past the 30-second mark. It then takes a progress
fence and reads each unanswered node's applied index, and names a node whose
index is below the fence and has not advanced when read again 10 seconds
later; when the fence fails it names every unanswered node without those
reads. The limit must therefore outlast 30 seconds by the heal's run time plus
one check. In replays of nine rqlite settles that stayed not ready, the ready
property failed 3,050 to 3,300 ticks after the heal action ended and the
per-node property 3,150 to 4,000. The 6,000-tick limit leaves 20 seconds past
the latest of these. A settle whose heal runs longer than that margin, or
whose workload loses readiness late in the settle, can still reach the limit
before either property reports. It then ends `not ready within the settle
limit` if the workload never became ready, and `no check without a fault in
force finished` if it was ready once. The target then restores the
snapshot, which the chain held through the phase, so the restore runs no action again unless the session was
replaced or the phase restored a prefix older than the endpoint. A restore that
finds no held link rebuilds the endpoint as any other restore does, so the hold
saves work and never decides a result. The target also restores the job's
tick, horizon and last-action observation counters, so unless the phase found a
violation, the job's result, its `execution_work` and every search decision are
those of the run without the phase. The snapshots taken during the phase stay in
the snapshot cache and can evict others, so cache statistics, worker placement
and wall time can differ from that run. A new violation that the run's objective
counts ends the phase with outcome `bug`. The target then applies the phase's
actions again from the restored snapshot as actions of the job, which reaches
its objective at the action where the violation appeared: the search reports
and confirms that violation as it does any finding, whose input ends with the
phase's actions, the job's `execution_work` includes the phase's ticks, and no
phase action becomes an archive entry. A violation that does not recur when the actions are applied
again ends the run with an error. Each job appends one JSON line to `PATH` with
the path and suffix lengths, the job's execution work, the event-kill armed bits
at the suffix's end, the nodes restarted, the ticks to ready and to the
comparison, the phase's actions and ticks, the fault-free check count at the
job's start, after the suffix and after the phase, and the outcome. Every line
whose outcome is not `compared` also carries the full input as `actions`, so the
line alone, saved as a file, is an input for `harmony debug`: the path with the
phase's actions, the path alone for a skipped job, and in either case through
the action that hung when one did. A job
that hung, ended terminal, was cancelled or reached its objective gets a line
with a `skipped:` outcome and no phase. A resumed run replays its recorded jobs
and appends their lines again.

## macOS

Hypervisor.framework allows one virtual machine per process, so a macOS run
takes one worker per process. A worker releases its target when it finishes so
the next one can boot a virtual machine in that process.

## User-mode Linux

`--backend uml` with `runner.options.uml_profile = "PROFILE"` in the TOML
recipe runs the same search on a
User-mode Linux guest (`UmlSession` in
[consonance-client](../../consonance/client/README.md)) as an ordinary user on
any Linux host, with no KVM. The profile is verified at start, its kernel
replaces `runner.options.kernel`, and each session runs in the search thread. The profile
identity, the loaded Harmony executable identity, and a digest of the complete
host identity, including the physical processor and CPU-info lines UML imports
at boot, join the
execution identity and the workload identity, and the campaign stream and
snapshot checkpoint get UML formats of their own, so a UML run never reuses
VM state. A profile changed after workload admission is rejected before guest
boot. Saved searches and command runs also bind the profile loaded for guest
execution to the recording's verified profile identity and require its
executable digest to match the saved kernel. The staged initramfs is checked
against the requested image, then the sealed bytes are checked
again before boot, so the identity names the image the session actually runs.
The UML identity hashes kernel arguments with their
lengths, preserving the boundaries used at boot when a live session or cache
entry is reused. Search admits one processor from the selected coordinator
placement for the whole campaign.
Host worker threads still run on their assigned cores, while every UML guest
child is pinned to the admitted processor; this prevents guest-visible CPUID
or MIDR differences from entering snapshots passed between workers, at the
cost of contention among concurrent guests. Standalone debug and replay use
the same coordinator selection, so they do not switch guest processors when
they leave the search worker pool. Each snapshot
is a full guest image of about the guest's RAM, so a
worker's store fills its budget faster than under KVM.

## Running it

Put explicit guest artifacts in the runner table of `harmony.toml`:

```toml
[runner]
kind = "consonance"
backend = "kvm"
[runner.options]
kernel = "vmlinux"
base_initramfs = "initramfs.cpio.gz"
ram_mib = 1024
```

```sh
harmony search IMAGE.oci --config harmony.toml --seed 1 --executions 100000 --out run/
harmony debug replay run/ --finding 1 --repeat 10 --out confirm/
```

For UML, select `runner.backend = "uml"` and set `runner.options.uml_profile`
instead of `kernel`. Workload knobs, supervision and interventions belong in
`workload.options`; see the [CLI recipe](../../cli/README.md).

Both modes write `report.json` ([`package`](src/package.rs)) with the pinned
image and kernel hashes, the execution identity, the run bounds, and
either the bugs found or the replay outcomes. Its `format` is
`faults-report-v4`. Decoding a `Report` refuses another format, or a missing
one: exact replay and the timeline refuse an old report, and `search --resume`
discards it and replays its bugs again. The run manifest names `report.json`
by its SHA-256 instead of holding a copy, so the report is stored once.
Each bug's `violation_details` maps a violated assertion's id to its
`failure_details` text, and is omitted when no violated assertion has details;
`bug-N.json` carries the same field on each assertion of its observations.
`harmony show --finding N` prints the details on the line after each violation,
and lists a violation the search recorded but the replay did not reproduce,
with its details, after the replay's own violations. The search archive's
format is `faults-archive-v3`, because its per-bug observations carry the
details too.

A search report keeps each bug's replay outcome but not its per-step timeline
(the console and observations at every step), which is most of a bug's
bytes. Search writes the timeline of bug N to `bug-N-timeline.json` beside
`bug-N.json`, and the bug's `timeline` field names that file and its SHA-256.
`BugSummary::witness` is the one reader: it refuses a name that is not a plain
file name in the run directory, and refuses with the file's path when the file
is missing or its digest differs. The CLI reads every finding through it
whenever it reads a search report, so `show`, the timeline, logs, branch,
shrink, explain, `diff` and exact replay all refuse a run with a missing or
changed timeline file.

`search --resume` carries a bug unchanged from the earlier chunk when its
execution, actions and evidence match. It then hard-links the earlier chunk's
timeline file into the new chunk, checks its digest, and links `bug-N.json`
when its bytes are unchanged, copying either file when the link fails (for
example across file systems). Every chunk is still self-contained: deleting an
earlier chunk leaves the linked files readable. A carried bug whose earlier
timeline is missing or changed is replayed again instead.

New replay outcomes encode the engine's 32-byte state digest directly as
lowercase hex and mark it with `state_hash_encoding: "engine_page_digest"`.
That digest is the page-digest state hash that `vmm-core` describes: BLAKE3
over per-page BLAKE3 digests of guest RAM, then the VM-state suffix.
`"engine_digest"` marks reports from earlier versions, whose engine digest was
SHA-256 over all of guest RAM and the same suffix. The two encodings never
compare equal, so replay checks refuse to match a report against one written
with the other encoding.
UML outcomes instead have an empty `state_hash` and
`state_hash_encoding: "unavailable"`: their guest memory has no canonical
cross-process state digest. Search still confirms a bug when the fresh replay
reproduces its recorded assertion or stop evidence. `harmony debug replay`
records the new observations but refuses to call two UML outcomes an exact
reproduction without a full-state identity.
Reports written by earlier versions omit that marker and contain SHA-256 of
the digest; readers default a missing marker to the legacy interpretation.
Earlier UML `engine_digest` values covered only bridge state and cannot prove
full-state replay equivalence.

The session records `watchdog_cutoffs` when a guest action or parent
reconstruction exceeds its host progress bound, including fresh and in-place
UML restores. Typed session infrastructure errors and abandoned sessions carry
an independent `infrastructure_failure` flag; they do not increment the timeout
counter. Neither cause is guest bug evidence.

A watchdog cutoff inside a campaign job makes that job a hung job. The job
result carries the actions it attempted, up to and including the action that
hung, and no admitted actions. A cutoff during the job's parent reconstruction
or parent replay records no attempted suffix actions. The searcher counts the
hung job, writes its stream record with no decisions, and keeps running for the
rest of the budget. The hung job's end state never enters the archive, and it
adds no selection, mixture or duration feedback. Result certification accepts a
hung result only when the target recorded the cutoff, and replay reruns the job
and must hang again at the same place.

Each hung job's full input, including the branch root's actions, becomes a
`hang-N.json` file in the run directory (at most 64 per campaign). A hang is a
separate record rather than a finding: the cutoff is a host-time bound, so one
cutoff cannot show whether the guest hung or the host was slow, and a confirmed
finding ends the campaign while a hang should not. The reports'
`watchdog_cutoffs` field and the summary's `hung_jobs` field count hung jobs.

An action that fails in the guest ends its execution, for example when the
fault agent reports an infrastructure failure. The reports'
`execution_failures` field counts these executions. Each one's full input, up
to and including the failing action and with the branch root's actions,
becomes a `failure-N.json` file in the run directory (at most 64 per campaign),
and the search prints the file name. `harmony debug run --actions
failure-N.json` replays the input, and its console shows the guest's error.
The search's own message does not carry that error, and a guest
out-of-memory kill in a deep lineage is recorded as an ordinary failed
execution; both are tracked in
[#992](https://github.com/pH14/harmony/issues/992).

The searcher stops the campaign when the current run of consecutive hung jobs
is too long for the hang rate seen before it. With `h` hangs in the `n` jobs
before the run, the estimated rate is `(h + 1) / (n + 2)`. The campaign stops
when a run of `k` hangs has a chance below 1 in 1000 under that rate, that is
when `rate^(k - 1) < 1/1000`. With no earlier jobs this stops after 11 hangs in
a row. The jobs in flight finish, and the run directory gets `report.json`,
`campaign-summary.json` and the `hang-N.json` files as for any other run. Both
reports carry `hang_stop`, a message that names both counts, and the search
prints it and exits `2`.

Infrastructure failures remain incomplete: the affected reservation returns an
error before admission, result certification, selection accounting, mixture
updates or duration feedback. Cutoffs during bootstrap, branch-prefix replay and
shared-cache imports are incomplete in the same way, since the campaign has no
job to record them against.
The campaign then stops with an explicit incomplete error instead of writing a
completed report. Previously admitted stream records are flushed and existing
search checkpoints remain available; the incomplete stream has no successful
completion record. A later search can resume an earlier complete checkpoint.
The recorded `host_failure_policy = host_incomplete_v3` binds this behavior;
streams and checkpoints with an absent or different policy are refused, including
earlier policies. The execution and endpoint-certificate identities
also advance because observations now encode the distinct infrastructure flag. The
combined replay endpoint uses certificate domain v4, binding ordinary assurance,
ordered event history and infrastructure-marked observations under one schema. Reading ordered history
preserves typed host failures and rejects a known abandoned matching session as
incomplete; missing or mismatched session identities remain ordinary errors. The
shared hash helper retains the actual error even when that call retires the
session: a timeout stays a timeout and a malformed reply stays an ordinary error.
A successful hash from a session retired during the call cannot certify a
continuation. These checks also cover receipt reconstruction and imported roots.
Execution identity v11 binds this combined contract and its assurance identity;
earlier identities that omitted either cannot be reused. Ordinary
guest terminal outcomes retain their existing meaning. An outer CLI timeout is
also an infrastructure failure. A supervisor
runtime error has a separate SDK status and cannot turn a PID 1 exit into bug
evidence.

The searcher learns the duration of waits and instrumented event holds from
admitted campaign outcomes and logical execution cost. It continues sampling
short and long logarithmic durations while favoring durations whose own action
recently produced useful work. The adapter groups this feedback by node liveness
and whether hooks, workload, checks, or event faults have progressed. Site
identities and workload-specific concepts do not enter the duration policy.
Choices and feedback state are recorded in the campaign stream; input replay
executes the recorded durations directly.
The archive key's 128-bit assertion digest is a decimal string in JSON
campaign streams, archive reports, and progress records. This lets the tagged
job and skip records decode. The binary campaign result hash and checkpoints
retain the native 128-bit encoding.

Each campaign job's recorded result digest binds its logical result to a tagged
final endpoint certificate. The v2 result domain also binds versioned ordered-history
records (v2 includes completed semantic service exchanges) whenever a KVM whole-state endpoint is available, including terminal and
zero-action jobs. Failed or unavailable endpoints explicitly carry no history;
that absence cannot stand in for a whole-state endpoint.

Prepared execution can be admitted independently of image staging through
`prepare::admit_prepared`. It verifies the actual packed rootfs, the mode's image
admission and the selected supervisor bundle, then derives the instrumented-event
vocabulary from that admitted archive. A caller-supplied vocabulary or frontend
readiness result cannot replace those checks. Generic prepared execution records
and canonical OCI control validation remain owned by `oci-support`.

Every runnable snapshot carries a persistent, shared action history. Each node
binds its parent history, action, actual endpoint evidence, action-local
observation commitment, and observation boundary moments. KVM endpoint evidence
includes the whole-state certificate and cumulative ordered normalized-event
count and commitment; UML explicitly records that whole-state
evidence is unavailable while still committing observations. Nodes are captured
from stopped execution before their snapshots are published. Candidate clones
share nodes, and snapshot/result identities hash their history roots rather than
serializing every ancestor.

A retained endpoint restores directly after comparing its independently captured
history receipt with the requested history. This requires one endpoint replay and
whole-state hash on KVM, regardless of action depth. An imported endpoint receives
its receipt from the integrity-checked cache object, never from the caller's
expected certificates. Missing suffixes execute and check each actual prefix
before the next action or publication, so an intermediate divergence cannot be
hidden by a converged final endpoint. Snapshot standalone capture still reads
fresh live evidence; no whole-state digest is cached across operations.
Parent replay compares every archived prefix certificate with the actual receipt
captured by that same replayed action, without hashing the endpoint a second time.
The campaign still checks integrity and the typed watchdog-cutoff check before
accepting the comparison.
Cached-child hits do not materialize the action prefix. New execution still
materializes the actions required by branch configuration and window scheduling;
that work scales with prefix length. Missing-prefix checks traverse the expected
suffix once and compare borrowed receipts rather than repeatedly walking ancestors.

SDK observation decoding keeps an actual-session cursor separate from the normalized
ordered-event commitment: its offset counts every transported SDK tuple, including
ignored events. Successful suffix batches update cumulative registers, assertion
generations and hidden check passes atomically. Each action retains only parks
and reads strictly after its actual parent's observation moment. Empty suffixes
preserve the last tuple moment and repeated observations preserve action evidence.
Branch and replay install a checkpoint from the selected actual captured Link;
rollback, new sessions and failures cannot reuse another lineage's cursor.

Captured observation attachments use `fault-observations-v3`, included in the exact
cache namespace. Imported decoder checkpoints remain untrusted. When a pending
checkpoint is retained, the normal required machine replay or branch reconstructs
the full actual SDK history and compares all decoder state, including hidden maps
and offset. If pressure has discarded the pending checkpoint, restoration instead
reconstructs actual decoder state and validates the captured receipt without using
the imported decoder. Missing local checkpoints use that same reconstruction path.
First import and eviction fallback add full-history fetch and decode work, but no
additional machine restore.
Expected campaign snapshots and root payloads provide no decoder authority; fresh
root imports start from offset zero. Whole-state and ordered-event certificates
retain their existing formats and fresh integrity checks.

Action execution uses Consonance's generic owned endpoint transaction. A retained
validated parent decoder checkpoint supplies the exact SDK cursor, allowing one
Execute exchange to branch, run, capture and return the first bounded suffix
page. The request preserves the legacy run's assertion stop mask. Missing or untrusted checkpoints keep the existing selected-parent branch
and full-history verification before the guest runs. Every returned page retains
its request/stop/cursor binding; the workload decodes all pages and checks SDK
infrastructure status before Commit transfers the provisional snapshot and its
separate whole-state and ordered-history facts. A decode failure abandons the
owned session. A capture or proof failure releases all session-owned handles,
including the selected parent, without publishing an uncertified child.

Service decisions remain workload decisions. Before the deadline the adapter
answers with its current action handler and explicitly resumes. At or after the
deadline it finalizes the returned stopped question without answering or running
again. The Consonance receipt keeps the actual Decision; Faults alone projects
that observation as Deadline and leaves the pending question in the snapshot for
the next action's service windows. UML explicitly omits whole-state/history
proofs, and uncaptured terminal cuts carry unavailable evidence. The common
worker path takes Execute plus Commit, with extra bounded Page exchanges only
when needed and a FinalizeDecision exchange at that rare boundary. Inner control
and SDK guards remain active. Run host time now includes the whole endpoint
exchange and decoding; combined branch/seal counts remain operation counts,
while their separate host-time buckets cover only unbatched operations.

Optional Link checkpoints have an internal 8 MiB retention cap, including for
sessions without a shared index. This is a performance policy, not a logical
retention setting. Under shared-cache pressure they are discarded before certified
VM links. Restoring an evicted checkpoint enforces the cap and reports its charge
without moving the selected VM Link. `report_store` includes session storage plus
unique retained decoder allocations and checkpoint metadata. If a session does not
expose storage bytes, the report still includes the optional checkpoint charge.
Shared Arc state is charged once by physical identity; map storage uses a
conservative per-entry bound,
with owned string/vector capacities and Arc headers counted. Cached charges are
computed at installation, not by rescanning maps on budget queries. Current working
decoder and temporary decoding/accounting scratch remain target-private, as the
previous full-history decoder scratch did; retained checkpoints sharing that state
are still charged. Dropping optional checkpoints never changes serialized attachment
bytes, actual receipts or leases.

`observe.events` and `observe.payload_bytes` count successful suffix fetches;
`observe.decoded_events` counts successful append transitions, including ignored
records. Payload bytes exclude transport framing. `session.observe_decode` and
`session.observe_projection` separate successful decode/infrastructure checks from
capture projection. Full actual reconstruction at restore is reported separately
as `session.observe_checkpoint_rebuild` and `observe.checkpoint_rebuild_*`, for both
imports and local eviction fallback. Fetch counts can include batches subsequently
rejected; failed decode/infrastructure attempts do not contribute completed timing
samples. Checkpoint rebuild timing counts completed fetch/decode/infrastructure
checks even when a subsequent decoder comparison or receipt-binding check rejects
the reconstructed checkpoint. `observe.checkpoint_hits`,
`observe.checkpoint_evictions` and `observe.full_rebuilds` distinguish reuse and
fallback. Explicit residency diagnostics retain the VM-page meaning of
`resident_bytes`; `observation_checkpoint_bytes` is the optional pool charge and
`charged_bytes` combines both live gauges.

Fault caches use exact fixed-width action-path roots. The cache object commitment
binds both the admitted machine target and opaque captured-history attachment;
different histories for one action path quarantine the namespace even if final
machine states agree. Page, inherited-page, omitted-page, host-image and lease
checks remain enabled. This removes repeated historical VM work, not the data
integrity scans required by those checks.
Child publication selects the lease of the actual restored parent point, including
when another sibling remains resident. Runnable, terminal and interrupted-rebuild
observations all exclude park and read evidence at or before the previous endpoint.

Standalone and whole-search checkpoint histories use a parent-indexed DAG with one receipt per
semantic history root. Encoding follows missing tails iteratively and deduplicates
independently allocated equivalent prefixes. Decode validates receipt roots, paths,
depth, domain, moments and backend continuity before constructing shared Arc nodes;
endpoint records reference those nodes. The generic searcher frames opaque records
and never interprets fault history. Old flat snapshot envelopes are refused.
Asynchronous checkpoint capture inventories the complete cloned snapshot graph:
inline observations, endpoint/event-history metadata and fixed-size park rows;
the capacities of both park vectors; the optional check's `Vec<String>` slots and
every point string; and assertion-map nodes plus each key, message and location
string capacity. History receipts and actions contain only inline fields. Each
shared history node includes its Arc control block and is charged once by physical
allocation identity, with conservative B-tree overhead for the private ledger.
Independent equivalent histories retain separate charges; owned observation
containers and strings are charged again for every cloned endpoint. The image
budget covers these capacities and container envelopes, not allocator fragmentation
or a process RSS ceiling. A failed charge uses the same synchronous codec before
cloning; it does not bypass persistence. Frozen snapshots own those
nodes independently of archive outer-Arc pins; capture neither certifies a
receipt nor exempts a later execution from actual-history or fresh VM checks.
History destruction remains iterative. Restore canonicalizes validated expected histories into
actual shared node allocations before rebuilding the memory ledger. Bootstrap
imports use the admitted live genesis allocation. Before archive installation, a
typed retention index normalizes a candidate's new tail against currently
budget-owned nodes. Independent workers therefore
share actual prefix allocations in both live execution and checkpoint resume.
Index registration and pruning follow the ledger's first retain and final
release, and index metadata is charged. Runtime accounting acquires only newly
unique history nodes and releases them at physical snapshot removal,
including inactive snapshots still needed by work in flight. Expected-history
canonicalization does not grant captured execution authority. Branch roots use
`HMROOT03` to preserve their independently pinned semantic object commitments.
Old history, snapshot, stream, cache and root formats are refused. Missing,
malformed, or conflicting certificates stop the campaign.

The replay certificate binds the v3 ordered-history format independently of
logical endpoint state. An intermediate event change alters the certificate even
when endpoint, observations, event count and final virtual time agree. Fresh setup
preserves the boot event prefix. Replay, branch, portable import and cache import
restore the captured prefix; snapshot capture and diagnostic segmentation add no
semantic events. Snapshot and schedule IDs never enter that commitment. Missing
KVM history cannot be replaced by an empty commitment. UML carries no ordered
normalized-event commitment and remains uncertified for whole state.

The VM stream and snapshot formats are v15 and v11, and the UML formats v12 and v7. The action policy
is `fault-action-duration-coverage-v7-parked-pause`; older contracts are refused.

Restoring a source snapshot compares its independently captured receipt and
reconstructs the SDK observation before using that saved observation, including
the current action's park and read evidence. Tree import still rebuilds each
imported action and checks source-bound checkpoint identities. A source entry
digest commits its history root and ordered endpoint history. Reconstructed
certificates without a source-bound digest describe the current executor; they
do not establish the source run's intermediate states.

Campaign suffix execution and whole-tree import capture each runnable candidate
and certificate in one bounded operation with one whole-state hash. Import keeps
every action-prefix certificate and reuses its final captured snapshot. Starting
a search from an explicit action prefix captures the final replayed action in
that same operation; an empty prefix obtains fresh setup evidence. Integrity and
typed incomplete-attempt checks run before captured evidence is accepted. Terminal,
failed or watchdog-interrupted prefixes cannot return an earlier successful
snapshot. Source identity checks and every intermediate imported certificate remain
mandatory; candidate construction still clones the owned observation projection.
Parent reconstruction inside campaign jobs verifies every archived prefix through
a fresh endpoint check. Standalone snapshot capture hashes fresh live state. A
healthy job whose last action is runnable reuses that candidate's hash for its
final certificate; terminal and zero-action jobs, and a job that ends on settle
phase actions applied again, obtain fresh final endpoint and ordered-history
evidence. Missing, foreign, abandoned or
unhashable sessions cannot supply certified KVM endpoints. Failed KVM jobs use an
explicit whole-state-unavailable tag while preserving their checked logical
observations; UML has a separate whole-state-unavailable tag. Whole-state hashes
still scan RAM at fresh endpoint capture and replay of missing suffixes, and cache
admission still verifies all required snapshot pages.

Instrumentation actions become available automatically when the staged image
contains the runtime bridge, nonempty event symbols, and an executable whose
hash matches its instrumentation attestation. A runtime hello identifies the
ready nodes in each incarnation; event faults select only those nodes, so mixed
instrumented and uninstrumented bundles share the same search policy. A step
drawn from the retained-input table is held to the same rule and falls through
to the alphabet when its node is not ready, so the readiness a run recorded
bounds every draw rather than the alphabet alone. The adapter supplies that
alphabet and the duration each drawn action carries; the searcher owns the suffix draw and the
retained-input table. The continuous client and oracle remain image-owned
commands; the oracle decides when its observations are conclusive, including
when some nodes are down.

Every replay run starts from a session no earlier run has touched, and executes
the recorded actions itself, so no snapshot another run cached can stand in for
an action. The setup point comes from the host boot cache
(`SessionConfig::with_boot_cache`, under `HARMONY_CACHE_DIR/boot`) when an
earlier boot with the same image, executable, configuration and host stored
it, and from a fresh boot otherwise; both give the same setup state hash. Each run records
the actions it applied beside the horizons it ran in the guest, and the two are
equal when nothing came from a cache. A search replays each bug it records the
same way, except the bugs a resumed search carries as described below, and reports the bug as confirmed only when the replay reproduced the
evidence the campaign saw: the assertions it violated, or the same stop when
the stop was the only evidence. `bug_found` and `first_bug_execution` come from
the confirmed bugs, so a hit that no replay reproduced is reported and does not
count as a rediscovery. A search that resumes a checkpoint carries a bug's
entry forward from the report of the run it resumed instead of replaying it.
It does so when that report has the same identity, image, kernel and memory size,
and holds an entry with the same execution, actions, stop and assertion sets
whose stored replay agrees with its `confirmed` flag. A replay that ran and did
not confirm is carried as unconfirmed. An entry without a replay result, because
its replay returned an error, is replayed again, and so is every inherited bug
when the earlier report is missing or does not parse.

Search also writes
`campaign-summary.json`, `stream.jsonl`, `progress.jsonl`, `job-host.jsonl`,
`first-bug-input.json`, and one `bug-N.json` per recorded bug
([`report`](src/report.rs)). `job-host.jsonl` holds host measurements, which
differ between runs of the same search: one line per job row of `stream.jsonl`,
joined to it on `sequence` and `reservation`, until a failed write to the file,
which is reported on stderr and ends its lines. `wall_ns` is the job's host wall
time on its worker, from parent restore through the result digest.
`parent_actions` is the parent's depth in actions. `parent_replay_actions` is the archive path replayed from a retained keyframe
snapshot to the parent (0 when the parent's own snapshot was retained).
`parent_rebuild` counts the actions the session re-executed during the job to
reach a prefix it did not hold, as a prefix rebuild or a replayed archive
action, with their host time in `ns`. An action cut off before it returned
counts too. 0 actions means the parent was restored
from a cached snapshot. A campaign records its first 64 bugs. After that
it records a bug only when it violates a property that no recorded bug
violates, so a campaign records at most 64 bugs plus one for each property
first violated after the 64th, and a long campaign whose first bugs share one
property still keeps the input of a later bug of another. A job that runs
several actions records its bug again at each later action that still
violates, so `campaign-summary.json` gives `distinct_reports`, the number of
distinct executions among the `bug-N.json` files. A resumed campaign rewrites
every earlier report with its original execution number, so the
`distinct_reports` of the last summary of a resumed chain counts each
execution once, and adding up the chain's summaries counts earlier ones
again. The summary also groups the reports into `violation_classes`, with
their number in `violation_class_count`. A class is the set of reports that
stopped the same way and violate the same set of properties. A class is
neither an upper nor a lower bound on the number of distinct failure
mechanisms: one class can hold reports from two mechanisms, and one mechanism
can fail different properties in different reports. Counting mechanisms needs
a replay of each report with a workload-specific probe, such as the first raft
log index at which the nodes disagree. Each `bug-N.json`
carries the action list and the
encoded window list used by the guest, including windows inherited from a
branch root. At completion, `first-bug-input.json` holds the first confirmed
finding's actions when one exists; otherwise it holds the first recorded
candidate's actions. Each `hang-N.json` and `failure-N.json` has the same
`actions` and `standing` fields, so `harmony debug run --actions` and replay
accept it as input.

The archive key is a strategy key compiled by the searcher's assertion keys
(`dissonance/searcher/src/search/assertion`). [`key.rs`](src/key.rs) declares
18 values from each state's observations into a catalog, and the strategy
[`faults.toml`](src/faults.toml) keys them. The declarations are:

- `sometimes.count`, the number of Sometimes and Reachable assertions passed,
  and `sometimes.set`, the top 62 bits of the digest of their sorted ids. SDK
  catalog points and JSON assertions both arrive at run time, so the catalog
  carries the passed set as these two aggregates rather than one value per
  point.
- `always.violated`, the number of violated assertions.
- `alive` and `event.ready`, the node masks.
- `edges`, the top 62 bits of the edge digest, and `edges.crossed`.
- `ticks` and the lifecycle counters `hooks.started`, `hooks.finished`,
  `checks.started`, `checks.finished`, `workload.started`,
  `workload.finished`, `event.kill_fires`, `event.park_fires`,
  `event.park_held` and `event.kill_armed`.

A search config's key table can also name facts, each a list of terms:

```toml
[workload.options.key.facts]
stuck_transfer = ["transfer_timeout", "second_transfer"]
```

Each fact declares one `sometimes_all` after the 18 values, in name order: a
count `stuck_transfer`, the number of its terms that hold, and one flag per
term, `stuck_transfer.transfer_timeout` and `stuck_transfer.second_transfer`.
A term is the id of a
Sometimes record that the target's guest processes write; the kit SDK uses
the message as the id. A term holds while the latest hit Sometimes record
with that id has a true condition, so a guest clears a fact by writing the
same Sometimes with a false condition. A record of another kind with the same
id neither sets nor clears the term; two kit SDK assertions of different kinds
share an id when they share a message. The binary SDK reports a Sometimes point only when it
passes, so a fact term must be written as a JSON record. A strategy reads a
term by its dotted name, quoted when the id is not a plain word, for example
`'stuck_transfer.harness: a check read succeeded'`. A term the guest never writes
declares a flag that never holds, and nothing reports it, as with a declared
game value that never changes; the archive report's `assertions` lists the
ids the guest wrote. A fact term's records also count in `sometimes.count`
and `sometimes.set` like every other passed Sometimes, and stay there after
the fact clears.

A fact name is ASCII letters, digits and underscores, starts with a letter or
an underscore, and is not the first part of a built-in value's name, such as
`sometimes` or `alive`. A fact holds 1 to 64 terms, and the facts together
declare at most 77 values: a record holds 95 values beside the reserved
`action_end`, and the built-in values take 18. The key policy identifier
covers the catalog, so it pins the facts, and the vocabulary identifier does
not change. A target keeps its key table in the search config it runs with,
beside `[workload.options]` lines such as `hold_hooks`.

The shipped strategy's tier is the passed count. Its place is the
passed set and every lifecycle count except liveness, clamped at 8, plus two
flags for faults still in effect when the state is saved: a thread held by an
event park on any node, and an event kill armed on any node. Liveness and the
edge digest are the holder identity inside the place. The selector's tiers
rank states by the passed count and draw most parents from the states that
passed the most. Places inside a tier rank by their draw counts.

The place and the identity are each a 64-bit product of their lanes' ranges,
so the shipped strategy keeps these widths:

| Value | Kept | Saturates at |
|---|---|---|
| `sometimes.set` | top 34 bits of the 128-bit digest | not applicable |
| `edges` | top 48 bits of the 64-bit edge digest | not applicable |
| `sometimes.count`, `always.violated` | exact | 65,535 |
| lifecycle counters | exact | 2^62 − 1 |
| lifecycle place lanes | exact | 8 |

Two distinct passed sets share a place, and two distinct edge digests share
an identity, when their kept bits agree. Among n distinct values of a w-bit
fingerprint the expected number of colliding pairs is about n^2 / 2^(w+1):
about 0.07 for 50,000 passed sets at 34 bits, and about 4 in a million for
50,000 edge digests at 48 bits.

The node count has these limits:

| Limit | Nodes | Checked by |
|---|---|---|
| platform supervisor | 64 | `FaultVocabulary` |
| duration context, strategy reads `alive` and `event.ready` | 29 | `FaultKeys::new` |
| duration context, strategy reads one of them | 59 | `FaultKeys::new` |
| duration context, strategy reads neither | the supervisor's 64 | `FaultVocabulary` |
| strategy reads `alive` or `event.ready` in any line | 62, the width of a declared value | `FaultKeys::new` |
| shipped strategy's place width | 10 | the strategy compile |

A strategy for a bundle of more than 10 nodes drops or coarsens place lanes.
When a strategy's lanes do not fit the bundle, the error names the largest node
count that strategy keys.
The 62-node check looks for the lane names `alive` and `event.ready` in the
strategy text, so above 62 nodes a strategy that mentions either name anywhere
is refused, whether or not its compiled lines read that lane.

The duration context is a function of the parent key alone, because replay
validates the recorded context. It is read from the key's lanes named
`hooks_running`, `event.ready`, `kill_fires`, `park_fires`,
`checks_finished`, `workload_running` and the identity lane `alive`; a
strategy without one of them leaves that part of the context zero, and packs
no bits for a node mask it does not read. A strategy may give stages, latched
stages and aggregates. Every stage reads the shared `spread` and `identity`
lines, given at the top level or under `[every_stage]`, because the duration
context reads its lanes from them, so `FaultKeys::new` refuses a `[[stage]]`
that gives its own `spread` or `identity`.

A reject line, at the top level or in a stage, marks states the search does
not keep. An action endpoint that matches one while the strategy's `goal` line
does not hold is a doomed terminal, whether the guest was still running or had
stopped, unless the run's objective counts a bug there. `Compiled::observe`
gives it no key and leaves the accumulator as the
action found it, so it advances no stage and counts no stage fall. The job
stops at it, it is never an archive entry, the settle phase skips it, and the
campaign report counts it in `doomed_endpoints` rather than in the archive
report's `deaths`. The evidence takes nothing of it into the milestones, the
progress watermark or the champion input. Its assertions still count in the
archive report's `assertions` coverage, and a property it violates is still a
finding. A rejected endpoint where the objective counts a bug is that finding,
and the evidence merges it as it merges any finding.

Each `FaultTarget` carries the strategy's accumulator, the searcher's
`Accumulator`. The target folds its observation into a fresh accumulator with
`action_end` false when it starts or resets, and folds each completed action's
endpoint observation once with `action_end` true. Every way of applying an
action goes through that one fold: a new action, a replayed parent action, a
rebuilt prefix action and a settle action. A `FaultSnapshot` saves the
accumulator and the endpoint's `Observed` key, and a restore takes both from
the snapshot, so a job's parent, a detached settle run and the action applied
after it continue from the fold the snapshot saved. A rebuilt prefix that stops
before its certified endpoint ends the requested action with stop
`Unexpected`; that action has no endpoint, so the target folds nothing, and
the state has no key and counts no stage fall. A target without the
strategy's keys also folds nothing and gives each action endpoint no key. The snapshot codec
`fault-history-dag-v7` and the campaign stream and checkpoint formats carry
both. A rooted search (`search --from BRANCH`) starts the accumulator at the
root: a branch root carries only its endpoint observation, so its accumulator
is that observation folded into a fresh accumulator, as at genesis.

A staged strategy's archive report and `campaign-summary.json` list
`stage_falls`: every stage with the
number of action endpoints whose stage is below the stage of the state the
action departed from, counted under the stage they fall to. The games make the
same count with `Checker::stage_falls` when they re-key their records, and
count keyed records only. The target counts falls live (ours): at each
completed action whose endpoint has a key, it compares the departure
accumulator's stage with the endpoint's and records a fall in that action's
`FaultObservations::stage_fall`. An endpoint without a key counts no fall. The
campaign evidence counts the falls of every action it merges, kept or not. A
fall inside an action is not seen, and
an action of a detached settle run counts only when the job applies it again.
A tier strategy's report and summary have no `stage_falls`.

[`strategy-keys.json`](src/key/strategy-keys.json) records the key and
duration context the shipped strategy gives 64 sampled states for each of 1,
3, 8 and 10 nodes, and a test checks that the strategy still gives them.

`workload.options.key.strategy` in a search config replaces the shipped
strategy text, and `workload.options.key.facts` names the facts. The key
table accepts no other field.

The Consonance backend needs Linux and KVM, or HVF on macOS. The UML backend
needs Linux. The action model, the bundle
parser, the archive key, the image preparation and the report shapes are
portable and tested everywhere.

Replay summaries include completed-check provenance automatically for bundles
with a continuous `check`. The supervisor records the check run, its pid, and
the disturbance generations at its start and completion. The host adds the
Sometimes and Reachable assertions that pid passed during the run, and the
pending process faults. A case can require evidence from a successful check that
started and finished in the final generation with no outstanding fault.
Assertions passed anywhere else remain exploration evidence and cannot establish
this recovery condition.
Bundles that use drawn hooks have `check: null`; their evidence comes from those
hooks instead.

Replay summaries count event-kill and event-park fires, which shows whether a
recorded input's event actions ran. The settlement wait after an input keeps
the last action's coverage quantum.

## CLI investigations

The CLI supplies an optional supervisor bundle generated from TOML. Preparation
mounts it as a read-only external input at `/etc/harmony/cli.bundle`; it never
rewrites the source image. The prepared initramfs is retained by the CLI along
with the kernel, vocabulary and execution configuration.

`package::SearchStart` selects genesis, a recorded action prefix, or a whole
search checkpoint. Prefix origins replay before capturing a root. Findings
retain that prefix, so confirmation still executes the complete history from
boot. Search checkpoints persist workload evidence as well as the generic
coordinator state. The package writes periodic checkpoints every 100 admissions
and a final checkpoint at shutdown. A CLI-selected checkpoint carries its
journal digest into the searcher, which rejects a file swapped after selection.

Fresh scenario executions retain `ReplaySummary::timeline`: the observation,
recorded action including coverage quantum, and console evidence at each action
boundary. Recovery-check waits are labeled separately. A step's `console` holds
only the guest output since the previous step. The session's `console_since`
cuts it at the byte count the guest console has emitted, so steps do not
overlap, and a character split across two steps is kept whole in the later
step. A step keeps at most its newest 64 KiB and counts the rest in
`console_dropped`. A step that ends inside a
character holds those bytes in the replay's cursor rather than reading them
again, and the last step keeps any unfinished character as a replacement
character. `ReplaySummary::console_through` joins the steps up to a step and
marks each `console_dropped` gap with a line naming the bytes not kept, so
text is never joined across a gap. `harmony show --logs` prints that console
for a selected step, and `harmony show --timeline --json` carries it in the
selected step's `console`; without a selected step, `show --logs` prints each
step's own output.
Console evidence is diagnostic and does not enter archive identity or
scheduling. Branch previews stop at their exact action
prefix without adding settlement actions.

`FaultCampaignRun::violations` names the violations a run's objective needs: a
bug counts only when its observation reports one of them, and an empty set
(the search's run) counts every bug. `package::shrink` runs the searcher's
`search::shrink` on a finding's actions with that run, and
[`shrink`](src/shrink.rs) holds the faults shrink hooks, which the campaign
traits call: a tape costs its
fault count and then its ticks, a fault's simpler actions are a wait of the same
length (keeping its `choice` and coverage quantum) and then the same fault for
half its ticks, a wait's is a wait for half its ticks, and the lower bound is
zero. It sets `primary_shortcuts`, so shortcut windows cover only the faults,
turns `ordinary` off, so every job is a shortcut or a continuation, sets
`shortcut_snapshots`, so a shortcut's steps enter the archive, sets
`until_shortcuts_exhausted`, and runs an admission window of one, so each job
starts from the tape the previous job left. Its goal tail is the
replay's settlement waits, so a candidate whose violation appears only while the
replay settles still counts, as it does in a report replay.
Its workers come from the same memory plan as a replay, and every replay
target shares one snapshot cache.

UML boots the shared `/usr/lib/harmony/init` entrypoint directly. This keeps
application output on UML’s console instead of passing through the hardware
arm64 MMIO console wrapper.

## Investigation roots

The package can prepare a recorded prefix for guest debugging and export the
resulting state as a durable root. A root binds the prepared boot identity to a
cache extent containing pages and device/service sidecars, plus every standing
fault window that started by its snapshot moment. A closed window stays in the
root because the supervisor's next poll may still ask for windows that closed
after its previous poll. Import validates that
identity and the extent; root bytes also contribute to the workload identity.
The CLI verifies and loads the saved root once before starting a search, then
passes those bytes to every worker. Later action prefixes/checkpoints
are relative to it. A continuation retains inherited standing windows, including
an unfired event kill armed before the root, while it adds its new action windows.
Commands and interactive input are diagnostic evidence, not
an instruction to execute those commands again during restoration.

The CLI exposes this through `branch --exec`, `branch --exec-file` and
`branch --shell`. `search --from BRANCH` restores the saved guest state.

Campaign coordinator bootstrap and checkpoint resume explicitly finish their
thread-local session before releasing execution-worker readiness. This boundary
also runs after bootstrap construction or replay errors and on unwinding. It
removes LIVE before teardown, so a process exit need not run main-thread TLS
cleanup. Unconfirmed owned-session exit fails an otherwise successful bootstrap;
a prior typed execution error remains primary, with a secondary cleanup diagnostic.
Linux targets continue reusing their thread-local session between worker jobs.
Target Drop is not a universal session-finalization boundary.

Final cumulative telemetry is captured once before explicit session shutdown and
merged into retired counters without a post-close RPC. It excludes teardown work;
telemetry failure does not prevent the owned shutdown attempt. Bootstrap scope
acquisition refuses nesting atomically, preserving the outer scope's ownership.

The generic searcher's standalone full and prefix replay mechanism owns the
coordinator thread's session for the whole operation and finalizes it on every
result. macOS target Drop defers to that active scope, preserving session reuse
inside it. The faults CLI's own replay commands use FaultTarget directly and do
not enter this generic replay scope.

The standing service declares standing questions and inactive debug questions
local to the installed deterministic handler, so the VMM answers them without a
Decision stop and a resolution round trip to the coordinator. A question asked
at or after the run deadline still stops the run unanswered, and the next
action's handler answers it. The attached debug handler declares no local
services, so standing and debug questions both take the external path while it
is installed. The supervisor still makes each actual debug SDK request:
attachment after a standing reply is delivered at the same next debug request,
without a dormant flag or an extra tick. Local answering removes only the
external Decision/resolution exchange; guest doorbells and mandatory guards
remain. Both handlers decode debug Status before answering. Standing and debug
identities are respectively faults-standing-coverage-v5 and
faults-debug-terminal-v2.

The generic local-answer capability requires unchanged captured handler state;
the inactive standing debug response satisfies it. Stateful local responses are
rejected before publication. Active debug remains on the external path.

Coordinator measurement can read `coordinator_retired_work_telemetry` only when
the calling thread has no LIVE session. The accessor rejects before any session
query and returns an owned copy of retired counters. This supports bootstrap
attribution after explicit coordinator finalization without creating a session,
querying a closed child, or measuring shared cache counters twice. It does not
finalize a session or imply teardown success; the lifecycle receipt remains
independent. Ordinary thread work and residency diagnostics are unchanged.

Thread work reports add final controller counters across retired LIVE sessions.
For SDK attribution, `vmm.sdk_dispatch.event_shape_slots` therefore sums
per-session filled-slot memberships and can exceed 32. Equal shape keys from
different sessions have their values added, so this number need not equal the
number of distinct named rows in the thread report. The 32-row witness cap
applies per producer table; the accumulated thread report can retain keys from
multiple sessions and has no global 32-row bound. Neither this field nor the
shape rows report current residency or enter archive memory policy.
Fault endpoint certificates bind the ordinary, unqualified execution-assurance
identity as well as state, event history and observations. History receipts keep
and validate that identity. A KVM whole-state certificate is evidence for the
observed replay, and cannot be promoted into CPU portability qualification;
`HistoryReceipt::require_qualified` refuses unsupported finite compositions.
UML keeps `UmlWholeStateUnavailable` and has no whole-state certificate. See the
[execution assurance contract](../../consonance/execution-contract/README.md).

Execution-endpoint proofs carry the actual session's admitted assurance identity
with its measured state and ordered history. The history consumer compares that
returned identity with the parent receipt before changing observations or
extending the receipt. A relabelled endpoint proof refuses even when its other
binding fields are unchanged. The combined worker protocol requires host hello5;
both incompatible hello4 schemas and all older versions are refused.

The shared history codec is `fault-history-dag-v7`, whose endpoint frame also
carries the snapshot's accumulator, observed key and the endpoint observation's
kept first-crossing sites. Every receipt frame retains
its ordinary-unqualified `assurance_identity`, includes it in the semantic node
root, and validates it before encoding/deduplication and after decoding.
Changing assurance is refused even if the receipt root and frame checksum are
recomputed. The codec does not upgrade an ordinary execution into a qualified
composition. Older DAG codecs and flat snapshot envelopes are refused;
whole-search checkpoints require the distinct v10 envelope. The current
execution identity remains v11 and ordered event history remains v3.
