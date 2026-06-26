# Task 36 — guest kernel rebase: Kata-class container-host config + determinism overlay

> **START NOW · consonance workload stream, step 1 of 3 (36 → 37 → 38).** Runs **in parallel with
> task 39** (dissonance) — they share nothing and both execute on the box. This task does **not** run
> Postgres or Docker; it makes the guest *kernel* capable of running them deterministically, and proves
> the existing `GUEST_READY` boot still holds on the bigger kernel. Depends on **task 34 merged** (the
> deterministic-Linux-boot baseline). Branch from a main that has it; fast-forward local `main` before
> spawning.
>
> **Environment:** box-only for the boot/determinism gates (Linux bare-metal Intel, VMX, `/dev/kvm`,
> patched KVM per [[box-patched-kvm-ops]]). The config authoring + capability audit are Linux-only
> (kernel build); they do not run on macOS.

Read `tasks/00-CONVENTIONS.md`, `tasks/30-linux-boot.md`, `tasks/34-deterministic-linux-boot.md`,
`consonance/vmm-core/IMPLEMENTATION.md` (Task 30/33/34 sections), `guest/linux/config-fragment`,
`guest/linux/build-kernel.sh` + `lib-build.sh`, and `consonance/vmm-core/src/devices.rs` (the
`LegacyPlatform` / i8042 fast-fail) first.

## Why (the decision this task implements)

Today the guest kernel is `make ARCH=x86_64 tinyconfig` + the hand-curated `guest/linux/config-fragment`,
which boots a busybox `init.sh` to `GUEST_READY`. That base is **far** below what Postgres + Docker need
(cgroup-v2, overlayfs, namespaces, ext4, a RAM block device, epoll/futex/`/proc`/`/sys`, …), and growing
`tinyconfig` symbol-by-symbol toward "runs containers" is exactly the bespoke curation we are moving away
from. **Determinism does not live in the kernel config** — it is enforced from below (the patched KVM
backend determinizes TSC/RNG, V-time drives the timer, the VMM device models + cmdline handle the rest).
So the config only governs *capability* and *probe surface*, and we should adopt a known-good
container-host config rather than author one.

**Decision:** swap the kernel **base** from `tinyconfig` to a **Kata Containers guest-kernel config**
(vendored + pinned under `guest/linux/`), and keep `config-fragment` as the **determinism overlay**
merged on top (`merge_config.sh` + `olddefconfig`), unchanged in intent. Build it with the *existing*
`build-kernel.sh` pipeline (reproducible levers, pinned bytes, `MANIFEST.sha256`) — we use Kata's
*config*, not Kata's *binary* (we keep brd/loop, the golden flow, and a reproducible artifact; we never
boot Kata's agent/initramfs — `init.sh` stays our init).

## Phase 1 — rebase the base, preserve the determinism overlay

- Vendor + pin a Kata guest-kernel `.config` (or its fragment) for the kernel version in
  `guest/linux/versions.lock`; record provenance (Kata release + URL/commit) in `IMPLEMENTATION.md`.
- Change `build-kernel.sh`: base `= kata.config` instead of `tinyconfig`, then `merge_config.sh -m`
  the **determinism overlay** (`config-fragment`) on top, then `olddefconfig`. Keep the `assert_y`/
  `assert_off` guards — **every** determinism symbol now in the fragment (KASLR off, `SMP` off, `NUMA`
  off, `CPU_FREQ` off, `HZ_PERIODIC`/`HZ_100`, no `HIGH_RES_TIMERS`, `X86_PM_TIMER` off, `HW_RANDOM`
  off, `TRANSPARENT_HUGEPAGE`/`KSM` off, `SUSPEND`/`HIBERNATION` off, `MODULES` off, empty
  `LOCALVERSION`, gzip image) must survive the merge against the *richer* base. Assert each, loudly.
- Add the cmdline determinism params that are **not** build symbols to the VMM `DEFAULT_CMDLINE`
  (alongside the existing `tsc=reliable`/`lpj=`/`no_timer_check`/`random.trust_cpu=off`/`hpet=off`):
  **`nokaslr`** (Kata's base will have `RANDOMIZE_BASE` on — config-off it *and* belt-and-suspenders the
  cmdline) and **`nosmp` / `maxcpus=1`** (an `SMP=y` base running on one vCPU is fine and deterministic;
  we keep `SMP` off in the overlay where it survives, but the cmdline guarantees one CPU regardless).

## Phase 2 — close the new probe surface (the i8042 lesson, generalized)

A bigger config probes more absent devices, and under patched V-time **every jiffies-timeout probe spin
strands the boot for minutes** (exactly the i8042 `RCTR` spin task 34 fixed in `devices.rs`). Boot the
rebased kernel on the patched backend, find each *new* probe stall, and fix it with the proven pattern,
**lowest-cost first**:
1. **cmdline disable** of the offending probe (free, no code), else
2. **host-side device fail-fast** in `LegacyPlatform`/`devices.rs` (the i8042 `OBF`-set model) — a pure
   function of the port, no device state, nothing folded into `state_hash`.

Document each stall (the driver, the spinning loop, the fix, and why the fix is deterministic) in
`IMPLEMENTATION.md`, the way task 34 documented the i8042. Keep the busybox `init.sh` userspace
unchanged — this task changes only the kernel + cmdline + (if needed) device fail-fast.

## Phase 3 — container-capability audit (sets up 37/38, not exercised here)

Assert (config audit, `IMPLEMENTATION.md` table) that the rebased kernel has what 37/38 will need, so
the gap surfaces *now* rather than mid-Postgres-bring-up:
- **Storage:** `EXT4_FS`, and a RAM-backed block device — `BLK_DEV_LOOP` (loop-over-an-ext4-image-file,
  near-universal) and/or `BLK_DEV_RAM` (brd). Either gives "real ext4 + real `fsync` backed by RAM";
  prefer whichever Kata's base already provides built-in.
- **Containers:** cgroup-v2 (`CGROUPS`, `MEMCG`, `CPUSETS`, …), `OVERLAY_FS`, the namespace set
  (`NAMESPACES`/`PID_NS`/`NET_NS`/`USER_NS`), `BINFMT_ELF`, `EPOLL`/`FUTEX`/`SIGNALFD`/`EVENTFD`,
  `TMPFS`, `PROC_FS`/`SYSFS`. Networking (`NETFILTER`/bridge) is **not** required — 38 runs
  `docker run --network none`.

This is an audit only: presence of a symbol, not a running container. Absent must-haves become a noted
follow-on config delta, not a silent gap.

## Acceptance gates

1. **Deterministic-twice on the rebased kernel (box, patched, the milestone):** two same-seed boots of
   the Kata-config kernel + unchanged `init.sh` reach `GUEST_READY` and produce **bit-identical** serial
   + `state_hash`. Quote both (equal) digests in the PR. (This is task 34's gate, re-passed on the new
   kernel — the headline that "we can boot a container-class kernel deterministically.")
2. **Overlay survives:** `build-kernel.sh`'s `assert_y`/`assert_off` pass for every determinism symbol
   against the Kata base; the new `MANIFEST.sha256` is pinned and the build is reproducible (same bytes
   twice). Provenance of the Kata config recorded.
3. **Probe stalls closed:** the patched boot reaches `GUEST_READY` in a bounded V-time + wall budget;
   every new stall is documented with its deterministic fix (cmdline or device fail-fast).
4. **Capability audit** present and honest (the §Phase-3 table); any missing must-have flagged.
5. **No regression:** M1/M2/P6 + det-corpus goldens byte-identical (the kernel change is Linux-path;
   non-Linux `state_hash` unchanged); standard gates green incl. mutants/Miri/public-api where touched.
6. **Box hygiene:** every patched-module run reverts to stock KVM (`1396736`) after; verify `lsmod`.

## Non-goals

Running Postgres (37) or Docker (38); virtio; Docker networking (`--network none`); minimizing the
config back down (we are *accepting* a bigger probe surface in exchange for capability — minimization is
not load-bearing for determinism). No CPU/MSR contract or `state_hash` schema change. Don't re-architect
the loader/seam/serial — build on task 30/33/34.
