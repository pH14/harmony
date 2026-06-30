# IMPLEMENTATION.md — patched-KVM determinism series

## Task 57 — productionize the determinism kernel: canonical 6.18.35 port of 0004 + 0005

**What this task added.** The series grew from 3 patches (0001-0003, the value
intercepts) to **5**: `0004` (in-kernel force-exit preemption →
`KVM_EXIT_PREEMPT` 42 / `KVM_ARM_PREEMPT_EXIT` 0xe4) and `0005` (MTF deterministic
single-step → `KVM_EXIT_DET_STEP` 43 / `KVM_ARM_MTF_STEP` 0xe5). 0004+0005 are the
Phase-1 + Phase-2 timing control the Postgres-on-k3s frontier (tasks 49/56,
`state_hash 226437a3…`, 0 skid) was proven on. They are now canonical patch files
in `patches/`, `git am`-clean onto the pinned `linux-6.18.35` tag.

**0005 was ported (not just copied) for 6.18.** The box-proven delta was authored
against 6.12.90. On 6.18 the four touch points (`kvm_host.h` bool, `kvm.h`
exit/ioctl numbers, the `x86.c` ioctl case, the `vmx.c` arm/exit) port one-to-one
**except** the arm point: both kernels arm MTF in `vmx_vcpu_pre_run` on the path
after the unhandleable-emulation guard, but 6.12 spells that guard
`vmx_emulation_required_with_pending_exception` and 6.18 renamed it
`vmx_unhandleable_emulation_required`. Same VM-entry hook, same mechanism. 0004
was already ported in the canonical tree before this task; this task verified it
and added its canonical patch file. No redesign (a non-goal).

**Cross-model review fix — MTF stale-arm (P1).** Codex + pi independently found that
the one-shot `mtf_step_armed` was cleared *only* in `handle_monitor_trap`. If the
single-stepped instruction itself exits to userspace (MMIO/PIO/MSR/HLT/
`KVM_EXIT_DETERMINISM`) rather than taking the MTF exit, the bool + the MTF
exec-control stayed set, so the next entry would deliver a **stale
`KVM_EXIT_DET_STEP`** (rejected as an unhandled exit → abort); pi added that the
leftover bit also makes snapshot/restore unsound (hidden live state not in the
serialized blob). Fix (in 0005, both the 6.18 canonical patch and the 6.12 proxy):
`vmx_handle_exit` disarms (`mtf_step_armed = false` + `exec_controls_clearbit(...,
CPU_BASED_MONITOR_TRAP_FLAG)`) on **any non-MTF exit to userspace** (`ret <= 0`).
The `ret <= 0` guard is load-bearing: in-kernel-handled exits return `> 0` and
re-enter with the MTF still armed, so single-stepping through a demand-paged EPT
fixup still lands its `DET_STEP` (clearing on *every* exit would drop the step →
overshoot). Because every way `single_step_once` returns to the VMM (the snapshot
boundary) now leaves the MTF clear, and `VcpuState` captures only `KVM_GET_*`
architectural state (never `mtf_step_armed`/exec-controls — they are not exposed by
any ioctl, and a restored vCPU starts MTF-clear), **snapshot/restore is sound**.
Userspace defense-in-depth (`consonance/vmm-backend/src/kvm.rs` `decode_exit`)
swallows any stray `KVM_EXIT_DET_STEP` as a transparent re-entry instead of
aborting. The pi "reason-43 classifier missing" note was a verified false positive
(`kvm.rs` already maps `KVM_EXIT_DET_STEP → StepStop::SingleStepTrap`).
Re-validated on the box: `live_m1_m2` 4/4 deterministic-twice + k3s **k1** (below),
0 skid, no stale DET_STEP. NB the userspace half of this fix + the DIAG/SPDX/ndjson
cleanup land on the task-56 bundle branch (where those files live), not here.

**Build (canonical, gate #2 — box `/root/kvm-spike/linux-6.18.35`, 2026-06-30).**
The 5-patch series `git am`-applies clean onto pristine `v6.18.35` **and
reproduces the built tree byte-for-byte** (`git diff` empty vs the build commit).
Modules build with no warnings: vermagic `6.18.35-g83a4bb005323`, `kvm.ko`
2471344 B, `kvm-intel.ko` 670816 B (+1296 B vs the 0001-0004 build = the MTF
arm/exit path). Per-file patch sha256 pinned in `guest/linux/versions.lock`
(`KVM_PATCH_000x_SHA256`). See `BUILD.md` Part 1.

**Live determinism re-validation (6.12.90 proxy, the documented Part-2 path).**
The canonical modules are build-verified but not loadable on the box (it runs
stock 6.12.90; see the deviation below — booting 6.18.35 stays rejected). The live
round-trip therefore ran on the 6.12.90 proxy carrying the **same** 0001-0005
source change (`run-patched-ht49.sh`: hot-swap patched KVM → gate pinned `taskset
-c 2` → revert to stock 1396736). Result, `cargo test -p vmm-core --test
live_m1_m2 -- --ignored`:

```
host_assert_report          ... ok   (all §1.1 host-baseline asserts PASS)
m1_hello_boots_and_prints   ... ok
m2_hello_deterministic_twice    ... ok   (bit-identical twice)
m2_compute_deterministic_twice  ... ok   (bit-identical twice)
test result: ok. 4 passed; 0 failed; finished in 46.50s   — 0 skid
```

Box reverted to stock and **verified `kvm 1396736` on a fresh ssh**. NB the
harness's own trap-revert can be cut short when the ssh session drops on `pkill`
during teardown — always re-verify stock on a fresh connection and force-revert
(`rmmod kvm_intel kvm; modprobe kvm kvm_intel`) if it shows the patched size
(1400832). `guest/payloads` must be rebuilt (`cargo build --release`, target
`x86_64-unknown-none`) after any box re-ship — a wiped payload fails the gate loudly.

**On "validate on the canonical kernel, not just the box-proxy" (task §2).** This
runs into the project's own reviewed decision (below, "Options considered" #2):
booting this *shared, console-less* box into a self-built kernel was **rejected as
disproportionately risky**, and the box has only ever run stock 6.12.90 (root on
`/dev/md2` software-RAID, NIC `e1000e`, `kernel/panic=0` — no auto-reboot net; a
failed boot is unrecoverable without Hetzner-Robot/console access the worker does
not have). The faithful-but-rejected path is documented; flipping it is a foreman
call with box-recovery readiness, not a worker default. Per `BUILD.md` line 44,
"to erase the proxy later: re-build via Part 1 and re-run the box gates on a host
whose running kernel **is** 6.18.35" — Part 1 then yields directly-loadable
modules. Everything needed for that (the 5-patch canonical series, build recipe,
verified modules) is delivered and ready.

---

# IMPLEMENTATION.md — patched-KVM RDTSC/RNG interception spike (tasks 16/55)

Originated as a throwaway feasibility spike (`tasks/16-patched-kvm-rdtsc-spike.md`);
the patch series it produced is the host-Linux KVM basis for the patched backend
(`../src/patched_kvm.rs`). The **retained** deliverables are **`patches/`** +
**`BUILD.md`** and the **GO** verdict recorded here; the Rust measurement harness,
guest stubs, and the raw results table were disposable and are **not retained** (the
load-bearing numbers are inlined below). This file records the decisions, deviations,
and what the integrator must know.

## What was proven

A minimal 3-commit KVM patch (+203/−2 lines) makes `RDTSC`/`RDTSCP`/`RDRAND`/
`RDSEED` VM-exit to userspace via a new `KVM_EXIT_DETERMINISM`, with a completion
path that writes the destination register(s) and advances RIP. It applies cleanly
to the pinned `linux-6.18.35` tag and builds; live (on a 6.12.90 proxy, see below)
it is 100/100 conforming for both TSC and RNG, bit-identical across runs, at
~3.4 µs (RDTSC) / ~3.8 µs (RNG) per intercept. **Verdict: GO.**

## The one big deviation: live experiments ran on a 6.12.90 proxy

The spec's environment section assumes the box runs the pinned `linux-6.18.35`.
It runs **`6.12.90+deb13.1-amd64`**. An out-of-tree module must match the running
kernel's vermagic to load, so the canonical pinned-tag build (vermagic `6.18.35-…`)
is **build-verified but not loadable** here.

**Options considered:**

1. **(chosen) Proxy on 6.12.90.** Author + build-verify the canonical series
   against `linux-6.18.35` (gate #2), and run the live round-trips against a
   6.12.90 build of the *same* change. Low risk (module swap only, reverted),
   real 100/100 numbers, proxy named in RESULTS. The VMX exec-control bits, exit
   reasons, and userspace-exit machinery are materially identical across 6.12→6.18,
   so the proxy is faithful (named as such here). Only code delta:
   `EXPORT_SYMBOL_FOR_KVM_INTERNAL` → `EXPORT_SYMBOL_GPL` (the namespaced macro is 6.16+).
2. **Reboot the box into a self-built 6.18.35 kernel** — fully faithful, but
   reboots a *shared* box into an unproven kernel with no remote console;
   **rejected** as disproportionately risky for a feasibility spike. The spec's
   hygiene section only contemplates swapping *modules*, not the kernel.
3. **Buildability-only, no live load** — weakest evidence (no 100/100). Rejected
   since the load path was open (no Secure Boot / lockdown) and the live numbers
   are the heart of the spike.

To erase the proxy later: re-build via `BUILD.md` Part 1 and re-run the `vmm-core`
box gates (`live_determinism`, `box_corpus`) on a host whose running kernel **is**
`linux-6.18.35` — Part 1 then produces directly-loadable modules (no proxy needed).

## Patch design decisions

- **Opt-in, default-off.** Gated on a per-VM cap (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`,
  settable only before vCPU creation). With it off, behavior is byte-for-byte
  stock (exp 2 confirms 0 exits), and RDRAND/RDSEED keep stock `#UD` semantics.
  This keeps the patch safe to carry without affecting other KVM users.
- **Modelled on `KVM_EXIT_X86_RDMSR`.** The exit fills `kvm_run`, sets
  `complete_userspace_io`, and the completion callback writes registers + advances
  RIP on re-entry — the well-trodden MSR-filter round-trip, so reviewers recognize
  the shape and RIP/skip handling is not reinvented.
- **TSC machinery untouched.** Only the guest-RDTSC read is taken over (primary
  control bit 12). Offset/scaling and the in-kernel TSC-deadline timer (host TSC)
  are not displaced; kvmclock is unused by the pinned guest.
- **RNG dest decode in kernel.** The VMX exit qualification carries no operand
  for RDRAND/RDSEED, so the patch decodes dest reg + width from the trapped
  instruction bytes (`[prefixes][REX] 0F C7 /6|/7`) via `kvm_read_guest_virt`.
  Minimal and correct for the standard encodings; a reviewer should sanity-check
  the prefix loop if exotic encodings ever matter (they don't for the pinned guest).

## Harness notes (historical — the measurement harness is not retained)

These record how the spike originally exercised the ABI, for context; the harness
itself (a box-only Rust crate + guest stubs) was not moved into the tree.

- `kvm-ioctls`/`kvm-bindings`/`libc` only (per the spec's extended whitelist).
  `unsafe` is confined to guest-RAM mmap (`guestmem.rs`) and the raw `kvm_run`
  mmap + `KVM_RUN` ioctl (`vm.rs`), each with a `// SAFETY:` comment — needed
  because `kvm-bindings` has no `KVM_EXIT_DETERMINISM`, so the harness overlays
  the determinism payload on the `kvm_run` page by documented offset.
- The guest is dropped into 64-bit long mode with a real GDT so each stub runs a
  **CPL3 phase** via `iretq` (the production guest runs these mostly in
  userspace); results are written to a guest-memory buffer the harness reads.
- Determinism (exp 7) compares all 18 GPR words + the result bytes; injection
  values come from a seeded splitmix64 (no `rand`, reproducible).

## Re-validation

The patches are re-validated by **`BUILD.md` Part 1** (`git am`-clean apply + build
against the pinned `linux-6.18.35` tag) and, with the modules loaded, the in-tree
`vmm-core` box gates (`live_determinism`, `box_corpus`) on the determinism box. This
directory is data + recipe only — it has no cargo crate, so `cargo build`/`nextest`
do not touch it.

## For the integrator / next task (`PatchedKvmBackend`)

- The ABI (`KVM_EXIT_DETERMINISM` = 41, cap 245, the `determinism` payload) is a
  spike proposal, not upstream; the real backend can rename/renumber freely. The
  load-bearing result is that the **mechanism works** and is cheap.
- Exit cost (~3.4 µs RDTSC) is the input to R-Backend's deferred in-kernel
  V-time fast path decision: fine for occasional reads, worth optimizing only for
  hot RDTSC loops. Re-measure on a release build / the real kernel before deciding.
- Non-goals untouched: AMD/SVM, multi-vCPU, nested control propagation, upstreaming.
