<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# uml

`uml` runs the User-mode Linux profile built by
`consonance/harmony-linux/uml` as an ordinary host process.

- `Profile::load` reads `profile.json`, checks the schema, the SECCOMP
  userspace mode, the host architecture and the empty host library list, then
  re-hashes the executable, config and fixture rootfs and checks that the
  executable has no ELF interpreter. It also requires the entire parsed
  profile to match the architecture's source-reviewed manifest in
  `reviewed-profiles`. This manifest pins the kernel, config and fixture
  rootfs digests, kernel version, patch-series digest and virtual-time costs
  independently of the caller's `profile.json`; rewriting both an artifact
  and its self-declared digest cannot admit a different owned kernel.
  Workload initramfs overrides have a separate image-admission boundary.
  The x86 qualification check uses a test-only initramfs override with the
  reviewed kernel to execute direct `WRFSBASE` and `WRGSBASE`, then compares
  guest-visible FS and GS bases after reused and fresh checkpoint restores.
  It requires a host that exposes FSGSBASE and leaves the reviewed fixture
  rootfs unchanged.
  `VerifiedProfile::identity_sha256` hashes the parsed profile. On Linux it
  copies and re-hashes those three files into sealed
  memory-backed descriptors before returning. The executable and rootfs
  descriptors remain open through child spawn, so later path changes cannot
  substitute different bytes for the verified profile. The child receives the
  executable at descriptor 240 and rootfs at 241; an initramfs override uses
  242. These stable numbers let a checkpoint captured by one worker restore in
  another even when their parent descriptor layouts differ. The launcher
  reserves those numbers for its lifetime and refuses profiles if another host
  resource already occupies one.
- `HostIdentity::current` reads the first processor allowed by the calling
  thread and its CPU model and features from `/proc/cpuinfo`. It also records
  the exact CPU-info lines the pinned UML kernel imports at boot. On x86, the
  kernel consumes lines containing `flags` or `cache_alignment` until it has
  seen two matches; those lines may belong to processors other than the one
  running the guest. On arm64, it consumes leading `cache_alignment` lines
  before the first ordinary line. The identity also records the host
  auxiliary-vector hardware capabilities, minimum signal-stack size, and
  platform string, which UML reads at boot. It reads the platform string's
  bytes from the parent process, rather than recording its process-specific
  pointer. `HostIdentity::identity_sha256` binds all recorded fields with
  lengths. Each
  `Launch` admits one processor and host identity on first use and shares that
  admission with its clones;
  moving an ordinary launch to a thread that excludes that processor refuses
  execution. A caller can instead bind a verified expected host identity when
  multiple worker threads must run guests on one canonical processor. The
  launcher checks that identity and pins every guest to its admitted processor
  in its final pre-exec step, after caller-supplied callbacks. The child fails
  to spawn if its cpuset no longer permits the processor. Qualification's
  pinned replay perturbation contends for the actual guest processor and
  verifies the final affinity despite an earlier override callback. The host
  identity is checked before spawn. At the final check, `Guest::start` passes
  the admitted parser-input lines through `HARMONY_UML_CPUINFO`; the owned UML
  kernel consumes those bytes instead of reopening `/proc/cpuinfo`. A changed
  host CPU-info file after the check cannot alter the boot import. Schema 4
  profiles require the matching owned kernel patches for the CPU-info import
  and auxiliary-vector check: the launcher passes the admitted values
  at spawn, and the kernel refuses a child whose initial vector differs before
  it imports guest-visible features. Missing or malformed pinned values refuse
  boot on both x86 and arm64.
- `Launch` builds the UML command line: memory size, initramfs,
  `seccomp=on`, `time-travel=inf-cpu` starting at zero, console 0 on the
  output pipe, every other console and serial line on `null`, and a
  per-launch work directory for
  `TMPDIR` and `uml_dir`. The environment is cleared except for `TMPDIR`,
  `GLIBC_TUNABLES`, and the pinned boot CPU-info and auxiliary-vector values
  passed at `Guest::start`.
  With a `Bridge`, it adds `harmony_fd=3` and the boot seed.
  It refuses extra kernel arguments that would replace these settings or
  attach host files, disks, consoles or network devices, because a
  `Recording` cannot carry those host resources. `set_initramfs` likewise
  copies an override into a sealed descriptor when configured, and every
  later launch from that `Launch` uses the same bytes. Linux launches normalize
  the host personality to `ADDR_NO_RANDOMIZE` before executing the pinned
  kernel. If that request is denied, it clears the inherited flags with
  `personality(0)`; if both calls fail, launch is refused. This avoids UML's
  `/proc/self/exe` re-exec, which cannot reopen a memory-backed executable by
  its symlink target. If the host denies that request, the patched kernel's
  randomized-layout fallback applies. Qualification deliberately clears the
  request before its randomized rounds.
- `Bridge` serves the guest's `/dev/harmony` requests on a `SOCK_SEQPACKET`
  pair from a thread of its own. It answers entropy from the seed, records
  each event with its virtual time, and fails the run if virtual time goes
  backwards. With a cut, it stops answering at that event, and the guest
  stops with `ExitReason::EventCut`. A cut at zero stops the guest before its
  first request and records no events.
- `Session` holds the bridge socket on the caller's thread instead. Its
  services answer SDK requests through `environment::sdk`: entropy, events,
  assertion violations, recorded standing-service answers, payload pulls and
  coverage. An unrecorded custom service or coverage quantum pauses the socket
  before a guest reply. `resolve_and_run` accepts a matching answer, records it
  against the held question, and then sends the guest response. Invalid
  resolutions leave the held request and services state unchanged. A recorded
  coverage schedule supplies a branch's window quantum without per-callback
  decisions.
  It records the host process loader generation before starting the guest,
  checks it across session operations, and checks again after every service
  decision immediately before sending the guest reply. A handler that loads
  and unloads a library during one decision fails the run even if the set of
  loaded images is the same afterward. The client also holds its admission
  generation across fresh UML processes and checkpoint publication.
  `branch` checks that reseeding with both the branch seed and the decode seed
  zero keeps the handler identity and configuration unchanged, then installs
  the handler, payloads, optional coverage schedule, and a fresh host-bridge
  entropy stream from the branch seed. The guest kernel's `getrandom` stream
  starts from the boot seed and remains part of checkpointed guest state; a
  branch does not reseed it. A plain restore retains the captured host-bridge
  entropy position. A decode-seed probe that changes the original handler is
  rejected. A rejected branch leaves independent live services untouched; if
  a shallow handler clone changes the live service state, the service channel
  is poisoned so it cannot export a misleading checkpoint.
  `run_until` runs to a
  virtual-time deadline, and `run_to_snapshot_point` runs to the guest's
  snapshot request. A request already paused at or beyond a deadline stays
  unanswered when the deadline is checked again. Each returns a `Stop`: an
  event count, deadline, snapshot point, violation, exhausted input, or a
  provisional terminal observation. With no earlier stop due, bridge EOF,
  process exit or signal, and a host-enforced console or wall limit all report
  `Stop::Closed` regardless of which host poll wins. Callers use `Session::run`
  or `settle_terminal_console` to classify the final process reason;
  settlement probes the current process state and refuses host limits and
  signaled host exits after process cleanup; callers cannot supply an earlier
  exit reason. A host signal such as SIGKILL is an infrastructure failure, not
  a guest shutdown or crash, and the client abandons the session. A
  guest whose virtual time
  stops moving for the progress limit fails with `SessionError::Hung`. The
  limit is supplied when the session starts, so a fresh process restoring a
  checkpoint is bounded while it waits for the restore request and replies.
  An in-place restore also uses that limit for its bridge acknowledgements.
  A capture acknowledgement that stops arriving reports the same limit, and
  the client abandons that incomplete session.
  The session pauses the guest before each answer, takes a `Checkpoint` there,
  restores one in place, or starts a fresh process that restores one at boot.
  At restore, the console collector drains output already written by the
  paused guest and clears its diagnostic tail before the new branch runs. This
  keeps text from a discarded branch out of later diagnostics. Its cumulative
  byte count still enforces the host output limit.
  After a terminal guest stop, the client waits for the host process to exit,
  finishes process cleanup, and joins the console collector before recording
  the diagnostic tail. The kernel-owned exit status classifies a panic. A closed
  bridge whose process does not exit within the progress limit fails instead
  of guessing from partial output.
  The host owns the guest's physical memory file and an image file, both
  memfds, and maps them side by side. A `Checkpoint` is a snapshot in the
  session's `Checkpoints` page store plus the services state at the capture.
  A capture stores the image and the physical pages the kernel reports as
  written since the last capture or restore, as a delta on the previous
  checkpoint. A restore writes back only the pages that differ between the
  current state and the target. `Checkpoints::delta` lists a checkpoint's
  pages relative to a base for publishing, `Checkpoint::sidecar` encodes the
  services state, and `Checkpoints::import` rebuilds a checkpoint from both. An
  import rejects trailing sidecar bytes, a private image length above the
  256 MiB live image limit, and pending requests whose size or virtual moment
  could not have come from a paused guest. Both import paths check the image
  length and match the recorded physical-memory size before touching the page
  store. The sidecar and checkpoint digest bind the guest RAM size even when
  its additional pages are all zero. The live session rejects oversized
  bridge requests before they can become checkpoints.
  Reading a checkpoint's image length and building its sidecar verify the
  sealed memory-state digest, so corrupted length metadata fails rather than
  exporting a sidecar with a zero length.
  Imported page payloads must match their hashes even when the nearby snapshot
  already has the claimed page; the import then reuses that resident page.
  `Checkpoint::digest` hashes a checkpoint's pages and services state.
  Checkpoint handles belong to their `Checkpoints` store. Export, import, and
  live restore reject a handle from another store before using its snapshot ID;
  cloning `Checkpoints` retains the same store ownership. A checkpoint dropped
  inside an export callback is released at the next store access, after the
  callback's page borrow ends.
  `Checkpoints::import` reproduces the published checkpoint only onto a base
  with the publisher's digest, because a base also holds per-process host
  values: the C library's stack canary and pointer guard, host process IDs
  and the work directory name. `Session::state_hash` refuses a full-state
  digest because a stable cross-process identity for guest memory is not yet
  available. `bridge_state_digest` covers only services and the pending request;
  the client combines it with the full setup checkpoint digest for its
  setup-scoped cache identity. It must not be compared as a whole-state replay
  proof. Sessions are Linux only.
  Public UML sessions require a sealed static Linux host in every build.
  Admission hashes the full sealed main executable. Before and after session
  operations and each service exchange, the guard reuses that immutable file
  digest while comparing live executable bytes and the complete executable-map
  census. An ordinary GNU executable cannot start a UML session.
- `Recording` schema 6 names the admitted Harmony executable, profile, full
  host identity, seed, memory, boot arguments, event count and event hash of a
  run. Earlier schemas are refused. On Linux, the executable identity requires
  the same sealed static host as a session. `Recording::new`, `check`, and
  `launch` all refuse an unsealed or dynamically linked host before they can
  certify or replay a recording. A replacement at the original path cannot
  make a different host build claim the recording. The seed comes from the
  live launch's bridge; an unbridged run or replaced initramfs is refused.
  Replay checks read the recorded processor's current identity and refuse a
  different executable, profile, or host. `Recording::launch` binds the
  recorded processor and identity even if the caller skips `check`.
- `Guest` starts the process in its own process group with a parent-death
  signal, merges stdout and stderr into one pipe, and keeps a bounded console
  tail. It stops the guest at a wall-clock limit or a console byte limit. After
  an automatic stop, cleanup joins the console reader and bridge before
  classifying the exit. Apart from reserved host-failure status 87, output
  already written beyond the byte limit is a console-limit stop, including
  when an event cut or another automatic stop was observed before the reader
  caught up. Without an overflow or host failure, a completed event cut or
  bridge failure takes precedence over the process status, even if the exit
  poll won the race. An explicit kill remains a kill. A UML
  session refuses a console-limit host stop instead of treating it as a guest
  crash. The owned UML kernels reserve process status 87 for an internal host
  clone failure. `Guest` reports `ExitReason::HostFailure` even if the bridge
  or console collector also observes the exit, and `Session` refuses to settle
  it as a guest shutdown or make it an accepted search stop. The signal is
  the host process status; the diagnostic console line is only for diagnosis.
  Cleanup kills the process group, reaps it, kills any process still in the
  group or session, and removes the work directory. `Exit` reports any
  process that survived the sweep.

  A kernel panic exits with reserved status 86 and is reported as
  `ExitReason::GuestPanic`. The client classifies that kernel-owned status as
  a panic crash; console text cannot claim one. The kernel rejects guest
  `/proc/exitcode` writes selecting 86 or 87, including low-byte aliases.
  Host signals and fatal UML traps remain control failures.

The parent-death signal follows the thread that spawned the guest, so a guest
must be started from a thread that outlives it.

`harmony-uml-qualify` is the qualification binary described in
`consonance/harmony-linux/uml/README.md`. Qualification CI builds it as a
static-musl executable. Its suites reexec from a fully sealed memfd before
using public Recording and Session admission, both natively and inside the
Docker qualification container.

The crate's `unsafe` blocks are host system calls: sequenced-packet sockets,
descriptor passing, `memfd_create`, fixed mappings and process-group signals.
Miri cannot run those calls, so the tests that make them are ignored under
Miri and the qualification suites exercise them on Linux; Miri runs the
crate's other library tests.

`Checkpoints::full` exports all nonzero pages relative to the store's zero root.
`import_full` restores that complete state over a fresh launch, removing stale
pages as well as restoring changed ones. Ordinary cache deltas still use their
explicit setup checkpoint. This distinction makes durable investigation roots
portable across launches without treating volatile host-private boot memory as
an interchangeable baseline. Profile and launch identity checks belong to the
client that persists the root.
