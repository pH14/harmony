<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# uml

`uml` runs the User-mode Linux profile built by
`consonance/harmony-linux/uml` as an ordinary host process.

- `Profile::load` reads `profile.json`, checks the schema, the SECCOMP
  userspace mode, the host architecture and the empty host library list, then
  re-hashes the executable, config and rootfs and checks that the executable
  has no ELF interpreter. `VerifiedProfile::identity_sha256` hashes the parsed
  profile.
- `HostIdentity::current` reads the CPU model and feature flags from
  `/proc/cpuinfo`.
- `Launch` builds the UML command line: memory size, initramfs,
  `seccomp=on`, `time-travel=inf-cpu` starting at zero, console 0 on the
  output pipe, every other console off, and a per-launch work directory for
  `TMPDIR` and `uml_dir`. The environment is cleared except for `TMPDIR` and
  `GLIBC_TUNABLES`. With a `Bridge`, it adds `harmony_fd=3` and the boot seed.
- `Bridge` serves the guest's `/dev/harmony` requests on a `SOCK_SEQPACKET`
  pair from a thread of its own. It answers entropy from the seed, records
  each event with its virtual time, and fails the run if virtual time goes
  backwards. With a cut, it stops answering at that event, and the guest
  stops with `ExitReason::EventCut`.
- `Session` holds the bridge socket on the caller's thread instead. It runs
  the guest to an event count and pauses it before the answer, takes a
  `Checkpoint` there, restores one in place, or starts a fresh process that
  restores one at boot. A `Checkpoint` is the guest image in a memfd plus the
  bridge state at the capture: entropy, events, virtual time and the pending
  answer. Sessions are Linux only.
- `Recording` names the profile, host, seed, memory, boot arguments, event
  count and event hash of a run. `Recording::check` refuses a different
  profile or host, and `Recording::launch` replays to the recorded cut.
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
