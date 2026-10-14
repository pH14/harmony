<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# uml

`uml` runs the User-mode Linux profile built by
`consonance/harmony-linux/uml` as an ordinary host process.

- `Profile::load` reads `profile.json`, checks the schema, the SECCOMP
  userspace mode, the host architecture and the empty host library list, then
  re-hashes the executable, config and rootfs and checks that the executable
  has no ELF interpreter. `VerifiedProfile::identity_sha256` hashes the parsed
  profile. On Linux it copies and re-hashes those three files into sealed
  memory-backed descriptors before returning. The executable and rootfs
  descriptors remain open through child spawn, so later path changes cannot
  substitute different bytes for the verified profile. The child receives the
  executable at descriptor 240 and rootfs at 241; an initramfs override uses
  242. These stable numbers let a checkpoint captured by one worker restore in
  another even when their parent descriptor layouts differ. The launcher
  reserves those numbers for its lifetime and refuses profiles if another host
  resource already occupies one.
- `HostIdentity::current` reads the first processor allowed by the calling
  thread and its CPU model and features from `/proc/cpuinfo`. Each `Launch`
  admits one processor on first use and shares that admission with its clones;
  moving an ordinary launch to a thread that excludes that processor refuses
  execution. A caller can instead bind a verified expected host identity when
  multiple worker threads must run guests on one canonical processor. The
  launcher checks that identity and pins every guest to its admitted processor
  in its final pre-exec step, after caller-supplied callbacks. The child fails
  to spawn if its cpuset no longer permits the processor. Qualification's
  pinned replay perturbation contends for the actual guest processor and
  verifies the final affinity despite an earlier override callback.
- `Launch` builds the UML command line: memory size, initramfs,
  `seccomp=on`, `time-travel=inf-cpu` starting at zero, console 0 on the
  output pipe, every other console and serial line on `null`, and a
  per-launch work directory for
  `TMPDIR` and `uml_dir`. The environment is cleared except for `TMPDIR` and
  `GLIBC_TUNABLES`. With a `Bridge`, it adds `harmony_fd=3` and the boot seed.
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
  services answer the same SDK requests as the KVM backend through
  `environment::sdk`: entropy, events, assertion violations, standing-service
  questions answered by a `ServiceHandler`, payload pulls and coverage.
  `branch` installs a seed, handler and payloads after checking that reseeding
  with both the branch seed and the decode seed zero keeps the handler identity
  and configuration unchanged. A rejected branch leaves the live services
  untouched. `run_until` runs to a
  virtual-time deadline, and `run_to_snapshot_point` runs to the guest's
  snapshot request. A request already paused at or beyond a deadline stays
  unanswered when the deadline is checked again. Each returns a `Stop`: an
  event count, deadline, snapshot point, violation, exhausted input, a closed
  bridge, or a guest exit. A guest whose virtual time
  stops moving for the progress limit fails with `SessionError::Hung`. The
  session pauses the guest before each answer, takes a `Checkpoint` there,
  restores one in place, or starts a fresh process that restores one at boot.
  At restore, the console collector drains output already written by the
  paused guest and clears its diagnostic tail before the new branch runs. This
  keeps text from a discarded branch out of later crash classification and
  diagnostics. Its cumulative byte count still enforces the host output limit.
  After a terminal guest stop, the client waits for the host process to exit,
  finishes process cleanup, and joins the console collector before using the
  tail to classify a kernel panic. A closed bridge whose process does not exit
  within the progress limit fails instead of guessing from partial output.
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
- `Recording` names the profile, host, seed, memory, boot arguments, event
  count and event hash of a run. Schema 2 includes the selected host processor.
  It derives the seed from the live launch's
  bridge and refuses an unbridged run or a replaced initramfs, which it cannot
  replay. Recording creation derives identity from the admitted `Launch`;
  replay checks read the recorded processor's current identity. A caller cannot
  supply a stale identity in either path. `Recording::check` refuses a
  different profile or host, and `Recording::launch` binds the recorded
  processor and identity through the cut even if the caller skips `check`.
- `Guest` starts the process in its own process group with a parent-death
  signal, merges stdout and stderr into one pipe, and keeps a bounded console
  tail. It stops the guest at a wall-clock limit or a console byte limit.
  Cleanup kills the process group, reaps it, kills any process still in the
  group or session, and removes the work directory. `Exit` reports any
  process that survived the sweep.

The parent-death signal follows the thread that spawned the guest, so a guest
must be started from a thread that outlives it.

`harmony-uml-qualify` is the qualification binary described in
`consonance/harmony-linux/uml/README.md`.

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
