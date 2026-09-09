<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Guest-kernel patches

This directory contains diffs against the Linux version pinned in
`../versions.lock`. The x86 and arm64 build scripts apply only the series for
their target architecture.

Kernel patch content remains under the kernel's GPL-2.0 license. First-party C
code elsewhere in the repository uses the repository license. Keep kernel
changes in diff form under this directory.

## Layout and application

- `x86/` is applied in lexical order by `../build-kernel.sh`. An already
  applied series is accepted; a partially applied or drifted tree is rejected.
- `arm64/` is applied in lexical order by `../build-arm64-kernel.sh` to a
  freshly extracted source tree.

To regenerate a patch, extract the pinned kernel on a case-sensitive
filesystem, apply the current series, make the change, and create a unified
diff against a pristine extract. Preserve the explanatory preamble before the
first diff header.

After an x86 clock-source change, run the counter-opcode scan and update
`../rdtsc-allowlist.txt` (and `../rdtsc-allowlist-faultlab.txt`, the
fault-library kernel's baseline) if the deliberate
instruction count changes. After any kernel patch change, run the image test to
regenerate and verify `../MANIFEST.sha256`.

## x86 series

- `0001-x86-harmony-pvclock-exit-count-clocksource.patch` adds
  `CONFIG_HARMONY_PVCLOCK`, the non-interpolating paravirtual clock source, and
  its one-shot doorbell registration. It is inactive unless the
  `harmony_pvclock` kernel parameter is present.
- `0002-x86-harmony-character-device.patch` adds `/dev/harmony`, attributed
  event delivery, and deterministic entropy transactions over the existing
  doorbell.
- `0003-x86-harmony-N6-user-counter-trap-switch.patch` adds the default-on
  `CONFIG_HARMONY_USER_COUNTER_TRAPS`, which arms `CR4.TSD` for an exec image
  on an active Harmony boot and refuses a later `PR_TSC_ENABLE`. Patch 0007
  serves the resulting counter faults from virtual time. Turning it off for
  one kernel makes ring-3 counter confinement testable on its own.
- `0004-x86-harmony-syscall-tick.patch` rings the virtual-time tick at every
  context switch and idle-poll iteration, so an armed clock event comes due
  even after the runnable task blocks and the sole vCPU enters idle polling.
- `0005-x86-harmony-early-boot-entropy.patch` recognizes the `harmony_pvclock`
  token in `boot_command_line` before `SETUP_RNG_SEED` is credited, keeping a
  native counter read out of the first CRNG key.
- `0006-x86-harmony-task-park.patch` adds `CONFIG_HARMONY_PARK` and
  `/dev/harmony-park`, through which a supervisor holds one thread of a
  workload at a user instruction on its k-th execution: a per-thread hardware
  execution breakpoint counted in the kernel, and a sleep taken on the thread's
  own return to user mode. Only the fault-library kernel enables it; with the
  symbol off, the system-call entry poll is an empty inline and every other
  kernel's bytes are unchanged.
- `0007-x86-harmony-user-counter-emulation.patch` completes trapped userspace
  `RDTSC` and `RDTSCP` from the clock page's materialized counter. Each read
  emits the existing execution tick before sampling; `RDTSCP` also returns
  the single guest vCPU's index as `TSC_AUX`. The handler decodes user code
  through Linux's checked instruction fetch and leaves unrelated faults on
  the normal signal path. Confinement is enabled in both production profiles;
  the N6 traps-off kernel remains a deliberate negative control.

The clock source contains two deliberate `rdtsc` instructions. The reviewed
allowlist records their locations. The x86 build rejects unaccounted counter
reads.

## arm64 series

- `0002-arm64-harmony-pvclock-exit-count-clocksource.patch` redirects the
  generic counter accessors to the guest's ABI-v1 clock page, disables direct
  userspace counter access, and retains the architectural timer only as a
  clock-event device.
- `0003-arm64-harmony-lse-only.patch` emits LSE atomics directly and removes
  runtime LL/SC alternatives and reservation-monitor waits from the owned
  guest image.
- `0004-arm64-harmony-virtual-time-clockevent.patch` expresses clock events as
  absolute work-clock deadlines on the MMIO page and uses virtual-timer PPI 27
  for delivery and acknowledgement.

The arm64 build rejects surviving generic-counter reads, LL/SC instructions,
and direct counter-compare programming in published artifacts.
