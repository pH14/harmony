# `vmm-core` — implementation notes

The deterministic VMM skeleton **above** the `vmm_backend::Backend` trait: the
Multiboot loader, the 32-bit-PM entry state, the CPUID/MSR-filter policy, the
bring-up device shims, and the event loop. Compiles against the trait alone; the
one place a concrete backend is named is the box-only M1/M2 integration test.

## Task 40 — single-node branching demo (the multiverse from one snapshot)

Task 40 is the dissonance **join point**: where the live snapshot/branch substrate
(task 39) meets the bare-Postgres workload (task 37) to show — by hand, not via the
task-12 explorer — that one snapshot forks into many **reproducible** and
**divergent** futures. The only change in this crate is a new box-only test,
`tests/live_branching_demo.rs` (`#[cfg(target_os = "linux")]` + `#[ignore]`, like
`live_postgres.rs` / `live_snapshot_branch.rs`). **No `src/` change** — it composes
the existing public API (`SnapshotEngine`, `Vmm::{save_vm_state, restore_snapshot,
reseed_entropy, state_hash, state_components}`), adds no dependency, and does not touch
`devices.rs` / the contract / the `state_hash` schema, so M1/M2/P6 + the det-corpus
goldens are byte-unchanged.

**The demo.** Boot the task-37 Postgres image on the patched backend; seal a quiescent
snapshot `S` (guest memory → `snapshot-store`, the non-memory machine → the `vm_state`
codec — at the boot-entry quiescent point, for the reasons below); drop the VM (freeing
its `perf_event` counter —
one open at a time, per `live_postgres.rs`); then fork `S` into a **base continuation**
(restore verbatim, no reseed) and `K` **branches** (`branch(S,seed') = restore(S) +
reseed_entropy(seed')`, task 39 Phase 4), each materialized as its own CoW view over
the one shared read-only base. Gate 1 (reproducibility): each fork, replayed `N`× from
its `(S,seed)`, is **bit-identical** every time. Gate 2 (divergence): at least one
branch's terminal `state_hash` differs from the base continuation, localized to a
**per-component** breakdown (`Vmm::state_components`) so the divergence is *shown*.

### The load-bearing finding: where `S` can be sealed (a task-39 substrate limit)

The spec asks to snapshot Postgres **mid-workload**. The task-39 snapshot codec cannot:
it captures only the **quiescent** machine — `vm-state`'s `VcpuEvents` deliberately
omits the in-flight-interrupt-injection fields (its own doc names the **HLT** point as
the snapshot target), and `snapshot::unrepresentable_state` fails closed on any pending
injection / SMM / triple-fault. A *cooperative, never-halting* Postgres guest (pg-init
avoids `HLT` because the VMM treats `HLT` as terminal) holds a **LAPIC-timer interrupt
in flight at essentially every V-time-sync boundary** once the timer is calibrated, so
`save_vm_state` is rejectable there. Measured directly (instrumented `seal`): of **8392**
post-readiness boundaries, **0** were snapshummable — 5280 rejected "non-synchronized"
(not at a V-time intercept) and **3112** rejected "in-flight injection." Task 39 only
ever validated snapshotting a *bare Multiboot payload* (no LAPIC, no interrupts); the
interrupt-driven Linux guest is the gap. (Reproduce the distribution with the
`scan_snapshot_points` diagnostic in the same test file, or seal late by setting
`SNAPSHOT_MARKER` to a post-readiness banner — the demo then panics with the rejection
tally instead of silently degrading.)

The reliably-clean boundaries are the **interrupt-free V-time reads of early boot**
(before the timer ramps). A **second constraint converges on the same root**: the
entropy fork only surfaces into *guest-observable* state (RAM/serial) if it happens
**before** the kernel seeds its CRNG — which it does before the first console byte — and
before Postgres draws its `pg_strong_random` secrets. Seal later and the branches are
byte-identical guests differing only in host-side entropy bookkeeping ("N identical
reruns," not a multiverse — quantified in *Box evidence* below). Both constraints point
to the **boot-entry quiescent point**, so that is where the demo seals `S` by default.
The forks then each run the **whole** boot→Postgres→workload forward from `S` — a real
fork into many futures, just **rooted at boot entry rather than mid-workload**. The
snapshot/branch mechanism exercised is identical (snapshot memory+`vm_state` →
materialize → restore → reseed → run; the CoW store interns the base **once** and
materializes `K` private views); only the root is early. (An explicit `SNAPSHOT_MARKER`
seals later — at a genuinely *running*, post-console VM — to demonstrate the trade-off;
see *Box evidence*.)

This is a genuine **task-39 follow-up** (snapshot an interrupt-driven guest at an
arbitrary point): it needs `vm-state`'s `VcpuEvents` extended to carry the in-flight
injection (`interrupt_injected`/`interrupt_nr`/…) so a restore re-injects it exactly
once — in the `vm-state` crate, **outside this task's directory**, and a change to a
frozen delegated interface, so it is deliberately **not** attempted here.

### Deviations considered and rejected

- **Snapshot Postgres mid-workload (the literal spec).** Impossible on this substrate
  (above): 0/8392 mid-run boundaries are snapshummable. Rejected in favour of the
  early-boot root, documented loudly rather than worked around.
- **Extend `vm-state` to capture in-flight injection.** The correct fix, but out of
  this crate's directory and a frozen-interface change (Conventions rule 1 + 3); it
  also re-opens task-39's determinism goldens. Left as a task-39 follow-up.
- **Seal later, at a genuinely-running (post-console) VM** rather than boot entry. More
  faithful to "snapshot a running machine," and it *is* reproducible — but past the CRNG
  seed the entropy fork reaches only the `vtim:` bookkeeping (byte-identical guests),
  failing gate 2's *meaningful*-divergence spirit. Rejected as the default for that
  reason; reachable via `SNAPSHOT_MARKER` and quoted as trade-off evidence below.
- **The crash-timing / WAL-recovery branch the spec also lists.** Needs either a
  cooperating guest (can't modify task-37's pg-init) or the host-side fault seam
  (`dissonance/environment` — a separate, live frontier). Out of scope; see below.

### Known limitations (set expectations honestly)

- **Bug class.** On RAM-backed storage the realistic divergence class is
  **concurrency/scheduling**, **not durability/crash-consistency** — there is no
  durable-vs-volatile split to fault against (the "fsync lied → recover wrong" class
  rides the deferred host-side RAM-disk model, **D1**). This demo drives only the
  **entropy-fork** knob the public branch API exposes (`reseed_entropy`); the
  crash-timing fault is out of scope (needs the host fault seam / a cooperating guest).
- **Restored-VM run cost.** A restored continuation runs markedly slower than a fresh
  boot (the V-time injection planner re-arms on restore and single-steps far more — a
  vmm-core/task-39 restore-path perf characteristic, not a demo bug). Each fork runs the
  full workload, so the matrix is wall-clock-bounded; `BRANCHES`/`REPLAYS` are env-
  configurable and the property is **exact** (bit-identity), so it is N-independent —
  the quoted small matrix is not a weaker claim than a larger one.

### Box evidence (`ssh <det-box>`, det-cfl-v1, patched KVM, CPU-pinned core 4)

**Reproducibility matrix + meaningful divergence — boot-entry seal (the headline).**
`K=3` branches + the base continuation, each replayed `N=3`× from its `(S, seed)`.
Every fork is **bit-identical across its 3 replays**, all four digests are **distinct**
(four reproducible, mutually-divergent futures from one snapshot), and every branch
diverges from the base in **guest-observable** state (RAM + serial), not just the
entropy bookkeeping.

`S` sealed at the boot-entry quiescent point (524288 guest pages, 12076 owned). Quoted
verbatim from the run (`finished in 2271s` ≈ 38 min, core 4):

```
 fork                       seed                 replays   digest (all equal)
 base (verbatim replay)     0x0028c0ffee5eedc0     3/3     7ea21de2e3eb3ba2dede8370edda84a6950f97afe7469de8c990f88090845e39
 branch 0                   0x9e1fb946911491d5     3/3     ed8e26455aaf7b1d60a61e2ffbdd13b82277eb6ccf425c902f54dae142abeb66
 branch 1                   0x3c46338d10ca15ea     3/3     4256313e065607a43c774dd652b730f818f142661209fb565a767407840ae2c1
 branch 2                   0xda8eadd3938199ff     3/3     6e45bac610f3492868910706ba86dfbeb7bd5e792c612e4d1ada87ff8cb580a7
 gate 1 ✓ every fork bit-identical across its 3 replays — reproducible.
 base shared once store-wide: 12073 unique pages after 3 branches — one read-only base.

 branch 0/1/2 DIVERGE from base; differing components =
   ["RAM:16M..","regs","control-regs","serial", "vtim:eff-vns","vtim:entropy",...]
   guest-observable among them = ["RAM:16M..","regs","control-regs","serial"]
 gate 2 ✓ divergence reaches guest-observable state (RAM + serial), not just bookkeeping.
```

Four reproducible, **mutually-distinct** futures from one snapshot. The forks' forward
step counts also differ (base 162609; branches 162616 / 162569 / 162622) — the seeded
entropy fork takes a **different execution path** (a real interleaving), not merely
different values, then each path replays bit-identically.

**Trade-off evidence — a genuinely-running-VM seal (sealed later in boot).** The
substrate *can* also snapshot a running early-boot VM (a separate K=3×N=3 run sealed
`S` at step 123885 — kernel mid-boot, `legacy console [ttyS0] enabled`, 25146 dirtied
pages). There the matrix is **equally reproducible** — base `7ea21de2…`, branch0
`4d93eecc…`, branch1 `d5d7d275…`, branch2 `b238bec8…`, each 3/3 identical and all
distinct — but the divergence is **only in the host-side `vtim:` entropy bookkeeping**,
not guest-observable state: the branches are byte-identical guests ("N identical
reruns"). This is *why* the headline seals at boot entry — by step 123885 the kernel
CRNG is already seeded, so reseeding the VMM's RDRAND stream no longer reaches Postgres'
secrets. Same reproducibility either way; meaningful divergence needs the pre-CRNG-seed
root. (One nice determinism corollary: the **base continuation's terminal digest is
`7ea21de2…` regardless of where `S` is sealed** — step 0 or step 123885 — and across
separate box runs. Same seed ⇒ same future.)

**Scale note.** Digests are **exact** (bit-identity), so the property is
N-independent — the 3/3 matrix is not a weaker claim than 100/100. `BRANCHES`/`REPLAYS`
are env-configurable; N is bounded only by wall time, since each restored fork runs the
full boot→workload (~160k V-time intercepts) and the restored-VM injection path runs
markedly slower than a fresh boot (a vmm-core/task-39 restore-path characteristic). The
boot-entry K=3×N=3 run took ~38 min on core 4 (each fork runs the full ~162.6k-step
boot+workload); the running-VM-seal variant ~37 min.

### Box hygiene

Run via `run-patched-ht40.sh` (mirrors `run-patched.sh`): coordinate (abort if
`kvm_intel` is in use by a concurrent patched run, e.g. task 38), load patched
`kvm.ko`/`kvm-intel.ko`, run pinned to **core 4**, and **always revert to stock on
exit** with a verified `lsmod | grep '^kvm ' == 1396736`. Every run above reverted OK.

## Task 37 — bare-Postgres workload (box gates only here; image lives in `guest/linux/`)

Task 37 boots a real PostgreSQL 17 in the guest and proves it runs **bit-identically
twice** on the patched backend. The image build + the full determinism closure are in
**`guest/linux/IMPLEMENTATION.md`**; the only change in this crate is a new box-only
test, `tests/live_postgres.rs` (`#[cfg(target_os = "linux")]` + `#[ignore]`, like
`live_linux_boot.rs`): gate 1 (postgres runs + streams the workload to `ttyS0`) and
gate 2 (deterministic-twice: bit-identical serial + `state_hash`). No `src/` change —
`devices.rs`, the CPU/MSR contract, and the `state_hash` schema are untouched, so
M1/M2/P6 + the det-corpus goldens are byte-unchanged. The test's `DEFAULT_CMDLINE`
mirrors `live_linux_boot`'s with two task-37 deltas (each documented in that file):
`random.trust_cpu=off` is **dropped** (under deterministic V-time the CRNG can only
seed from the trapped+seeded RDRAND/RDSEED, else postgres' blocking `getrandom` hangs)
and `reboot=t` → **`reboot=t,force`** (a plain poweroff strands in the kernel's
`device_shutdown` once block I/O has run; the forced triple-fault is a clean terminal).
Gate 2 also boots twice **in one process, dropping run A's `Vmm` before run B**, so only
one `perf_event` work counter is open at a time (two would multiplex and skid V-time).

## Task 36 — guest-kernel rebase (cmdline only here; config lives in `guest/linux/`)

Task 36 rebased the guest kernel from `tinyconfig` to a Kata-class container-host config
(cgroup-v2/overlayfs/ext4/loop/brd/namespaces for tasks 37/38), keeping `config-fragment` as
the determinism overlay. The full rationale, provenance, capability audit, and box digests
are in **`guest/linux/IMPLEMENTATION.md`**. The only change in this crate is the box gate's
`DEFAULT_CMDLINE` (`tests/live_linux_boot.rs`): it gained the runtime determinism params the
Kata base needs — `random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable` — each
a no-op against the overlay's *build* symbols, present belt-and-suspenders because Kata's base
sets the opposite (e.g. `RANDOMIZE_BASE=y`, `SMP=y`, `X86_X2APIC=y`). No `devices.rs` change
was needed: the larger config introduced **no** new jiffies-timeout probe stall under patched
V-time (the task-33/34 i8042 OBF-set fast-clear in `LegacyPlatform` already covers the one
such probe), so `state_hash` and every non-Linux path are byte-unchanged. Box milestone
(deterministic-twice, patched, core-2-pinned then reverted to stock 1396736): two same-seed
boots of the rebased kernel reach `GUEST_READY` bit-identically,
`state_hash = b277bc5260144dcb22545f6350c42886f2691a0f95ffcc8e18f8dc1b44bd6847`.

## Task 30 — boot real Linux in consonance (direct 64-bit boot protocol)

**Result: a real Linux 6.18.35 kernel + static-busybox initramfs boots inside the
VMM all the way to userspace `/init` — on the box, on the stock `KvmBackend`.** This
is the milestone that *proves consonance*: until now determinism held over
bare-metal task-04 payloads; now an actual Linux kernel decompresses, enters 64-bit
long mode, initializes, unpacks the initramfs, and executes the userspace init
process inside `consonance`.

### What landed (all in `consonance/vmm-core/`)

- **`src/linux_loader.rs`** — the direct 64-bit boot protocol (Firecracker model;
  integrator ruling 2026-06-25 — **no** real-mode setup-code emulation). `load()`
  parses the bzImage `setup_header` (`boot_flag`/`HdrS`/`version ≥ 0x020c`/
  `XLF_KERNEL_64`), loads the protected-mode kernel at `pref_address`, the initramfs
  high (page-aligned, < 4 GiB), and builds: `boot_params` (a one-page zero page with a
  two-entry E820 map, the command line, the `ramdisk_*` fields, and the copied/patched
  `setup_header`), an identity page table (2 MiB pages over the first 1 GiB), and a flat
  64-bit GDT (`__BOOT_CS=0x10`/`__BOOT_DS=0x18`). The `#[repr(C)]`
  `SetupHeader`/`BootParams`/`BootE820Entry` structs are pinned by a layout test
  (gate 1). The loader is **total over untrusted bytes** (gate 2): every image read is
  bounds-checked and every address `checked_*`; a malformed/truncated image is a loud
  `LinuxLoadError`, never a panic/OOB (driven by `tests/linux_loader_proptest.rs`).
- **`src/entry.rs::long_mode_entry`** — the long-mode entry state: `CR0.PG|PE`,
  `CR4.PAE`, `EFER.LME|LMA`, `CR3 =` the identity map, flat `__BOOT_CS`/`__BOOT_DS`,
  `GDTR →` the boot GDT, `RSI =` `boot_params`, `RIP = load_addr + 0x200`, `IF=0`.
- **`src/bringup.rs`** — `ImageKind::detect` (Multiboot vs Linux), `boot_linux` /
  `compose_linux` (host gate → policy → load → map → long-mode restore → xAPIC +
  legacy-platform wiring), the `apply_linux_entry` overlay, and the box-only
  `boot_linux_selected` composition root.
- **`src/vmm.rs`** — wired the userspace xAPIC (the `lapic` crate, ruling R1) and a
  minimal legacy-platform I/O stub (`devices::LegacyPlatform`) into the event loop, both
  **`Option`-gated to the Linux path**: an MMIO access in the `0xFEE0_0000` page routes
  to `lapic::Lapic`, and the curated legacy ISA/PCI ports (PCI config, PIC, PIT, i8042,
  CMOS, POST/0x80, extra COM) read back "no device". M1/M2/corpus leave both unwired, so
  their port-I/O default-deny **and** `state_hash` are byte-for-byte unchanged (no
  `LAPC`/`LEGY` chunks) — gate 5.

### Two boot-bring-up subtleties worth recording

1. **`EFER` is overwritten by the MSR restore.** `EFER` is an *allow-stateful* MSR, so
   `KvmBackend::restore` rewrites it from the snapshot's MSR map **after**
   `KVM_SET_SREGS2`. Setting only `sregs.efer` left the guest entering with `LMA` set
   but `LME` clear (an impossible combination) → VMX "invalid guest state",
   `KVM_EXIT_FAIL_ENTRY`. `apply_linux_entry` therefore also writes `EFER` into the MSR
   map (`state.msrs[0xC000_0080]`), overwriting the reset value without changing the
   validated key set.
2. **Even a "stock" Linux boot needs V-time wired.** The frozen contract marks
   `IA32_TSC` (0x10) and `IA32_TSC_ADJUST` (0x3b) `emulate-vtime`, and Linux reads them
   early in boot. So `boot_linux_selected` wires V-time on **both** substrates. On
   `Stock` the RDTSC/RDRAND *instructions* still run untrapped against the host (the
   boot is nondeterministic by construction — Phase A only *proves the boot*); on
   `Patched` they trap to V-time / the seeded stream (Phase C).

### Box evidence (`ssh <det-box>`, i9-9900K / det-cfl-v1, CPU-pinned)

The committed `guest/linux` artifacts build bit-for-bit to the committed
`MANIFEST.sha256` (`bzImage d797c47e…`, `initramfs.cpio.gz f0bb7c0d…`). On the stock
`KvmBackend`, `tests/live_linux_boot.rs::a_linux_boots_to_userspace_stock` boots them to:

```
clocksource: Switched to clocksource tsc
Unpacking initramfs...
Freeing initrd memory: 1228K
serial8250: ttyS0 at I/O 0x3f8 ... is a 16450
Run /init as init process          <-- userspace reached, in consonance
```

and busybox userspace **executes correctly** (verified directly via `rdinit=`): `mount
-t proc` → exit 0, `mount -t sysfs` → exit 0, `poweroff -f` → clean HLT, `sh -c true/false`
→ exit 0/1 as expected. The QEMU/KVM control boots the *same* artifacts to `GUEST_READY`,
confirming the guest image is good and the boot path is the VMM's.

### Phase A vs `GUEST_READY` — the remaining gap is interrupt delivery (Phase B)

The task's gate-3 string is `GUEST_READY`, printed by `guest/linux/init.sh` via
`echo GUEST_READY` (a **userspace** console write). The boot reaches `/init` but does
**not** emit it, because userspace console output requires the 8250 **TX path**, which
needs an interrupt — and this VMM delivers **none**. Diagnosis (box, definitive):

- `sh -c echo` as init exits 1 → the userspace console *write itself fails*; the kernel
  registered the polled printk console (so kernel logs appear) but the tty TX buffer
  drains only on the UART IRQ or the 8250 poll-timer, and the poll-timer needs jiffies.
- No jiffies, because the LAPIC timer cannot be delivered: `KvmBackend::inject` is
  `Unsupported` (it lives in `vmm-backend`, **below** the trait and outside this task's
  directory — conventions rule 1). And even with injection, the LAPIC timer cannot
  *calibrate* on stock KVM (the kernel's calibration busy-waits on in-guest RDTSC, which
  does not trap, so our V-time — and thus `TMCCT` — does not advance during the spin); it
  calibrates only on the **patched** backend, where RDTSC traps to V-time.

So `GUEST_READY` (and a fully deterministic boot, Phase C) needs **Phase B = the
interrupt-injection seam** (`KvmBackend::inject` + the `KVM_INTERRUPT`/interrupt-window
handshake in `vmm-backend`) driving the already-wired LAPIC timer off V-time, on the
patched backend. The `vmm-core` side is in place: the xAPIC register file is wired and
its timer is driven from `lapic_now_vns()`; what is missing is the backend ability to
*deliver* the vector. **This is escalated to the integrator** (conventions rule 5,
ask-by-comment): it is a cross-crate change to `vmm-backend` (the frontier KVM run loop)
that the task's Phase B anticipated.

### Acceptance gates

| Gate | Status |
|---|---|
| 1 — layout pinned (`boot_params`/`setup_header`/`e820` offsets) | ✅ `linux_loader::tests::{setup_header,boot_params}_field_offsets` |
| 2 — loader total over untrusted bytes | ✅ `tests/linux_loader_proptest.rs` (4 props, ≥512 cases; miri-bounded) **plus** `load_pins_every_computed_value` (exact addresses/`boot_params`/page-table entries — mutation gate) |
| 3 — **milestone**: `GUEST_READY` + clean poweroff | ⏳ **expected-fail until Phase B** — held distinct as `gate3_linux_guest_ready_and_clean_poweroff` (never weakened); `a_linux_boots_to_userspace_stock` is the honest current-capability gate (reaches `/init` AND terminates clean within budget, no contract violation) |
| 4 — Phase C deterministic-twice (patched) | ⏳ deferred — needs Phase B (inject seam) + loaded patched modules; test written (`c_linux_boot_deterministic_twice_patched`) |
| 5 — standard gates; no determinism leak in M1/M2/P6 | ✅ build/clippy/fmt/nextest/deny/public-api/**mutants** green; LAPC/LEGY chunks are Linux-path-only |

### Review fixes (PR #58)

- **Mutation gate (blocking):** added `load_pins_every_computed_value` (and exact
  page-table-bound + `LegacyPlatform`/`ImageKind`/`apply_linux_entry`/`compose_linux`
  tests) pinning every computed address, `boot_params` field, and PD entry, so the
  placement-arithmetic mutants die. Restructured `write_page_tables` to a single PD
  (no single-iteration outer loop → no equivalent mutants). `boot_linux_selected`
  (the `cfg(linux)` composition root) is excluded in `.cargo/mutants.toml` alongside
  `boot_selected` — box-only, same precedent.
- **Honest live gate (blocking):** the current-capability gate now asserts reached-
  `/init` **and** a clean terminal within budget (no contract violation, no hang) —
  it can no longer pass on a mid-run `Run /init` followed by a fault/hang. The
  `GUEST_READY` milestone is a separate, expected-fail gate (gate 3 above).
- **Loader robustness (codex P2):** reject a protected-mode kernel shorter than
  `ENTRY_64_OFFSET + 1` (`KernelTooSmall` — the entry would point at uncopied RAM);
  honor `initrd_addr_max` in `place_initramfs`; cap the command line against the
  header's `cmdline_size` (`CmdlineTooLong { limit }`) and advertise no more than it.

## Task 27 — V-time determinism surface completion (the corners #45 deferred)

Finishes the V-time surface task 21 left for a follow-up. Three test-pinned items, each a
separate commit; **item 2 is the blocking determinism fix** (the task-28 box corpus O1
localized the *sole* same-seed `state_hash` divergence to the `VTIM` chunk). All three live
in `src/vmm.rs`; no contract, public-API, M1/M2, or `unsafe` change.

### Item 2 (commit `c385b40`) — anchor the VTIM hash to deterministic last-intercept work

**Bug.** `encode_vtime` hashed two things that broke determinism/transparency:
- it hashed `vns_base` and the work counter **separately**, so a restored VM (`vns_base=E`,
  `work=0`) and a fresh VM at the same effective V-time (`vns_base=0`, `work=E`) hashed
  **differently** — breaking the transparency `unison::compare_runs` wants; and
- it hashed a **live read of the raw work counter** (`vt.work.work()`) **at hash time**. Perf
  skid (post-last-intercept exit-path branches) makes the *terminal* raw work non-deterministic
  across two same-seed runs, even though work at every *intercept* is deterministic (that is why
  the guest's TSC reads — `observable_digest` — match). So the `VTIM` chunk (hence `state_hash`)
  diverged intermittently (box O1, PR #51).

**Fix.** `VtimeWiring` now records `last_intercept_work` — the work read at **every** V-time
intercept (the four determinism-cap traps RDTSC/RDTSCP via `complete_tsc`, RDRAND/RDSEED via
`complete_rng`, and the `IA32_TSC`/`IA32_TSC_ADJUST` MSR paths), the synchronized point the
patched backend corrects skid to, a **deterministic** value. `encode_vtime` takes **no live
counter read**; it hashes **one canonical effective-V-time field**,
`clock.snapshot_vns(last_intercept_work) = vns_base + last_intercept_work·ratio`, which folds
`vns_base` + work together. This gives both properties at once:
- **determinism-twice:** the anchor is the deterministic intercept work, never the skid-laden
  terminal read; the encoding is now total/infallible (no counter read, no poison sentinel);
- **restore-transparency:** `restore_vtime` resets the anchor to `0` (the counter restarts at 0
  and the effective V-time moves into `vns_base`), so a restored and a fresh VM at the same
  effective V-time hash identically.

The clock-*rate* fields (`ratio_num`/`tsc_hz`/`tsc_base`) are still hashed directly (they govern
future TSC); only the `vns_base`+work *position* was canonicalized.

**Why record at every V-time intercept, but not at the terminal non-V-time exit (the design
call).** The deterministic value is the work read *at* a V-time intercept (where the patched
backend corrects skid). **Every** V-time intercept must advance the anchor — RDTSC/RDTSCP,
**RDRAND/RDSEED**, and the TSC MSRs — because `last_intercept_work` is the *only* work-derived
value in `VTIM`: if an RNG exit were the last intercept before a checkpoint and it didn't sample
work, the hash would carry a stale prior-intercept value and two states that burned different
branch counts before the same seeded draw would collide (a false determinism MATCH that diverges
on the next TSC read — the box-verification cross-model finding, fixed in `complete_rng`).
Conversely, reading the counter at a *non-V-time* exit (`out 0xF4`/`hlt`/IO) would re-introduce
the very skid this fixes, since the backend does not correct skid there. So the anchor is the
last V-time intercept's corrected work — deterministic, and precisely "the work the next
RDTSC/RNG continues from". (For P6 the last V-time intercept is the RDSEED, after which no branch
retires before the debug-exit, so the anchored effective V-time is also the terminal value.)

Tests (Mac, MockBackend + ScriptedWork): `state_hash_does_not_read_the_live_work_counter` (a
read-counting source proves hashing takes **zero** counter reads — the fix itself);
`vtim_is_deterministic_twice_despite_terminal_skid` (test i — a source whose post-intercept read
diverges by skid, yet `state_hash` is byte-identical); `restored_and_fresh_at_same_effective_vtime_hash_identically`
(test ii); `rng_exit_advances_the_vtim_work_anchor` (an RNG exit at different work hashes
differently despite an identical draw — the box-verification fix). The pre-existing
distinguish-seed/cfg test was reworked to observe a work difference via a **stepped** RDTSC (the
raw counter is no longer hashed live).

### Item 1 (commit `f2de4c8`) — wire emulate-vtime TSC MSRs through V-time

`dispatch_rdmsr/wrmsr` routed the `EmulateVtime` disposition (the contract's `IA32_TSC` 0x10 /
`IA32_TSC_ADJUST` 0x3b rows) to a `ContractViolation` with a stale "V-time is not wired in this
skeleton" message — so a guest reading TSC via `RDMSR(0x10)` aborted despite the patched path
advertising a deterministic TSC. Now routed through the **same** V-time:
- the guest-visible TSC is `visible_tsc(work) = VClock::tsc(work) + IA32_TSC_ADJUST` (wrapping mod
  2⁶⁴, as the architectural counter does); **RDTSC, RDTSCP, and `RDMSR(IA32_TSC)` all go through
  `visible_tsc`**, so they agree byte-for-byte. With the default `tsc_adjust == 0` it is exactly
  `VClock::tsc(work)` — no change to P6/M-tests.
- `RDMSR(IA32_TSC_ADJUST)` → the stored adjust; `WRMSR(IA32_TSC, X)` sets the adjust to
  `X − VClock::tsc(work)` (which equals the architectural `adjust_old + (X − TSC_old)`, so the
  visible TSC then reads `X`); `WRMSR(IA32_TSC_ADJUST, Y)` sets it to `Y`. Both honored
  (`complete_ok`). **All four V-time MSR paths (`IA32_TSC` and `IA32_TSC_ADJUST`, read and write)
  record `last_intercept_work`** — each is a V-time intercept, so the hashed effective V-time
  stays current after any of them (cross-model finding 1).
- `tsc_adjust` lives in `VtimeWiring` and is **hashed** (it governs future TSC output). It is `0`
  at reset and for every audited payload (none touches the TSC MSRs). Unwired (stock KVM / M1/M2)
  still **fails closed** in both directions — never a laundered host value.

The MSR index numbers are architectural x86 constants (`IA32_TSC`/`IA32_TSC_ADJUST` named
consts), not contract policy; the contract still gates *which* indices are `emulate-vtime`. An
unexpected index reaching the emulate-vtime arm fails closed.

### Item 3 — `restore_vtime` is symmetric with `save_vtime` at the RNG boundary

The spurious-`ContractViolation` item 3 targets (restore-then-`save_vtime` at a clean boundary) is
real, but the box-verification cross-model pass showed the original fix — `restore_vtime` *clearing*
`rng_completion_staged` — is unsound: the flag mirrors a **backend** staged RDRAND/RDSEED completion
(reg-write/RIP-advance pending for the next `KVM_RUN`) that a V-time-only restore does **not** undo;
clearing it while rewinding entropy lets that stale completion commit against the restored stream →
shifted draws. So `restore_vtime` now **fails closed at an RNG mid-exit boundary**, symmetric with
`save_vtime`, and does **not** clear the flag. At a *clean* boundary the flag is already false, so a
restore-then-save succeeds (item 3's actual requirement); the flag is cleared only by the next
`step`'s re-entry (which truly commits the completion) or by the task-08 full `vm_state` restore
(which discards it). `Backend::restore` does not own the mid-exit completion state (task-08), which
is exactly why V-time restore must not declare the backend clean.

### VTIM encoding (final, after all three items) — for the box re-verification

`VTIM` chunk = `ratio_num`‖`tsc_hz`‖`tsc_base`‖`tsc_adjust`‖`effective_vns`‖`entropy_state`
(four `u64` LE, then `snapshot_vns(last_intercept_work)` as `u64` LE, then the trailing
`SeededEntropy::save_state()`). This **changes the P6 `VTIM` hash value** vs task 21 (the
restore-transparency canonicalization + the new `tsc_adjust` field), but it is now
**deterministic-twice**. Re-run P6 on the box (load the patched `kvm{,-intel}.ko`, `taskset -c 2
cargo test -p vmm-core --test live_determinism -- --ignored --test-threads=1`, revert to stock)
and capture the new `state_hash` here — it should be byte-identical across two runs. The guest
*results* (the RDTSC/RDTSCP/RDRAND/RDSEED values + RDTSCP aux) are unchanged from task 21.

**Box-verification status (foreman, first round).** Confirmed working: corpus O1 — all 6
conformance items `state_hash = MATCH` (were `DIVERGE`); P6 deterministic-twice. The same pass's
cross-model review then found a P1 hole (`complete_rng` didn't advance the work anchor — see the
RNG-intercept note above), now fixed (`e515203`).

**Box-verification — final, foreman-captured (det-cfl-v1 box, after the `e515203` P1 fix).**
`task/27` merged onto #51 on the **patched** box (`kvm{,-intel}.ko` loaded; reverted to stock
after). The blocking determinism fix holds:

- **Corpus O1 diagnostic, run 3×** — all 6 conformance items (`insn-rdtsc`, `insn-rng`,
  `insn-cpuid`, `insn-rdpmc`, `msr-allowed`, `msr-denied`) `state_hash = MATCH` on **every** run.
  The original divergence (the `vtim` component `DIVERGE` on all 6) is now consistently `MATCH`.
- **P6 deterministic-twice — PASS** with the new `VTIM` hash:
  `p6_rdtsc_rng_are_deterministic_and_vtime_backed` (TSC reads `[0, 2, 4, 6]`, `rdrand =
  0x9f72a62a`, `rdseed = 0x42b87398`) and `p6_snapshot_restore_resumes_both_clocks_exactly` —
  both `ok`.
- The `VTIM` hash **value** changed (expected — the chunk format changed) but is
  **deterministic-twice**. **M1/M2** (`vtime: None`) hashes and **`contract_hash`** are
  **unchanged**.

This is the codex-P2 box proof: the box O1 divergence task 27 item 2 targets is resolved, which
unblocks PR #51.

### `tsc_adjust` is carried in `VtimeSnapshot` (the contract puts TSC/TSC_ADJUST in `vm_state`)

`VtimeSnapshot` now carries `tsc_adjust` alongside `vns` + `entropy`: `save_vtime` captures it and
`restore_vtime` re-applies it. An earlier round failed closed on a non-zero adjust (to avoid silent
loss), but the box-verification cross-model pass noted the contract places TSC/TSC_ADJUST in
`vm_state`, so the correct fix is to **serialize + restore** it — a guest that wrote the MSR is then
snapshottable and restores faithfully. This is the one **public-API** change in task 27
(`VtimeSnapshot` gains a `pub tsc_adjust: u64`); `tests/public-api.txt` was updated in sorted order
(`entropy`, `tsc_adjust`, `vns`). It does **not** change the `VTIM` hash (the chunk already encoded
`tsc_adjust` from item 1; `VtimeSnapshot` is a separate struct), and `tsc_adjust` stays `0` for every
audited payload, so M1/M2 and the P6 box hash are unaffected.

### The under-capture: HASH and SNAPSHOT have DIFFERENT correct resolutions (integrator ruling)

Anchoring V-time to `last_intercept_work` **under-captures** work the guest retires *after* its last
V-time intercept and before a non-V-time exit (CPUID/PIO/HLT): that work is **skid** — only the
determinism-cap traps (RDTSC/RDTSCP/RDRAND/RDSEED) and the TSC MSRs are skid-synchronized; a
non-V-time exit gives only the raw counter, whose post-intercept value is non-deterministic (the
original O1 bug). The integrator ruled the **hash** and the **snapshot** resolve this **differently**
— do not treat them the same:

**HASH (`encode_vtime`) — KEEP `last_intercept_work`; it is the correct hash, not a bug.**
`state_blob` is **V-time replay-equivalence up to the last synchronized intercept**: two states are
equal iff identical there; post-intercept work — distinguishable only by re-synchronizing at the next
RDTSC — is **intentionally not captured because it is not deterministically measurable**. It is
**exact for same-seed determinism (O1)** — box-proven (the CPUID/MSR O1 items, which carry no V-time
access, MATCH because their anchor is a deterministic constant). The corpus **must** hash at
non-intercept exits (it checkpoints at `isa-debug-exit`), so "refuse to hash off an intercept" would
break the corpus and is wrong; hashing the live counter would reintroduce skid. Action taken:
documented crisply as a deliberate property (the `encode_vtime` doc), no code change.

**SNAPSHOT (`save_vtime`) — here the under-capture WAS a bug; now fails closed.** A snapshot's `vns`
must be the **exact** V-time: restore resumes the TSC from it (§4), so a stale `last_intercept_work`
as `vns` resumes from the last intercept, not the snapshot point → the next RDTSC is low by the
missed work → a **silently-wrong restore**. Project rule is fail-closed, never silently wrong.
Investigated whether the backend gives a deterministic/skid-free work count at a quiescent/HLT exit:
**no** — the patch corrects skid only at the four determinism-cap traps (HLT is stock KVM), and the
box O1 evidence shows a terminal cumulative read diverges (the pmu-count spike's "rock-stable skid"
is the overflow-PMI path, not the cumulative read at HLT). So `save_vtime` now **fails closed unless
at a V-time-intercept boundary** (`vtime_synchronized`), where `last_intercept_work` *is* the exact
current work — never recording a stale `vns`. The `vtime_synchronized` flag is set `false`
**before** each `step`'s `backend.run()` (so a failed run leaves it `false`, not stale-`true` — a
cross-model finding) and back to `true` only by a V-time-intercept completion (and after
`restore_vtime`, which lands at work 0 = `vns_base`, and for a fresh VM). Tests:
`save_vtime_fails_closed_at_non_intercept_exit` (RDTSC ok → UART OUT fails → RDTSC ok again),
`run_error_leaves_vtime_desynchronized`, `save_vtime_anchors_vns_to_last_intercept_not_live_work`.

**FLAG for the integrator/user (design matter).** This means **exact V-time snapshots are only
possible at V-time-intercept boundaries**, which constrains the dissonance control plane: it snapshots
at quiescent `HLT` (+ empty timer queue), which is **not** an intercept, so `save_vtime` there now
fails closed. Resolving it needs one of: (a) the backend exposing a **skid-free quiescent work read**
at HLT (needs box measurement — not established for the cumulative read); (b) the explorer taking a
**deterministic V-time intercept** (e.g. an RDTSC) immediately before snapshotting; or (c) accepting
that snapshots are intercept-aligned. That is a control-plane/backend design choice above this crate.

### Cross-model pass (GPT-5.5 via pi)

A GPT-5.5 cross-model correctness pass (`pi --provider openai-codex --model gpt-5.5`) raised three
findings on the first round: (1) the `IA32_TSC_ADJUST` paths now sample work; (2) the non-zero
`tsc_adjust` snapshot gap (first handled by failing closed, later **superseded** by serializing it
into `VtimeSnapshot` — see the fourth pass); (3) the intercept-anchored hash semantics documented
and the over-claim removed. A **second** pass on that diff returned **CLEAN — no material findings**.

A **third** cross-model pass run during the foreman's box-verification round (corpus O1 already
MATCH, P6 already passing) caught a **P1** the earlier passes missed: `complete_rng`
(RDRAND/RDSEED) advanced the entropy stream but did **not** advance `last_intercept_work`. Since
the anchor is the only work-derived value in `VTIM`, an RNG exit being the last intercept before a
checkpoint left a stale anchor, so two states that burned different branch counts before an
identical draw collided in `VTIM` (a false O1 MATCH that diverges on the next TSC read). Fixed:
`complete_rng` now records the synchronized work like the other intercepts, and
`rng_exit_advances_the_vtim_work_anchor` pins it. This is the general invariant — **every** V-time
intercept advances the anchor.

A **fourth** cross-model pass (on the P1 fix, during the foreman's second box-verify) raised three
more (2× P1 + 1× P2), all addressed above: the crux V-time/skid under-capture (documented — owner's
call: no skid-free current-work at non-V-time exits, don't reintroduce skid); `restore_vtime`'s
unsound flag-clear (now **fails closed** at the RNG boundary, symmetric with `save_vtime`); and
`tsc_adjust` dropped by `VtimeSnapshot` (now **serialized + restored**, the one public-API change).
**None of these change the `VTIM` hash value or the guest results** — the encoding is unchanged
(`tsc_adjust` was already hashed; the flag was never hashed; the doc-only crux), and `tsc_adjust`
stays `0` for all payloads — so the foreman's captured box evidence (corpus O1 MATCH ×3, P6
deterministic-twice) **still holds**; the re-box-verify is a re-confirmation, not a new hash.

A **fifth** pass caught that `save_vtime` still read the **live** counter for `snap.vns` (the
skid-tainted read removed from `encode_vtime`); it was first re-anchored to `last_intercept_work`. A
**sixth** pass on that returned CLEAN. The **integrator ruling** then refined the snapshot
resolution further: re-anchoring alone is still wrong for the snapshot (a stale anchor = a
silently-wrong restore), so `save_vtime` now **fails closed** off a V-time-intercept boundary — see
"HASH and SNAPSHOT have DIFFERENT correct resolutions" above. The `vt.work.work()` call-site audit
from the sixth pass still holds: every read is at a V-time intercept (deterministic) except
`current_work`, which feeds only the diagnostic MSR log line — never the hash, snapshot, or a
guest-visible value.

A **seventh** pass (on the integrator-ruling split) confirmed the HASH/SNAPSHOT resolutions but
caught a **P1** in the new flag: `vtime_synchronized` was cleared *after* `backend.run()?`, so a
failed `run()` left it stale-`true` (a later `save_vtime` would emit a stale anchor). Fixed by
clearing it **before** `run()` (`run_error_leaves_vtime_desynchronized` pins it). An **eighth** pass
returned **CLEAN — no material findings**, walking every flag path (failed run → false; non-V-time
exit → false; V-time intercept → true on success; RNG flag independent and correctly ordered).

## Task 21 — V-time work-counter + RDTSC/RNG completion + composition root

Task 21 wires the determinism-complete path above the trait: RDTSC/RDTSCP →
`VClock::tsc(work)`, RDRAND/RDSEED → the seeded entropy stream, with `work` from a
real `perf_event` counter — and proves it bit-identical on the box (P6).

- **Work source (`src/work.rs`, P3):** `WorkSource { work(); reset() }` — the seam between the
  clock and the host retired-branch counter, read at each exit. Portable `ScriptedWork` for the
  unit/property tests; box-only `PerfWorkCounter` (`src/work_perf.rs`, `cfg(linux)`):
  `perf_event_open` `BR_INST_RETIRED.CONDITIONAL` (config `0x1c4`), **`exclude_host`** (guest-only;
  VM-exits add zero branches — count-neutral, task-07 exp 3), **`pinned`** + an
  `time_enabled == time_running` check on every read (a multiplexed/unscheduled count is a hard
  error, never a silent wrong number). `reset()` = `PERF_EVENT_IOC_RESET` for snapshot restore.
  **Boundary decision (the one P3 left open):** the work source lives in the vmm-core run loop,
  **not** behind the `Backend` trait — it attaches to the vCPU thread, not to KVM-the-substrate,
  so it is identical for any backend and nothing above the trait branches on the impl (R-Backend).
  The raw perf syscalls follow `kvm_sys`'s `#[cfg(not(miri))]` + `#[cfg(miri)]`-stub pattern.
- **Completion (`src/vmm.rs`, P4):** `VtimeWiring` bundles `VClock` + `Box<dyn WorkSource>` +
  `SeededEntropy`; a `Vmm` holds it as `Option` (`None` = stock/M1/M2, where the four exits never
  surface and are a loud `ContractViolation`). `complete_tsc` reads `work`, computes
  `VClock::tsc(work)`, completes — **never a host TSC**. `complete_rng(width)` draws `width` bytes
  from the **same** `SeededEntropy` the `Entropy` hypercall uses (via its exact opcode-1 byte
  convention), so RDRAND and the hypercall RNG can't diverge. The clock config
  (`contract_vclock_config`) is 2.0 GHz / integer ratio (1 ns per branch ⇒ `tsc = 2·work`);
  integer ratio is required for snapshot-bearing configs (INTEGRATION §4). `loud_msr` now logs the
  real `work` count, not `unwired`.
- **Snapshot continuity (P4/P6):** `save_vtime`/`restore_vtime` capture `snapshot_vns(work)` +
  the entropy position; restore rebuilds the clock with `vns_base = vns`, **resets the work
  counter to 0**, and restores the stream — so `tsc(work)` and the RNG resume exactly
  (INTEGRATION §4). Unit-tested with a fresh wiring on Mac
  (`snapshot_restore_continues_the_clock_and_rng_exactly`) and on the box end-to-end (P6 below).
- **Composition root (`bringup::boot_selected`, P5):** `BackendKind::{Stock,Patched}` →
  `Box<dyn Backend>` → `boot` → (Patched) wire `PerfWorkCounter` + the contract clock + seed. The
  one place `KvmBackend`/`PatchedKvmBackend` are named; the returned `Vmm<Box<dyn Backend>>` is
  otherwise backend-agnostic.
- **Dependencies added (rule-2 sibling exceptions, task-sanctioned):** `vtime` (`VClock`) and
  `hypercall-proto` (`SeededEntropy`/`Service`). `hypercall-proto` is taken with `["host","guest"]`
  — `guest` only to keep it warning-clean under vmm-core's `clippy -D warnings` (host-only leaves
  its `core::fmt`/`Status::from_u16` dead; a one-line sibling gating fix would let us drop `guest`,
  noted as a `[question]` in `Cargo.toml`). Linux-only `libc` for `work_perf`.

### Task 21 — review round 1 (PR #45) fixes

- **[blocking] V-time/entropy state is now in `state_hash`.** `state_blob()` adds a `VTIM` chunk
  **only when `vtime` is wired** (`encode_vtime`): the clock config (incl. `vns_base`), the
  work-counter position, and the entropy stream position. So two states with identical RAM/regs but
  a different seed or `vns_base` now hash **differently** — restoring the replay-equivalence
  `unison::compare_runs` relies on (the prior hash ignored them, so two vtime-divergent states
  hashed the same yet diverged on the next RDTSC/RNG). Stock KVM / M1/M2 (`vtime: None`) emit **no**
  chunk, so their `state_hash` is byte-for-byte unchanged (the existing `event_loop` state-hash
  tests still pass; P6 box deterministic-twice still holds — the P6 hash value changes but stays
  identical across the two runs). Test: `vtime_state_is_hashed_and_distinguishes_seed_and_vns_base`
  (every clock-config field + seed + work position distinguished; stock emits no `VTIM`).
- **[hardening] `draw_rng` rejects non-architectural RNG widths.** The decoded exit width is
  untrusted; it now accepts **only `{2,4,8}`** (fail-closed on `1/3/5/6/7/…`), not the prior
  `1..=8`. Test: `rng_width_only_accepts_architectural_2_4_8`.
- **[hardening] `restore_vtime` is atomic.** It validates the cfg rebuild and the (untrusted)
  entropy blob — restoring into a **clone** — *before* mutating any live state, then resets the
  counter (last fallible step) and commits clock/cfg/entropy (infallible). A rejected snapshot now
  leaves the timeline fully intact, not half-restored. Test:
  `restore_vtime_rejects_bad_snapshot_atomically`.

Re-verified: Mac + box build/nextest/clippy/fmt/deny; Miri (vmm-core); `public-api` unchanged;
`contract_hash` unchanged (contract untouched); `cargo mutants -f vmm.rs` = 0 missed; **P6 re-run on
the box** (patched proxy modules loaded → `live_determinism` 2/2 byte-identical
`tsc=[0,2,4,6]`/`rdrand=0x9f72a62a`/`rdseed=0x42b87398` → reverted to stock).

### Task 21 — review round 2 (PR #45) fixes — the V-time **snapshot** path

- **[blocking] RNG draw vs. staged completion — clean-boundary guard.** `draw_rng` consumes the
  seeded stream eagerly (the value is needed to *stage* the completion), but `complete_read` only
  stages the reg-write/RIP-advance for the next `KVM_RUN` — which is **not** in `Backend::save` /
  `VtimeSnapshot`. A `save_vtime` taken between the draw and that commit would, on restore,
  re-execute RDRAND/RDSEED against the already-advanced stream and hand the guest the *next* word →
  divergence. **Fix:** `Vmm` tracks `rng_completion_staged` (set in `complete_rng`, cleared at the
  next `step`'s re-entry, which commits the staged completion); `save_vtime` **fails closed**
  (`ContractViolation`) at that boundary. RDTSC/RDTSCP/IO/MSR/CPUID completions are **idempotent on
  replay** (positional work; re-queried device/contract value — after `restore_vtime` resets the
  counter and sets `vns_base`, a re-executed RDTSC yields the same `tsc`), so they are not guarded —
  the guard is RNG-precise. **Disposition / scope:** capturing & replaying the *staged* completion
  (the backend-internal `complete_userspace_io` state) for a true mid-exit snapshot is **task-08**
  (`snapshot-store`'s `vm_state` blob, which owns backend-internal state — INTEGRATION.md §4); task
  21 owns only the V-time/entropy state and makes the unsafe combination impossible to do silently.
  Tests: `save_vtime_fails_closed_at_rng_mid_exit_boundary` (errors after an RDRAND step, OK after
  the next step commits it); the snapshot-continuity test now snapshots at a clean boundary.
- **[hardening] Fractional V-time rejected at config.** `save_vtime` records whole-ns
  `snapshot_vns` and `restore` resets work to 0, so a fractional `VClockConfig` (`ratio_den != 1`)
  would lose the sub-ns remainder across a snapshot (INTEGRATION.md §4; carrying it is the §6 open
  question). `VtimeWiring::new` now **fails closed on `ratio_den != 1`** (the det-cfl-v1 contract is
  exact, so this only rejects misconfiguration); its return type became `Result<_, VmmError>`
  (`ratio_den` is consequently an invariant and is no longer hashed in `encode_vtime`). Test:
  `vtime_wiring_rejects_fractional_ratio`.

Re-verified (round 2): Mac + box build/nextest/clippy/fmt/deny; Miri (vmm-core); `public-api`
refreshed (only `VtimeWiring::new`'s return type `VtimeError`→`VmmError`); `contract_hash` unchanged;
`cargo mutants -f vmm.rs -f bringup.rs` = **0 missed** (49 caught); **P6 re-run on the box** →
`live_determinism` 2/2 byte-identical (`tsc=[0,2,4,6]`, `rdrand=0x9f72a62a`, `rdseed=0x42b87398`) →
**box reverted to stock**.

### Task 21 — P6 box determinism proof (deterministic-twice + snapshot continuity)

`tests/live_determinism.rs` (box-only, `#[ignore]`) runs a 32-bit-PM Multiboot payload that
executes RDTSC ×3 (in a `dec/jnz` loop — one retired branch per read), RDTSCP, RDRAND, RDSEED and
writes the results to guest memory, via `boot_selected(Patched, …)`. **Run on `ssh <det-box>` with
the patched 6.12.90 modules loaded** (the live proxy, BUILD.md Part 2; vermagic must match
`uname -r`), CPU-pinned `taskset -c 2`, then **reverted to stock**.

Evidence (captured `--nocapture`, both runs identical → identical `state_hash`):

```
[p6] tsc reads = [0, 2, 4, 6]   aux = 0x0
[p6] rdrand = 0x9f72a62a   rdseed = 0x42b87398
[p6] snapshot/restore transparent: results = { tsc: [0,2,4,6], aux: 0, rdrand: .., rdseed: .. }
test result: ok. 2 passed; 0 failed
```

- **RDTSC = V-time, not host TSC:** `[0,2,4,6]` — exactly `2·work` (work `0,1,2,3`), strictly
  monotonic, constant 2-tick delta per retired branch. A leaked host TSC would be ~10¹³.
- **RDTSCP aux = `0`** — the contract `IA32_TSC_AUX` (the guest never WRMSR'd it).
- **RDRAND/RDSEED** = consecutive words of the seeded stream (asserted against an independent
  `SeededEntropy::new(seed)` recomputation) — never host RNG.
- **Deterministic twice:** identical `state_hash` + identical guest results across two runs.
- **Snapshot/restore mid-run:** snapshotting V-time + entropy after two reads and restoring
  (perf counter reset to 0, `vns_base`/stream restored) yields a guest image **identical** to the
  un-snapshotted reference — both clocks resumed exactly.

**Scope note:** P6 proves the *clock/RNG* snapshot continuity (the task-21 surface). A full
mid-run VM snapshot (vCPU + dirty guest memory) is task-08's `snapshot-store` and is not wired
into vmm-core yet; the test demonstrates the V-time/entropy continuity using the real perf-counter
reset within a running VM (transparent-to-the-guest restore), which is the property INTEGRATION §4
specifies for the clocks.

### Task 21 — canonical patch gate (determinism-of-record, vs `linux-6.18.35`)

The 3-patch series (`consonance/vmm-backend/kvm-patches/patches/`) **`git am`-applies cleanly** to a fresh
`linux-6.18.35` checkout (sha256 `f78602932219…`, the pinned tag) and **builds** `kvm.ko` /
`kvm-intel.ko` — the determinism-of-record target (the box runs 6.12.90 and can't *load* the
6.18.35 vermagic, so the live P6 uses the 6.12.90 proxy build, named as such). Reproduced by
`~/task21-canonical-build.sh` on the box (BUILD.md Part 1).

### Task 21 — CI wiring touched outside `consonance/vmm-core/` (sanctioned, box-only-glue)

- `.github/workflows/quality.yml` (coverage): `--ignore-filename-regex` extended to drop the
  box-only `consonance/vmm-core/src/work_perf.rs` (the `perf_event` counter) — same rationale as
  vmm-backend's `kvm_sys.rs`; the portable `work::WorkSource`/`ScriptedWork` + V-time completion
  stay coverage-gated.
- `.cargo/mutants.toml`: `**/work_perf.rs` added to `exclude_globs`; `exclude_re` gains
  `boot_selected` (box-only composition root), `current_work` (observability-only, folded into the
  `loud_msr` line), and the one provably-equivalent `draw_rng` `||`→`&&` mutant (its fail-closed
  guard is unreachable for the infallible in-tree `SeededEntropy`). The `Box<B>` blanket forwards
  are mutation-killed by `vmm-backend/tests/dyn_backend.rs` (only the trait-unobservable `inject`
  forward is excluded there).
- `tests/public-api.txt` regenerated on the box (additive: `work`/`work_perf`/`VtimeWiring`/
  `VtimeSnapshot`/`BackendKind`/`boot_selected` + the new `Vmm` V-time methods); `tests/public_api.rs`
  now skips on non-Linux (the surface gained Linux-only items). No deletions — no breaking change.

## Status — all gates green

- **Pure-logic (macOS + Linux):** `build`, `nextest` (lib + event-loop + 3
  loader-proptest), `clippy -D warnings`, `fmt`, `cargo deny` — all clean.
- **Coverage** (`cargo llvm-cov nextest --all-features --fail-under-regions 93`, the
  whole workspace, box runner): **95.6 %** regions (was < 93 %). `contract/parse.rs`
  98 %, `contract/canonical.rs` 99.6 %, `vmm.rs` 95.5 %, `bringup.rs` ~91 %.
- **Mutation** (`cargo mutants --in-diff`, box runner): **0 missed** (258 caught, 1
  non-terminating-loop mutant detected by timeout). The determinism anchor
  (`contract/parse.rs` + the §6 `contract/canonical.rs` serializer) is killed
  entirely by tests; see "Mutation hardening" below for the `.cargo/mutants.toml`
  exclusions of structurally-unkillable mutants in the supporting code.
- **Miri** (`cargo +nightly miri test -p vmm-core`, `-Zmiri-permissive-provenance`):
  clean. Wired into the `miri` job (`quality.yml`) and `MIRI_CRATES` (`pre-push`).
- **Public-API snapshot** (`tests/public-api.txt`) committed; `-p vmm-core` added to
  the `public-api` job.
- **Box-only M1 + M2** are `#[cfg(target_os = "linux")]` **+ `#[ignore]`** — out of
  the default `nextest`/coverage lane, so they can **never vacuously pass green**
  (gate-honesty fix, review round 3). Default CI shows them *not-run*. Run them
  explicitly on the box (`-- --ignored`, the M1/M2 step builds `guest/payloads`
  first); there, every precondition that blocks a real boot — no `/dev/kvm`, an
  unbuilt payload, or a §1.1 host-baseline mismatch — is a **loud panic (FAILURE)**,
  never an early-return `Ok`. **As of contract-v3 (task 11, below) the i9-9900K box
  MATCHES the §1.1 `det-cfl-v1` baseline** — `host_assert_report` shows all PASS — so
  the host-baseline precondition no longer blocks M1/M2. See the **Task 11
  re-baseline** section below (it supersedes the two `[question]` sections that
  follow it).

## Task 11 — re-baseline `det-skx-v1` → `det-cfl-v1` (contract-version 3)

The frozen CPU/MSR determinism contract was modeled on a synthetic Skylake-SP
(`det-skx-v1`). The determinism box is an **Intel Core i9-9900K (Coffee Lake-S,
`06_9e_0c`, microcode `0xf8`)**, so the §1.1 host-assert *correctly* refused to run
(3/13 FAIL). Per the integrator ruling this task re-baselines to the box CPU
(`det-cfl-v1`, contract-version 3). **Every host-forced constant is derived from and
cited to the actual box dump** committed under `docs/fragments/cfl-baseline/` — nothing
is guessed. All evidence under that dir was captured **read-only by the foreman** (the
authoritative box operator) over `ssh <det-box>` (see that dir's `README.md` for the
per-file table). The corroboration is not a second capture but the foreman's **live
on-box gates**: the §1.1 `host_assert_report` (13/13 PASS, report below) and the M1/M2
determinism gates (3/3 PASS), both run by the foreman on the box.

### Change-set (row → old → new → box evidence)

| Where | Old (det-skx-v1) | New (det-cfl-v1) | Box evidence |
|---|---|---|---|
| `[contract] version` | `2` | `3` | — |
| `cpuid-baseline` | `det-skx-v1` | `det-cfl-v1` | — |
| CPUID leaf 1 EAX | `0x00050654` | `0x000906ec` | `cpuid-raw.txt` L1 EAX = `0x000906ec` (06_9e_0c) |
| CPUID leaf 4 sub2 (L2) EBX | `0x03c0003f` (1 MiB/16-way) | `0x00c0003f` (256 KiB/4-way) | `cpuid-raw.txt` L4.2 EBX = `0x00c0003f`; decoded "4-way, 1024 sets" |
| CPUID leaf 4 sub3 (L3) EBX/ECX | `0x0280003f`/`0x000007ff` (1.375 MiB/11-way) | `0x03c0003f`/`0x00003fff` (16 MiB/16-way) | `cpuid-raw.txt` L4.3 = `…03c0003f …00003fff`; decoded "16-way, 16384 sets" |
| CPUID leaf 7.0 EBX | `0x019c27eb` | `0x009c27ab` | `cpuid-raw.txt` L7.0 EBX = `0x029c67af`; decoded FDP_EXCPTN_ONLY=false, CLWB=false ⇒ drop bits 6,24 |
| CPUID brand (leaf 0x80000003 EBX) | `(SKX-class)` | `(CFL-class)` | identity re-style (synthetic string; box brand is "i9-9900K") |
| CPUID 0x80000006 ECX | `0x04008040` | `0x01004040` (256 KiB/4-way) | mirrors authoritative L4.2; box legacy leaf is `0x01006040` — see `[question]` |
| CPUID 0x80000008 EAX | `0x0000302e` (46 phys) | `0x00003027` (39 phys) | `cpuid-raw.txt` 0x80000008 EAX = `0x00003027` |
| MSR 0x10a ARCH_CAPABILITIES | `0x400000000d10e171` | `0x000000000a000c09` | `msrs.txt` `rdmsr -a 0x10a` = `a000c09` on all 16 CPUs |
| insn XBEGIN/XEND/XTEST/XABORT | `host-pin(tsx-ctrl-rtm-disable)` / class (c) | `fault-absent` / `#UD` / class (b) | `cpuid-raw.txt` L7.0 EBX[4,11]=0; `msrs.txt` `rdmsr 0x122` #GP (TSX physically absent) |
| host-assert family-model-stepping | `06_55_04` | `06_9e_0c` | `cpuid-raw.txt` L1 EAX |
| host-assert host-microcode-rev | `0x…0200005e` | `0x…00f8` | `lscpu-microcode.txt` sysfs + `/proc/cpuinfo` = `0xf8` |
| host-assert maxphyaddr-min | `46` | `39` | `cpuid-raw.txt` 0x80000008 EAX[7:0] = `0x27` |

**Carried over unchanged** (synthetic or SDM-architectural, *not* host-forced — and so
not changed per the "change only what the host forces" rule): the single-thread
topology (leaf 0xB), XCR0={x87,SSE,AVX} / leaf-0xD layout (the box has MPX + XSAVE
opt states, but the synthetic model already excludes them — §1.2 cooperative scope),
the 2.0 GHz/25 MHz/100 MHz frequency scalars (synthetic; RDTSC is V-time-trapped so
the host's 24 MHz/3.6 GHz never leaks), `mxcsr-mask 0x0000ffff` (**box-confirmed** via
FXSAVE), `guest-ucode-rev 0x…0100000000`, `cr4-force-reserved [PKE, PKS]`, the eight
`host-absent` instructions (all confirmed absent on the box), and every §1/§3/§5
disposition. **The MSR index partition is unchanged: 1043 indices, pairwise-disjoint**
(0x10a changed value, not class; no row moved class).

### Recomputed §6 contract hash + armed anti-drift gate

```
contract_hash (v3, det-cfl-v1) = e01f0835576444c269c6603fc4984d0b425785373f4c49613d75ce896565c832
```

(canonical form: 48 306 bytes, 1355 records.) Committed in **both**
`docs/cpu-msr-contract.toml` `[contract] contract_hash` and `docs/CPU-MSR-CONTRACT.md`
§6. The golden canonical form is regenerated at
`src/contract/testdata/canonical-v3.txt` (old `canonical-v2.txt` removed), and
`contract_hash_matches_committed_registry` is **un-ignored — live and green**
(computed-from-parsed == committed). `cargo nextest -p vmm-core --all-features`: 80
passed, 2 skipped (the box-only M1/M2).

### Box validation — §1.1 host-assert all-PASS (the acceptance bar) ✅

**Real box run** by the foreman on the determinism box (Intel Core i9-9900K, kernel
`6.12.90+deb13.1-amd64`), against this branch at commit **`5da8095`**, pinned per
`docs/BOX-PINNING.md`:
`taskset -c 1 cargo test -p vmm-core --test live_m1_m2 host_assert_report -- --ignored --nocapture`.
**All 13 §1.1 assertions PASS** (pre-rebaseline this box failed 3/13 — the
`det-skx-v1` baseline correctly excluded it). Verbatim output:

```
[host-assert] CPU-MSR-CONTRACT §1.1 host-baseline report:
  PASS family-model-stepping: expected 06_9e_0c, observed 06_9e_0c
  PASS host-microcode-rev: expected 0x00000000000000f8, observed 0x00000000000000f8
  PASS mxcsr-mask: expected 0xffff, observed 0xffff
  PASS maxphyaddr-min: expected >= 39, observed 39
  PASS rtm-disabled: rtm physically absent (XBEGIN #UDs; already non-usable)
  PASS host-absent HRESET/PCONFIG/RDPID/SERIALIZE/SHA/TPAUSE/UMONITOR/UMWAIT: all absent
```

This is the acceptance bar from the task spec: the host-assert report now shows all
PASS on the 9900K. (The box-only M1/M2 *boot* gates remain `#[ignore]`d live tests; the
host-baseline precondition they share no longer blocks them.)

### `[question]` items for review (judgment calls, not host-forced)

1. **CPUID 0x80000006 ECX = `0x01004040` (256 KiB/4-way).** The box's *own* leaf 4
   (authoritative cache leaf) reports L2 = 4-way, but its legacy 0x80000006 reports
   assoc-code `0x6` ("8-to-15-way") — an on-box self-inconsistency. The contract follows
   leaf 4 (SDM-authoritative; preserves the SKX design decision "0x80000006 mirrors
   leaf 4"), emitting `0x01004040`. Alternative = box-verbatim `0x01006040`. Chose
   leaf-4-consistency; flag for confirmation.
2. **Brand string `(SKX-class)` → `(CFL-class)`.** A synthetic, guest-visible hashed
   string; not strictly host-forced (the guest never compares it to the host), but
   leaving "SKX-class" in a CFL baseline is misleading. Changed as part of the identity
   re-style ("@ 2.00GHz" kept — synthetic TSC unchanged).
3. **TSX `fault-absent` is backed by `host-assert rtm-disabled`, not a per-opcode
   `host-absent` record.** The probe recognizes RTM by its CPUID bit (7.0:EBX[11]),
   not by mnemonic, so adding `host-absent XBEGIN/&c` would need new probe logic (out of
   scope: data-only). `rtm-disabled` (which reads EBX[11] and passes on absence) is the
   assertion; the §4/§6 prose documents this. No `hostassert.rs` logic change needed.

### Crate source changes (minimal; mechanical fixture updates beyond the un-ignore)

The task anticipated one crate change (un-ignore the hash gate). Changing the contract
*data* also forces these **test-fixture/prose** updates (no policy logic touched):
`canonical_form_matches_golden` (→ `canonical-v3.txt` + v3 hash), `canonical_form_well_formed`
(v3 header/leaf-1/host-assert anchors), `lookup_cpuid_exact_leaf_only_and_default`
(leaf-1 EAX → `0x000906ec`), `report_contract_hash` label, a new `#[ignore]`d
`regen_golden` helper (matches the documented regenerate-on-bump workflow), and
`det-skx-v1 → det-cfl-v1` prose in `hostassert.rs`/`bringup.rs`/`canonical.rs` doc
comments and the `live_m1_m2.rs` test prose (the now-stale "BLOCKED pending re-baseline"
messages). The host-assert *logic* and all contract *policy* code are untouched.

## HOST-BASELINE §1.1 ENFORCEMENT — integrator decision required (`[question]`)

> **[RESOLVED by Task 11 / contract-v3, above.]** The integrator ruling (re-baseline to
> `det-cfl-v1`) has landed: identity/microcode/MAXPHYADDR now match the box and TSX is
> reclassified to physically-absent `#UD`. The box now PASSES all 13 §1.1 assertions.
> The original finding (kept below for history):

Review finding 1: `bringup::boot` installed the frozen policy and entered the guest
**without** checking the CPU-MSR-CONTRACT §1.1/§1.2 host-homogeneity assertions.
Fixed: `boot` now calls `hostassert::enforce()` **first** — before `set_cpuid`,
`set_msr_filter`, or any guest entry — and returns `VmmError::HostAssert` (listing
every failed assertion) on mismatch. The new `hostassert` module probes the
**physical host** (`CPUID(1)` family/model/stepping; `CPUID(0x8000_0008)`
MAXPHYADDR; the FXSAVE `MXCSR_MASK`; the sysfs/cpuinfo microcode revision; the
`CPUID.7` feature bits for the eight `host-absent` instructions; RTM presence
(CPUID.7.0:EBX[11]) for `rtm-disabled`) and compares against the `[host-assert]`
records. The §6
`guest-ucode-rev` and `cr4-force-reserved` records are hashed-but-not-probed (one is
the guest-visible fake, the other a guest-CR4 invariant the frozen CPUID enforces).

**Box result (i9-9900K, `taskset -c 1`, `hostassert::report()`):** the SKX baseline
**genuinely excludes** this box — 3 of 13 assertions FAIL:

| Assertion | Expected (det-skx-v1) | Observed (9900K) | |
|---|---|---|---|
| `family-model-stepping` | `06_55_04` (Skylake-SP) | `06_9e_0c` (Coffee Lake) | **FAIL** |
| `host-microcode-rev` | `0x…0200005e` | `0x…00f8` | **FAIL** |
| `maxphyaddr-min` | `>= 46` | `39` | **FAIL** |
| `mxcsr-mask` | `0xffff` | `0xffff` | PASS |
| `rtm-disabled` | rtm physically absent (pin not installed here) | rtm-absent | PASS |
| `host-absent` ×8 | absent | all absent | PASS |

These are not cosmetic: MAXPHYADDR 39 < 46 and a different µarch/microcode mean
native (non-trapping) instruction/FPU/paging behavior would differ from the frozen
SKX contract while the guest is told it is SKX — exactly the divergence §1.1 exists
to refuse. **Per the review directive the assert was NOT loosened to fake a pass.**

**Integrator status (confirmed):** the integrator has confirmed the box *correctly*
FAILS the §1.1 baseline — that is the **right** behavior, not a bug to work around.
The decision is **option (a): re-baseline to `det-cfl-v1` (contract-version 3)** — a
targeted revision (both are Skylake-family; most of the synthetic frozen model
stays): change identity (`06_55_04 → 06_9e_0c`), microcode pin, MAXPHYADDR (`46 → 39`),
and reclassify TSX from present-but-aborting (class c) to physically **absent**
(class b, `#UD`); then re-validate on the box and recompute `contract_hash`. That
re-baseline is a **separate task** (it edits `docs/`, outside this crate's scope).

**Gate honesty (review round 3 — supersedes the earlier "skip-not-fail").** The box
M1/M2 tests are `#[ignore]`d, so they are **out of the default lane** and CI shows them
*not-run* — never a vacuous green. When run explicitly on the box (`-- --ignored`), a
host-baseline mismatch is a **loud panic (test FAILURE)**, not a skip-as-pass: under the
*old* `det-skx-v1` contract the 9900K run was `test result: FAILED. 1 passed; 3 failed`
(the `host_assert_report` diagnostic passed; the three M1/M2 tests fail-loud with the
full per-assertion report). **With this re-baseline (`det-cfl-v1`) the host now matches,
so M1/M2 PASS** — foreman-run on the 9900K after building the guest payloads
(`cd guest/payloads && cargo build --release`):

```
$ taskset -c 1 cargo test -p vmm-core --test live_m1_m2 -- --ignored m1_hello m2_hello m2_compute
running 3 tests
test m1_hello_boots_and_prints ... ok           # boots, golden serial byte-for-byte, clean isa-debug-exit
test m2_hello_deterministic_twice ... ok         # bit-identical state_hash across two runs
test m2_compute_deterministic_twice ... ok       # bit-identical state_hash across two runs
test result: ok. 3 passed; 0 failed
```

(`host_assert_report` shows 13/13 §1.1 PASS — report above.) Run with:
`taskset -c 1 cargo test -p vmm-core --test live_m1_m2 -- --ignored --nocapture`.

Fixes 2–4 (`cr4-force-reserved` spelling, `state_blob` UART completeness, the MSR
loud-log context) **do not touch the M1/M2 execution path** — #2 changes only
`contract_hash`; #3 adds the same UART-config bytes to *both* runs' hashes (so
run-to-run determinism and the byte-exact serial check are preserved); #4 only logs
on userspace MSR exits, which the audited M1/M2 payloads never trigger — so the only
thing now blocking M1/M2 is the host-baseline refusal.

## CONTRACT HASH — foreman action required (`[question]`)

> **[RESOLVED by Task 11 / contract-v3, above.]** `docs/cpu-msr-contract.toml` now
> carries `[contract] contract_hash` (the v3 value `e01f0835…565c832`), the golden is
> `canonical-v3.txt`, and `contract_hash_matches_committed_registry` is un-ignored and
> green. The v2 discussion below is kept for history (the v2 hash `61d6f810…` was never
> registered — the serializer post-dated it; v3 is the first committed body-hash).

`contract::contract_hash()` computes the §6 canonical SHA-256 of the ratified v2
contract. After the review fix to the `cr4-force-reserved` canonical spelling
(below), the computed value is:

```
contract_hash (v2) = 61d6f8104ab4b4f3629ef36b3e0e80919280b8b3b3e7fa2eb38d52a35ea7755b
```

(canonical form: 48 402 bytes, 1355 records.) **This supersedes the prior
`ba767ef4…` hash, which was computed from a non-normative serialization** — §6
spells the record `host-assert cr4-force-reserved [PKE, PKS]` (a bracketed array,
`, ` separated), but the serializer joined `PKE,PKS`, so the old hash was *not* the
hash of the normative form (review finding 2). The whole canonical serializer was
re-audited against §6 §1–§8 while fixing this; `cr4-force-reserved` was the only
spelling divergence (timer device order, the 3-hex `xapic.<offset>` form, the 2-hex
`cmos` `port:`/`idx:` tokens, and `allow-fixed:<16hex>` / `emulate-*:<formula>`
cells all already matched, and the TOML pre-expands the mmio range registers).

**GOLDEN canonical form — the regression gate (review round 5).** The whole 48 402-byte
canonical form is committed at `src/contract/testdata/canonical-v2.txt`, and
`canonical_form_matches_golden` asserts `serialize(contract())` equals it **byte for
byte** (plus that `contract_hash` is `sha256` of those bytes). This is the gate that
would have caught the `cr4-force-reserved` bug: any §6 spelling/ordering drift — or a
**parser** change that alters a hashed value — is now a failing diff, not a silently
wrong hash. The parser (`src/contract/parse.rs`, the determinism anchor) gained an
exhaustive unit suite (every `parse_value` token form + reject/empty path, the
`TomlValue` accessor fallbacks, `strip_comment`, `hex32`, `reg_field`, `subleaf`,
`dispositions`, `RegField::base` bit-packing, `IndexSpec::indices`, `cpuid_row` /
`msr_row` every leaf/index form, exact `leaf_entry_count`) plus property tests
(`parse_value` totality/round-trips, and **canonical-form invariance to incidental
formatting** — the order/format independence the hash relies on). Region coverage of
`parse.rs` went from under the floor to **98%**, and the named diff-mutants
(`parse_value` `||→&&` / `==→!=`, `RegField::base` `<<→>>` / return-0,
`leaf_entry_count` return / `==`) are killed.

Per the task spec and the `contract-v2-freeze-ratified` decision,
**`docs/cpu-msr-contract.toml` does not yet carry a `contract_hash` field** —
committing it is a docs change the **foreman** makes, not this crate (scope =
`consonance/vmm-core/` only). So the `contract_hash() == toml-field` sub-gate of gate 6
is intentionally **left pending behind this `[question]`**; the rest of gate 6
stands. Once the foreman commits the hash above (seeding the §6
`(contract-version, body-hash)` registry), the `#[ignore]`d
`contract_hash_matches_committed_registry` test is un-ignored. **No unratified hash
was fabricated.**

This serializer **defines** the v2 canonical bytes (the §6 registry is seeded *from*
it — there is no prior oracle). Its rendering decisions are documented in
`src/contract/canonical.rs` and are §6-faithful: literal header spelling (decimal
scalars; `mxcsr-mask=0x0000ffff`), bare fixed-width lowercase hex in record bodies
(8 digits for CPUID/MSR-index cells, 16 for 64-bit `allow-fixed` constants),
`dyn:`/`emulate-*`/`insn` formula-ids emitted verbatim (their *meaning* is the
hashed semantics, per §6's immutability rule), range/member rows expanded to one
record per element, the §6 item ordering, and an LF after every record.

## Dependencies, grants, exceptions

- **`vmm-backend` (rule-2 exception, declared in the spec).** The whole point of the
  crate is to be the VMM *above* the `Backend` trait, so it depends on `vmm-backend`
  for `Backend`, `Exit`, `VcpuState`, `Gpa`, `Event`, `CpuidModel`/`CpuidEntry`,
  `MsrFilter`/`MsrRange`, `ExitCounts`, `BackendError`. **No `kvm-ioctls`/
  `kvm-bindings`/`vm-memory`** here — those live below the trait.
- **`unison` (dev-dependency only).** The M2 `Machine`/`MachineFactory` adapter
  lives in the box-only integration test (`tests/live_m1_m2.rs`).
- **rule-5 library deps:** `thiserror`, `sha2` (state + contract hash), `memmap2`
  (the `GuestRam` mmap). `proptest` is a dev-dep. All whitelisted.
- **Path-dep version pins.** `vmm-backend`/`unison` carry `version = "0.1.0"`
  alongside `path` so `cargo deny`'s `wildcards = "deny"` (root `deny.toml [bans]`)
  accepts them — a bare `{ path }` is a `*` requirement and fails that gate. This
  keeps the fix inside `consonance/vmm-core/` (rule 1); the workspace-wide alternative
  (`allow-wildcard-paths = true` in `deny.toml`) is a foreman call.

## The exact task-14 (`vmm-backend`) surface relied on

The event loop is written against task 14's concrete API; no divergence from it was
found (no new `[question]` against R-BACKEND while wiring the real `KVM_RUN` loop):

1. **Install-time policy via trait methods** — `set_cpuid(&CpuidModel)` and
   `set_msr_filter(&MsrFilter)`, called by `bringup::boot` **before the first run**.
   `vmm-core` supplies the data; the backend enables `KVM_CAP_X86_USER_SPACE_MSR`
   (`FILTER|UNKNOWN|INVAL`) then `KVM_X86_SET_MSR_FILTER` / `KVM_SET_CPUID2` below the
   trait. (Confirmed in `kvm_sys.rs`: the cap is enabled before the filter.)
2. **Exit-completion round-trip** — `complete_read(value)` (port/MMIO read, `Rdmsr`),
   `complete_fault()` (`deny-gp` → `#GP`), `complete_ok()` (`Wrmsr` allow/drop),
   `complete_cpuid(eax,ebx,ecx,edx)`. The loop computes the value and calls the
   matching completion; it never touches `kvm_run`.
3. **`Exit::Hlt` distinct from `Exit::Shutdown`** — M1 checks `DebugExit { code: 0 }`
   specifically, distinct from the `HLT` fallback and from a triple-fault `Shutdown`.

Also consumed: `exit_counts()` for `RunResult.exit_counts`, and `save()`/`restore()`
for the entry-state install and the M2 hash.

## How the mock-backend seam is wired

`vmm-backend` gates `MockBackend` behind a non-`#[cfg(test)]` `mock` feature, turned
on via `[dev-dependencies] vmm-backend = { features = ["mock"] }`. So a plain
`cargo build -p vmm-core` compiles the trait + value types only (no mock), while
**every test target** drives `Vmm` over the scripted `MockBackend` with no
`/dev/kvm`: a queue of `Exit`s replays a `hello`-shaped serial+exit sequence
(`tests/event_loop.rs`), and the same `boot()` / event-loop code runs against the
real `KvmBackend` on the box. This is the `vmcall-transport` loopback pattern applied
to the backend seam.

## Contract ingestion mechanism

`docs/cpu-msr-contract.toml` (the ratified canonical mirror) is **embedded with
`include_str!`** and parsed once (cached in a `OnceLock`) by a small, total
TOML-subset reader (`src/contract/parse.rs`). **No `toml` runtime dependency and no
`build.rs` codegen** — both were considered and rejected:

- A `build.rs` reading `../../docs/...` couples codegen to the repo layout and runs
  a hand-rolled parser anyway; `include_str!` achieves the same single-source-of-truth
  at compile time with less machinery and keeps the parse logic Miri-visible.
- A `toml` crate is outside the rule-5 whitelist.

The same parsed tables feed both the runtime policy (`cpuid_model`,
`msr_filter_allow`, dispositions) **and** the §6 canonical serializer, so what is
hashed is what is enforced (§6), with no second hand-maintained copy. A
`#[cfg(test)]` invariant test asserts the union of MSR indices is the contract's
pinned 1043 and pairwise disjoint.

## Miri: the granted unsafe and why the exclusion is sound

The `unsafe` is all box-path: (1) the pinned, page-aligned `GuestRam` backing; (2)
the **call** to `vmm_backend::Backend::map_memory` (an `unsafe fn`); and (3) the
host-baseline probe's `_fxsave64` read of the FPU save area for `MXCSR_MASK`
(`hostassert::probe`, a box-only read into a 16-byte-aligned 512-byte local — no
guest effect; `CPUID` needs no `unsafe` on x86-64). All three sit behind a
`cfg`-seam excluded under Miri: `GuestRam` falls back to a `Vec<u8>` (keyed on
`cfg(miri)`), and the whole `hostassert::probe` module is gated
`cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))`. Under Miri
`hostassert::report()` returns the skipped/passing outcome and `enforce()` is a
no-op, so `boot`/loader/event-loop/`state_blob` pointer-and-bounds logic is
exercised against the mock — exactly the surface the `unsafe ⇒ Miri` rule targets
(the `vmcall-transport` precedent). `cargo +nightly miri test -p vmm-core` is clean.

**`bringup::compose` — the `map_memory` seam under Miri (review rounds 3 → 5).** The
`unsafe map_memory` *call* is reached only by the `#[ignore]`d live KVM tests (never
run under Miri), while the Miri-run tests built `Vmm` directly — so the
`GuestRam`-pointer lifetime/bounds contract was escaping the interpreter. **Round 5**
factored `boot` into the §1.1 host gate (`hostassert::enforce`) **then** `compose`
(the policy-install + `GuestRam` alloc + `multiboot::load` + `write_boot_info` +
`unsafe map_memory` + restore). `compose` has **no host gate**, so the unit test
`bringup::tests::compose_drives_guestram_and_unsafe_map_memory` (a `MockBackend` + a
tiny hand-built address-override Multiboot image; asserts the loaded header / marker /
boot-info bytes via `state_blob`, not `state_hash`, to skip the ~100× sha-256 Miri
cost) runs on **every** platform — under Miri (exercising the pointer seam), on macOS,
**and on the Linux box** (where `boot` itself would refuse a non-baseline host before
reaching the RAM). That is also what restored `bringup.rs` coverage on the box from
**0 % to ~91 %** (round 3's `#[ignore]` of the live tests had left it uncovered):
`compose` is covered by that test everywhere, and `boot`'s gate wiring by
`boot_runs_the_host_assert_then_composes` (which accepts `Ok` off the box or
`HostAssert` on it).

`tests/loader_proptest.rs` disables proptest failure-persistence under `cfg!(miri)`
(its regression file needs `getcwd`, blocked by Miri isolation) and cuts cases to 24.

**`cfg(target_os)` in `hostassert` (declared rule-6 exception).** The host-baseline
probe reads the *physical CPU the guest runs on*, so it is intrinsically box-only —
the same Linux/x86-64 boundary the task already draws for `KVM_RUN` and the live
M1/M2 tests. Off the box (Mac, non-x86, Miri) there is no physical guest to protect,
so the probe is `cfg`-excluded and `enforce()` is a no-op; `boot` against the mock is
unaffected. The crate's *public* surface stays platform-identical (`hostassert::Outcome`
/ `report` are unconditional; only the private `probe` submodule and the
`contract::host_expectations` accessor are gated), so the public-api snapshot needs no
platform skip.

## Mutation hardening — the determinism anchor and the unkillable residue

`cargo mutants --in-diff` reports **0 missed**. The determinism anchor —
`contract/parse.rs` (the contract ingestion) and `contract/canonical.rs` (the §6
serializer whose SHA-256 *is* `contract_hash`) — is killed **entirely by tests**
(no exclusions): an exhaustive parser unit suite, the golden canonical-form test,
and direct tests of `cell` / `subleaf_sort_key`. The remaining mutants across the
supporting code are killed by targeted tests (event-loop dispositions, `lookup_cpuid`,
`resolve_cpuid` bit-math, the entry-state segment fields, the loader bounds, the
device accessors, `apply_entry`, …). A small, documented residue is **structurally
unkillable** and excluded in `.cargo/mutants.toml` `exclude_re` (same precedent as the
`kvm` path's `exclude_globs`), in four categories:

1. **`hostassert::probe`** (`probe::`) — the `cfg(linux)` host-CPU probe (live CPUID /
   FXSAVE / sysfs reads + the comparisons against them). It cannot be
   value-mutation-tested on a non-baseline CI runner (the box is a 9900K, not the
   frozen SKX baseline, so every assertion already fails); verified instead by its
   non-`#[ignore]`d pure-decoder unit tests, the `report()` smoke test, and the box
   `host_assert_report`. The cross-platform `report`/`enforce`/`verdict` stay gated.
2. **Disjoint-bit `|`≡`^`** (`consonance/vmm-core/src/.*replace \| with \^`) — `a | b` ≡
   `a ^ b` when the operands share no set bits, true of every bitwise-flag combine in
   vmm-core (`USER_SPACE_MSR_MASK`, `resolve_cpuid`'s clear-then-set / byte-merge,
   `CR0 = PE | NE`). No test distinguishes them. Scoped to vmm-core. (The analogous
   `1 << 0` ≡ `1 >> 0` shifts were instead **removed** by writing the bit-0 constants
   as `1`.)
3. **Observability-only `vmm.rs` helpers** (`loud_msr`, `guest_rip`) — the §1 MSR
   loud-log and the RIP it logs have no architectural effect and no test-observable
   return; plus `GuestRam::is_empty`, structurally always-`false` (`new` rejects 0).
4. **`msr_filter_allow`'s `&&`** — every `allow-stateful` row in the frozen contract
   is bidirectional (14 of them; 0 one-directional), so `&&` ≡ `||` here.

## Deviations considered and rejected

- **Direct `restore(protected_mode_entry(...))`** (as the spec sketch shows) —
  **rejected.** `KvmBackend::restore` validates the snapshot's XSAVE size and MSR
  key-set (`validate_restore_shape`), which a *pure* builder cannot produce. So
  `bringup::boot` overlays the entry registers/segments/control-regs onto a live
  `save()` template that already carries KVM's valid `TR`/`LDT`/`GDT`/`IDT`/XSAVE/MSR
  shape — the proven get→modify→set pattern of a working stock-KVM VMM. The
  `protected_mode_entry` builder still returns a self-consistent full `VcpuState` for
  the mock path and the gate-3 unit test.
- **Flat segment limit `0x000F_FFFF`** (the raw 20-bit field) — **rejected on the
  box.** KVM loads `kvm_segment.limit` straight into the VMCS and the hardware treats
  it as the **already-expanded byte limit**, so `0xFFFFF` caps CS at 1 MiB and
  triple-faults the very first instruction fetch at `EIP ≈ 0x100043` (observed as an
  empty-serial `Shutdown` before the fix). The flat limit is `0xFFFF_FFFF` with
  `G=1` (VMX entry requires `G=1` when limit bits 31:20 are set). This was the single
  bug between "infrastructure works" and "M1/M2 green".
- **`build.rs` codegen / `toml` runtime dep** for the contract — rejected (above).
- **A `target_os` fork for `GuestRam`** — rejected; keyed on `cfg(miri)` alone.
- **Loosening the §1.1 host-assert to pass on the 9900K** — **rejected** (the review
  forbade it, and it would be unsound: the box is genuinely outside the SKX domain).
  The assert refuses faithfully; the contract-vs-hardware mismatch is escalated as
  the host-baseline `[question]` above, not silently absorbed.
- **`value as u8` truncation of wide port I/O** — **rejected** (review round 3).
  `outl $0, $0xF4` (a 4-byte OUT) truncating to a `0` byte would become a *fake*
  isa-debug-exit `PASS`; a wide UART write would silently drop high bytes. The event
  loop now rejects any access to a modeled byte port (the `0x3F8..=0x3FF` UART block,
  `0xF4` isa-debug-exit) with `size != 1` as a loud `ContractViolation`
  (`require_byte_io` in `vmm.rs`, `Uart8250::owns` for the range) — fail closed, the
  default-deny posture, covered by `non_byte_io_to_modeled_ports_fails_closed`.
- **Reading the host microcode revision via `RDMSR 0x8b`** — rejected: `RDMSR` is a
  ring-0 instruction and would `#GP` in the userspace VMM. The probe reads the
  kernel-recorded revision from `/sys/devices/system/cpu/cpu0/microcode/version`
  (falling back to `/proc/cpuinfo`), which is the value the kernel sampled at boot.
- **`rtm-disabled` passing on the mere *existence* of `IA32_TSX_CTRL`** —
  **rejected** (review round 3). The contract's `rtm-disabled` is satisfied only by
  RTM being **physically absent** (XBEGIN `#UD`s) **or** vmm-core actually installing
  the `IA32_TSX_CTRL = RTM_DISABLE | TSX_CPUID_CLEAR` **pin before `KVM_RUN`**. This
  skeleton does **not** install that pin (a backend/VMCS concern, a later phase), and
  the MSR merely existing does nothing to a running guest — so the only honest pass
  is `!rtm` (CPUID.7.0:EBX[11] == 0). A TSX-capable host therefore **fails** this
  assertion here (it would run native, nondeterministic RTM) until the pin install is
  wired; the no-RTM box (and det-cfl-v1, where TSX is class-(b) absent) passes by
  absence. The earlier `tsx_ctrl`-CPU-flag heuristic was removed as unsound.

## Known limitations / deferred (per BRINGUP "later phases")

- **`run_until` / `inject` / V-time.** Not wired; the `step()` seam is shaped to
  accept them. The two V-time MSRs `0x10`/`0x3b` are represented faithfully as
  `EmulateVtime` and **fail closed** as `ContractViolation` until V-time lands (the
  audited M1/M2 payloads touch neither).
- **Userspace `Exit::Cpuid`.** Implemented (frozen model + `resolve_cpuid` overlay)
  but never fires on stock KVM (CPUID is in-kernel). `lookup_cpuid` uses a simplified
  subleaf-significance heuristic (leaves with >1 subleaf or `N+`/range marked
  significant) that does not affect the contract hash or M1/M2; a leaf like `0x7`
  with a single in-scope subleaf is not flagged significant. Refine when the
  patched/direct backend exercises this path.
- **`Mmio` / `Hypercall` / `Rdtsc`/`Rdtscp`/`Rdrand`/`Rdseed`** all fail closed as
  `ContractViolation` (xAPIC=task 13, hypercall host handler, M3 patched backend).
- **`state_blob`** is the ad-hoc length-prefixed encoding; it is superseded by task
  09's `vm_state` codec when that integrates (device state folds into the blob). Its
  `DEV` chunk now hashes the **UART register shadows + `LCR.DLAB`** alongside the
  terminal reason / debug-exit code (review finding 3), so two runs that drive the
  UART into a different register/DLAB configuration — even with byte-identical serial
  output — hash differently (their future port-I/O behavior differs). The serial
  *bytes* remain a separate `SERL` chunk.
- **MSR loud-log** (`vmm::loud_msr`, review finding 4) now emits the full §1
  context — direction, KVM exit reason, index, WRMSR data (`n/a` on a read), guest
  RIP, work/V-time, disposition — before any architectural effect. V-time is logged
  honestly as `work=unwired` (not a fake `0`) since it is a later phase.

## What the integrator must know

- **Commit the corrected v2 `contract_hash`**
  (`61d6f8104ab4b4f3629ef36b3e0e80919280b8b3b3e7fa2eb38d52a35ea7755b`) to
  `docs/cpu-msr-contract.toml` to close the deferred gate-6 sub-assertion (foreman
  docs change). This **replaces** the prior `ba767ef4…` (non-normative
  `cr4-force-reserved` spelling — review finding 2).
- **Decide the host-baseline `[question]`** above: the box (i9-9900K) fails 3 of the
  §1.1 assertions because it is not Skylake-SP. Either re-baseline the contract to
  the box CPU (version bump) or run M1/M2 on a Skylake-SP host. The host-assert was
  **not** loosened to fake a pass.
- This branch's base carries the unrelated `81f51f3` determinism-corpus docs commit
  (the worktree was cut before task 14/09 landed; the branch was rebased onto
  `origin/main` to pick up `vmm-backend`/`vm-state`). Rebase `--onto origin/main`
  to drop the stray base commit before opening the PR (per the
  `task15-stray-corpus-base` memory).
- CI wiring added (root files, as the task directs): `vmm-core` in the `miri` +
  `public-api` jobs of `.github/workflows/quality.yml` and in `.githooks/pre-push`
  `MIRI_CRATES`.
- The box M1/M2 gates are `#[ignore]`d (out of the default lane) and need the
  payloads built first: `cd guest/payloads && cargo build --release` (target
  `x86_64-unknown-none`), then `taskset -c 1 cargo test -p vmm-core --test
  live_m1_m2 -- --ignored --test-threads=1`. A missing `/dev/kvm`, an unbuilt
  payload, or a §1.1 host-baseline mismatch is a **loud panic (test FAILURE)** —
  never a silent early-return `Ok`. (On the current 9900K box the host-baseline
  panic fires, which is correct until the `det-cfl-v1` re-baseline.)

## Task 28 — corpus box-integration: the C1 corpus runs on the patched backend

The third piece that makes the conformance corpus run on the box: the **report
channel** (the one new ABI), the VMM-backed `det-corpus` `Machine`, the O2
report-stream goldens, and the box O1/O2 gate. Composes #48 (the `det-corpus`
oracle runner) + #49 (the C1 payloads) + #45 (`PatchedKvmBackend` + V-time/RNG).

### What landed

- **Report channel (`REPORT_PORT = 0x0CA2`).** A dedicated port for the payloads'
  trap-dependent values, **distinct from** #44's `0x0CA1` doorbell (a reported
  value can never be mistaken for a doorbell ring). `src/devices.rs` pins the
  constant; `src/vmm.rs`'s `dispatch_out` gains the report-port case → a 32-bit
  `OUT` appends `EAX` to a per-VM `Vec<u32>` report stream (no completion; a
  non-dword access fails closed via `require_dword_io`). `report(u64)` is two
  writes (low dword then high). Documented in `docs/INTEGRATION.md` §1.1 and a
  documentary `[ports]` row in `docs/cpu-msr-contract.toml` /
  `docs/CPU-MSR-CONTRACT.md` §4. **`contract_hash` is unchanged** — the canonical
  serializer (`contract::parse`/`canonical`) never reads the `[ports]` section, so
  it stays out of the §6 hash, exactly like the doorbell constants (the
  `contract_hash_matches_committed_registry` + golden tests are green).
- **Two digests, two oracles.** `Vmm::observable_digest()` hashes the **report
  stream + the serial banner** (domain-tagged `OBSV`, length-prefixed) — the O2/O3
  guest-observable conformance output. `Vmm::state_hash()` is **byte-for-byte
  unchanged** (a run that never touches the port leaves the stream empty, so
  M1/M2/P6 hashes and goldens are untouched). The shared digest is the free fn
  `corpus::observable_digest_of`, so a host tool can recompute an O2 golden from a
  captured stream.
  - **O1 folds the report stream in (PR #51 review fix).** `CorpusMachine::state_hash`
    (the unison `Machine` hash that `det-corpus` O1 compares) is
    `sha256(Vmm::state_hash ‖ Vmm::observable_digest)`, so a same-seed run that
    diverges **only** in `REPORT_PORT` values **fails O1** instead of passing
    falsely. The fold lives only in the corpus adapter — `Vmm::state_hash` stays
    unchanged (M1/M2/P6 byte-identical). Regression-tested
    (`corpus::tests::o1_catches_a_report_stream_only_divergence`: equal
    `Vmm::state_hash`, differing `CorpusMachine::state_hash`, `compare_runs` →
    `Diverged`).
- **The `Machine` bridge (`src/corpus.rs`).** `CorpusMachine<B>` wraps a `Vmm<B>`
  and implements `unison::Machine`: `run_to` runs the payload to terminal on first
  call → `Halted` (overriding `observable_digest` to the report-stream digest);
  generic over the backend, so the stream-digest + `Machine`-contract logic is
  mock-tested on macOS (`src/corpus.rs` tests + `tests/corpus_oracle_mock.rs`,
  which drives the real `det-corpus` runner over a scripted `MockBackend` bridge).
  The box-only `boot_patched_payload` (`cfg(linux)`) builds it via
  `boot_selected(Patched, …)`. `unison` was promoted from a dev- to a regular
  dependency (the adapter is now library code).
- **Payload `report()` is live (`guest/payloads/common/src/report.rs`).** Emits the
  two `OUT 0x0CA2` writes; under stock QEMU (no device at the port) the writes are
  discarded, so the Part-A serial gate is **byte-identical** (verified:
  `run-tests.sh` passes both runs). The payloads' `report(..)` call sites (#49) are
  unchanged — this just gives them a live transport on the box.
- **O2 goldens + manifest.** `docs/corpus-manifest.toml` declares `"conformance"` +
  `golden = "guest/golden/<name>.digest"` on the **six** C1 payloads that run to a
  clean isa-debug-exit PASS on vmm-core's current event loop (insn-rdtsc, insn-rng,
  insn-cpuid, insn-rdpmc, msr-allowed, msr-denied — trapped instructions / MSR
  dispositions / in-guest faults only). `det-corpus validate` passes (10 items,
  round-trips, the 6 goldens present). The six `.digest` files now carry the
  **foreman-captured box digests** (all 6 O1 PASS + blessed on the patched box at
  `72c7cd9`, reverted to stock — see "Foreman-captured box goldens" below); the
  V-time/seeded-PRNG-derived digests can only be captured on the patched box.
  - **Four payloads are O2-deferred** (they keep O1/O3 for the toy-registry
    self-test) because they can't reach a clean PASS on today's event loop:
    - **insn-hlt, irq-landing, pit-pic-stub** — depend on PIT/LAPIC-timer
      **interrupt injection**, LAPIC **MMIO** (0xfee00000), and the **idle-skip**
      protocol (a later phase; the "LAPIC timer interrupt landing" hard core,
      `docs/DETERMINISM-CORPUS.md` §C1). On today's event loop a bare `HLT` is
      **terminal** (`TerminalReason::Hlt`, not a PASS) and LAPIC MMIO / PIT port
      `0x61` are fatal `ContractViolation`s. The backend confirms it: `vmm-backend`
      uses `KVM_IRQCHIP_NONE` (userspace xAPIC), so there is no in-kernel timer.
    - **insn-mwait** — MONITOR/MWAIT are unmodeled on the event loop; the box run
      exits **`DebugExit { code: 1 }`** (in-guest FAIL), not a clean PASS (PR #51
      box-review issue 2 — its earlier inclusion was a source-read prediction the
      box reality refuted). It joins the deferred set; it gains conformance once
      vmm-core models MONITOR/MWAIT (alongside V-time timers + IRQ injection).
- **The box gate (`tests/box_corpus.rs`, `cfg(linux)` + `#[ignore]`d).** For every
  **conformance** manifest item, drives the VMM-backed `CorpusMachine` and asserts
  **O1** (`det_corpus::check_determinism`) + **O2** (`observable_digest` == golden,
  on a probe that also checks no run-error and a clean `DebugExit { code: 0 }`
  terminal), then re-runs the whole sweep and asserts an identical aggregate
  (**deterministic twice**). The O2-deferred items are logged and skipped (never run
  through the gate). Every blocking precondition (no patched `/dev/kvm`, non-baseline host,
  unbuilt payload, unblessed golden) is a **loud panic**, never a vacuous pass. A
  companion **`c1_corpus_o1_diagnostic`** test (box-only, non-asserting) prints
  `Vmm::state_hash` + `observable_digest` separately per run with a report-stream
  delta dump, to localize an O1 divergence (added for PR #51 box-review issue 1).

### Box-review round (task 27 #53 merged): O1 now 4/6 — `insn-rdtsc`/`insn-rng` localizer

Task 27 (#53) **merged to main and is merged into this branch** (clean). Its
`encode_vtime` now hashes `snapshot_vns(last_intercept_work)` — the effective V-time
anchored to the **deterministic** work at the last V-time intercept, **not** the live
terminal `vt.work.work()` read that the prior round identified as the skid source. So
the `VTIM` chunk is skid-free for a run that ends at a V-time intercept.

**Box result (foreman, task/28 + main):** O2 bless captured **4 of 6** conformance
goldens cleanly with **O1 PASS** — `insn-cpuid`, `insn-rdpmc`, `msr-allowed`,
`msr-denied`. These four never execute a V-time intercept, so `last_intercept_work`
stays 0 and the entropy stream stays at position 0 → their `VTIM` is trivially
constant. **`insn-rdtsc` and `insn-rng`** (the only two that advance
`last_intercept_work` / the entropy stream) still report **O1=FAIL** with:

```
diverged in (0, 1] but bisection could not localize it:
state hashes are equal at hi = 1: no divergence to bisect
```

**What that message proves.** `check_determinism` → `compare_runs` reached the
`(Halted, Halted)` branch (`CorpusMachine::run_to` always runs to terminal). It took
the **`Diverged`** path, not `HaltMismatch` — so `wa == wb` (the deterministic unison
work tick, `1`); the divergence signal was therefore `CorpusMachine::state_hash`
(`= sha256(Vmm::state_hash ‖ observable_digest)`) at the terminal checkpoint. The
bisector's **next** re-spawn then probed `hi = 1` and found the hashes **equal**
(`NoDivergence`). A divergence in one pair that the immediately-following pair cannot
reproduce is an **intermittent `Vmm::state_hash` inequality** — and since
`observable_digest` is deterministic (the report channel matched in round 1), the
intermittent part is a component of the architectural `Vmm::state_hash`.

**This round's deliverables (the diagnostic the foreman runs on the box):**

- **`state_components` now mirrors `encode_vtime` faithfully.** The pre-#53 breakdown
  exposed `vtim:vns_base` + the live `vtim:work-raw` — neither is what #53 hashes —
  and *omitted* the field that is. It now emits, as a **faithful cover** of the hashed
  `VTIM` preimage: `vtim:cfg` (now incl. `tsc_adjust`), **`vtim:eff-vns`**
  (`snapshot_vns(last_intercept_work)` — the actual hashed effective V-time), and
  `vtim:entropy`; plus two clearly-labeled **diagnostic-only** (NOT hashed) fields,
  `vtim:last-intercept` (the deterministic anchor) and `vtim:work-raw` (the live skid
  read). So a `VTIM` `state_hash` divergence now shows up as a **hashed** component,
  never as a red-herring live-read or a "diverged but every component matched" gap.
- **`c1_corpus_o1_repeat_diagnostic`** (box-only, non-asserting): repeats the exact O1
  comparison **N = 20** times for `insn-rdtsc` and `insn-rng`, logging per run both
  `CorpusMachine::state_hash` values, the `work()`/run-outcome comparison
  `compare_runs` makes first, the underlying `Vmm::state_hash` + `observable_digest`,
  the diverging `state_components` (the new faithful cover), **and** the real
  `check_determinism` verdict — then a SUMMARY with divergence counts + a per-component
  histogram. It disambiguates the two candidate fixes:
  - `CorpusMachine::state_hash` **always MATCH** across N but `check_determinism`
    still FAILs ⇒ the failure is the **oracle's detection** (a flake the single-shot
    diagnostic missed) → make the O1 comparison skid-robust; vs.
  - **intermittently DIVERGE** ⇒ **residual non-determinism**, the histogram naming
    the component (`vtim:eff-vns` ⇒ the V-time anchor `last_intercept_work` is not
    fully deterministic for these payloads; `xsave-*` ⇒ FPU init/host-leak; `RAM:*` ⇒
    guest scratch).

### Root cause + fix: the shared-thread `perf_event` work counter (coexistence)

The box localizer (`c1_corpus_o1_repeat_diagnostic`, run by the foreman, reverted to
stock after) was **dispositive**: across all completed `insn-rdtsc` runs the **direct
pair** was byte-identical every time — `CorpusMachine::state_hash` MATCH, `work` MATCH,
`Vmm::state_hash` MATCH, `observable_digest` MATCH — yet `det_corpus::check_determinism`
FAILed every run. So the machine **is** deterministic; the failure appears only through
`compare_runs`' access pattern.

**Root cause.** `compare_runs` **spawns both machines, then runs each in turn**
(`spawn ma; spawn mb; run ma; run mb`), whereas the localizer and the bisector's probe
**spawn-and-run sequentially** (`spawn; run; spawn; run`). The box work counter
(`PerfWorkCounter`) opens a `perf_event` with `pid=0, cpu=-1, exclude_host=1` —
a guest-only counter on the **shared, CPU-pinned vCPU thread** — and is **enabled at
open** (spawn). So when `mb` is spawned *before* `ma` runs, `mb`'s counter is live
throughout `ma`'s run and **accumulates `ma`'s guest branches**; `mb` then adds its own.
`mb`'s work — hence `last_intercept_work` → the hashed `vtim:eff-vns`, and for
`insn-rdtsc` the reported TSC values — is inflated by `ma`'s entire branch count, so the
two same-seed runs diverge. The four non-V-time payloads never read the counter, so they
were immune (matching the 4/6 split). The bisector's probe re-spawns `mb` *after* `ma`
finished, so its counter is clean — which is exactly why it found the hashes equal and
reported `NoDivergence`. `compare_runs` was **correctly** detecting a real coexistence
non-determinism that the sequential diagnostics masked.

**Fix.** A new `WorkSource::start_run(&mut self)` (default **no-op**), called by
`Vmm::run` immediately **before the first guest entry**, makes each run's work
self-contained. `PerfWorkCounter::start_run` issues `PERF_EVENT_IOC_RESET`, clearing any
cross-VM branches accumulated since open; in the single-VM case the counter is already
~0 (`exclude_host`: no guest ran), so it is a no-op and **P6/M1/M2 stay byte-identical**.
The portable `ScriptedWork` keeps the default no-op, so the existing V-time completion
tests (which pre-load a work value via `ScriptedWork::at(N)`, expecting `tsc = 2N`) are
unchanged. A lazy "baseline at first read" alternative was **rejected**: it would drop
the pre-first-intercept work and silently change the V-time conformance values; a
run-start reset preserves the "work counts from run start" semantic the contract assumes.

**Mac regression test (`src/corpus.rs`).** `SharedThreadWork` models the box counter's
shared-thread tally (a process-shared `Rc<Cell<u64>>` every counter observes, baselined
at open, advancing one tick per read); a `SharedWorkFactory` of `CorpusMachine<MockBackend>`
spawns coexisting machines that share one tally, run through the **real** `compare_runs`.
With the run-start reset the verdict is `Identical`; a companion
`shared_work_counter_without_run_start_reset_diverges` (the source's `start_run` set to a
no-op) asserts `Diverged`, so the positive test is provably non-vacuous — it reproduces
the box bug on macOS and pins the fix.

**Public-API note (foreman):** `WorkSource` gained the defaulted `start_run` method;
`tests/public-api.txt` is hand-updated (trait + the `PerfWorkCounter` override). The
exact rendering of an inherited default in non-overriding impls is
`cargo-public-api`-version-specific, so regenerate on the box if the box `public-api`
job flags a drift: `UPDATE_PUBLIC_API=1 cargo test -p vmm-core --test public_api -- --ignored`.

**Status — RESOLVED.** The fix made box **O1 green for all six** conformance items
(`insn-rdtsc`/`insn-rng` included): the foreman re-blessed on the patched box at `72c7cd9`
(reverted to stock) and **all 6 passed O1 and blessed cleanly**; the 4 timer/IRQ payloads
are correctly O2-deferred. Task 28's report channel, `Machine` bridge, O2/O3, and box gate
are complete, and the six foreman-captured digests are committed (below).

### Foreman-captured box goldens (`72c7cd9`, patched → reverted to stock)

All six conformance items passed O1 and were blessed on the determinism box after the
perf-counter coexistence fix; the digests below are committed to `guest/golden/<name>.digest`
(replacing the `PENDING-BOX-CAPTURE` sentinels). These are **foreman-captured box evidence**
(this worker has no box access).

| Item | O2 digest (`observable_digest`) |
|---|---|
| `insn-rdtsc`  | `1065ab4cf566433b2eec2b756810c4c5e75775012d63f46aafefc273bdb78ae3` |
| `insn-rng`    | `0fe06bf4edf727fc1d200f810a307c30e65915b7c1ed230a5e513defbb2a3926` |
| `insn-cpuid`  | `746d8bbbeb4591f8a2ef35eeefcb6dee306b4257999133f74eaf295f848216a9` |
| `insn-rdpmc`  | `25222db6da48e96daf022a1141e288918db4024e8e0e35fc966d7c3021b76dc1` |
| `msr-allowed` | `323ce46ba0a82269daaab47bd46a8f67c914f29994b0986daa34fe600a95e2e5` |
| `msr-denied`  | `b29a3e8591e1c03062f3efc905888425eeaa8df455ecb612a2ab499f0b9fbc60` |

The foreman runs the **official** (non-`BLESS`) deterministic-twice O1/O2 gate against
these committed goldens before merge.

### Official box gate — PASSED + final cross-model P2 fixes

The foreman ran the official non-`BLESS` gate on the box (reverted to stock): **all 6
conformance items O1=PASS O2=PASS, aggregate identical across two sweeps**
(`f8a694ee…`), `test result: ok` — the corpus proof point. The final cross-model pass
surfaced two **non-blocking P2s**, both **off** the corpus `run()` path (so the passing
gate stands); fixed here before merge:

- **P2-1 — work-counter prepare gated at the *actual* first guest entry.** The
  run-start reset was at the top of `Vmm::run`; a `step()`-then-`run()` consumer
  (telemetry/diagnostics) would either skip the early `step()` entries or restart work
  mid-run. Moved to a `first_entry_done` guard inside `step()` (the single first
  `backend.run()`, shared by `step()`/`run()`), so the prepare fires **exactly once, at
  the first guest entry**, regardless of how the VM is driven. `run()` no longer touches
  it. The corpus is `run()`-only so the gate is unchanged; this fixes the `step()`
  consumers. Test: `start_run_fires_once_at_first_guest_entry_via_step_or_run` (drives
  `step(); step(); run()` and asserts the prepare count is 1).
- **P2-2 — O2 conformance oracle compares `observable_digest`, not `state_hash`.** The
  committed `.digest` goldens are `observable_digest` values (and the box gate compares
  `observable_digest`), but `det_corpus::check_conformance` compared `state_hash`, so the
  generic `det-corpus run` would have mis-compared O2. Reconciled by making
  `check_conformance` compare `observable_digest` — which **degrades to `state_hash`** for
  a machine that doesn't override it (the toy goldens are unaffected) and is the
  report-stream digest for the VMM bridge, so the generic runner and the box gate now
  compare the **same** quantity (and it matches the manifest header's stated "O2:
  observable_digest == golden"). det-corpus tests updated to capture goldens via
  `observable_digest` (`o2_conformance.rs`, `report.rs`); `check_conformance`'s public
  signature is unchanged.

Re-verified after both fixes (Mac): vmm-core build/nextest (126)/clippy/fmt; det-corpus
build/nextest (42)/clippy/fmt; `det-corpus validate` (10 items, round-trips, goldens
present). No box round needed (the gate passed and neither fix touches the corpus
`run()` path).

### Final cross-model round — 3 minor cleanups (all off the corpus `run()` path)

- **[P2] `CorpusMachine::run_to` rejects a rewind.** Per the `unison::Machine` contract,
  `run_to(target)` with `target < work()` is `Err(MachineError::TargetBehind)`, checked
  **before** the halted no-op (e.g. `run_to(0)` after a terminal run set `work() == 1`).
  Previously `target` was ignored and a rewind silently returned `Halted`. `compare_runs`/
  `bisect` only ever call with non-decreasing targets, so the gate is unchanged. Test:
  `run_to_rejects_a_rewind_below_current_work`.
- **[P2] The box gate resolves the O2 golden via the manifest.** `box_corpus` now reads
  (and blesses) each golden at the path from `docs/corpus-manifest.toml`'s `golden` field
  (`golden_path(item)`), not a hardcoded `guest/golden/<name>.digest`, so the gate and the
  manifest cannot drift to different files.
- **[P3] Contract `[ports]` table nests correctly.** `docs/cpu-msr-contract.toml`'s
  REPORT_PORT row was `[[port.entry]]` (a separate top-level array, leaving the documented
  `[ports]` table empty); it is now `[[ports.entry]]`. **`contract_hash` unchanged** — the
  §6 canonical serializer never reads `[ports]` (`contract_hash_matches_committed_registry`
  + the golden canonical-form test stay green).

Re-verified (Mac): vmm-core nextest (127) + det-corpus nextest (42) = 169 pass; clippy
`-D warnings` clean; fmt clean; `det-corpus validate` ok; `contract_hash` unchanged.

### Box capture + run (foreman, on the patched box)

The O2 goldens and the O1/O2 evidence are **box-only**; capture then verify:

```sh
cd guest/payloads && cargo build --release            # build the C1 payloads
cd ../..
# 0a. N-run localizer for the two failing payloads (insn-rdtsc / insn-rng): per run,
#     CorpusMachine::state_hash A/B + work + Vmm::state_hash + observable_digest +
#     diverging components + the real check_determinism verdict; SUMMARY + histogram:
taskset -c 2 cargo test -p vmm-core --test box_corpus c1_corpus_o1_repeat_diagnostic \
    -- --ignored --nocapture
# 0b. (all 6, single-shot) state_hash vs observable_digest + report-stream deltas:
taskset -c 2 cargo test -p vmm-core --test box_corpus c1_corpus_o1_diagnostic \
    -- --ignored --nocapture
# 1. capture the report-stream goldens (skips any item that fails O1):
DETCORPUS_BLESS=1 taskset -c 2 cargo test -p vmm-core --test box_corpus \
    c1_corpus_o1_o2_on_the_patched_backend -- --ignored --nocapture
git diff guest/golden/*.digest                        # review, then commit
# 2. verify the gate (O1+O2, deterministic twice):
taskset -c 2 cargo test -p vmm-core --test box_corpus \
    c1_corpus_o1_o2_on_the_patched_backend -- --ignored --nocapture
```

Then **revert the box to stock KVM**. Paste the step-2 output here (the
`[box-corpus] … O1=PASS O2=PASS …` lines + the matching aggregates).

> **Box evidence — CAPTURED (`72c7cd9`, patched → reverted to stock).** The foreman
> ran the bless on the box after the perf-counter fix: **all 6 conformance items passed
> O1 and blessed cleanly** (the 4 timer/IRQ payloads correctly O2-deferred). The six
> digests are committed to `guest/golden/` (table above), replacing the sentinels. The
> foreman then runs the **official** non-`BLESS` deterministic-twice O1/O2 gate against
> the committed goldens before merge (proxy patched modules at
> `<box>/kvm-spike/deb612/.../kvm{,-intel}.ko`). This worker has no box access, so all box
> figures here are foreman-captured.

### Deviations considered & rejected; limitations

- **O2 reads `observable_digest`, not `state_hash` (so it does NOT use
  `det_corpus::check_conformance`).** `check_conformance` digests `state_hash`
  (the full V-time/RAM/entropy state) — correct for the design-doc's O2, but the
  task pins the **report stream** (the guest's *deliberate* conformance output),
  which is the stable, meaningful signal (`state_hash` is brittle to RAM layout /
  rebuilds). So `box_corpus` runs O1 via the `det-corpus` runner and does O2 itself
  against `observable_digest`. `det-corpus` is **unaffected** (no library change);
  the toy `det-corpus run` over the example manifest is untouched.
- **`run_to` runs to terminal (no intra-run work-targeting).** A C1 payload always
  runs to a terminal, and stopping the vCPU at an arbitrary work count needs the
  `run_until` deadline path (a later phase). So O1 compares `state_hash` at the
  single terminal checkpoint (the M2 adapter's shape) — sufficient and proven by
  P6. `work()` is `0`/`1`; the box gate runs **O1 + O2** (the spec's gate). O3 is
  **not** run on the VMM bridge: with `work() == 1`, the RNG-payload work-stability
  clause would be vacuous, so claiming O3 here would be misleading. O3 stays
  exercised by `det-corpus`'s toy self-tests (real intra-run work counter); a
  meaningful VMM-bridge O3 waits on `run_until`. Documented in `src/corpus.rs`.
- **A failed run is captured, not panicked, inside the bridge.** `run_to` stores
  the `VmmError` (`run_error()`) and returns `Halted` so a deterministic *failure*
  can't masquerade as a deterministic *pass* in `compare_runs`; `box_corpus`
  checks `run_error()` on a probe machine and the clean `DebugExit { code: 0 }`
  terminal, failing loud on either. (The box factory's `spawn` still panics on a
  boot failure — a genuine box-setup failure, same posture as the live M1/M2
  `PayloadFactory`.)
- **Public-API snapshot.** New public surface (the `corpus` module, `REPORT_PORT`,
  `Vmm::{report_stream, observable_digest, terminal_reason}`) is added to
  `tests/public-api.txt` by hand; the snapshot is generated on the Linux box and
  the `boot_patched_payload` item is Linux-only, so the foreman should re-bless it
  on the box (`UPDATE_PUBLIC_API=1 cargo test -p vmm-core --test public_api --
  --ignored`) if the hand-edit drifts from `cargo public-api`'s exact rendering.
- **Mac gates green:** `build` / `nextest` (105 pass, 2 box-only skipped) /
  `clippy -D warnings` / `fmt` for `vmm-core` + `det-corpus`; `cargo deny` ok; the
  QEMU Part-A shape gate green (both runs byte-identical); `contract_hash`
  unchanged. The live box O1/O2 run is box-only (evidence pending above).

## Task 32 — interrupt injection drive (the V-time LAPIC timer → `GUEST_READY`)

Completes task 30: the V-time-driven LAPIC timer now delivers its vector to the
guest, so the periodic tick advances, the userspace 8250 TX drains, and the box
boot reaches `GUEST_READY`. The backend half (the `KVM_INTERRUPT` / interrupt-window
handshake) lives in `vmm-backend`; this is the **minimal vmm-core glue** that drives
it.

- **`Vmm::service_lapic_timer`** runs once at the top of every `step()`, **before**
  `backend.run()` (so the queued IRQ rides the upcoming entry). It `advance_to`s the
  LAPIC to the current `lapic_now_vns()` (firing the timer vector into IRR when due,
  re-arming if periodic), then — if a vector is now deliverable above the processor
  priority — `take_interrupt()` moves it IRR→ISR and `backend.inject(Event::Interrupt
  { vector })` queues it. Calling it every step does **not** double-inject: a taken
  vector sits in ISR (raising PPR), so `take_interrupt` returns `None` for the same /
  same-class re-armed vector until the guest EOIs — the one-per-tick delivery the
  kernel expects. EOI flows back through the already-wired xAPIC MMIO path
  (`mmio_write(APIC_EOI)` → `Lapic::eoi`).
- **Linux-path-gated.** `service_lapic_timer` returns immediately when the xAPIC is
  unwired (M1/M2/corpus/multiboot never wire it), so those paths call neither
  `inject` nor `advance_to` — their observable state and `state_hash` are
  byte-for-byte unchanged (no determinism/contract regression; gate 4).
- **`lapic_now_vns` now advances on stock too.** It still feeds both the
  Current-Count register read and the timer expiry, but the work value it reads
  differs by *capability* (`Backend::capabilities().deterministic_tsc`), not backend
  identity:
  - **Determinism-complete** (patched KVM / the mock): the skid-free
    `last_intercept_work` anchor — **unchanged** from before, the same value the
    `VTIM`/`LAPC` hash uses. The patched backend traps every `RDTSC`, so the anchor
    advances densely *and* deterministically; two same-seed boots fire the timer at
    bit-identical V-times (Phase B.2 / task-30 Phase C).
  - **Stock** (no `RDTSC` trap): the anchor would freeze post-boot (only the rare
    `RDMSR(IA32_TSC)` advances it), stalling the tick and the serial-TX drain — so
    read the **live** work counter, which advances with guest branches. Stock claims
    no determinism (Phase B.1 only *reaches* the milestone), so a skid-laden live
    read is sound; a failed counter read degrades to the anchor.

**Why no `run_until`.** The timer is *checked* at exits, not run to a precise
`run_until` deadline (that remains task 07). During boot the guest exits densely
(xAPIC MMIO, serial I/O, and — patched — every `RDTSC`), so the periodic tick still
advances. This keeps the task self-contained (no PMU single-step dependency).

**Tests (mac-runnable + Miri).** Three scripted-`MockBackend` tests in
`src/vmm.rs`: `lapic_timer_injects_when_vector_becomes_deliverable` (deterministic
backend — the timer advances off the RDTSC-set anchor, and constant live work alone
would *not* fire it, pinning the anchor branch), `lapic_timer_injects_on_stock_off_live_work`
(stock caps — fires off the advancing live-work clock with no intercept, pinning the
other branch), and `no_injection_when_lapic_unwired` (M1/M2 no-op). `cargo mutants`
over the diff: **0 missed** (3 caught, 1 unviable). The injectability/window handshake
itself is tested below the trait (vmm-backend's synthetic-`kvm_run` `plan_irq_entry`
tests, under Miri).

**Box gates (run by the integrator on the det-cfl-v1 host).**
`tests/live_linux_boot.rs::gate3_linux_guest_ready_and_clean_poweroff` is the
milestone gate (stock → `GUEST_READY` + clean poweroff, Phase B.1) and
`c_linux_boot_deterministic_twice_patched` the determinism gate (Phase B.2). The
default boot cmdline already presets `lpj=`/`tsc=reliable`/`no_timer_check` (so
calibration does not hang); `BOOT_CMDLINE` overrides it if the box run shows the
guest needs the tick programmed differently to emit `GUEST_READY`.

**No public-API change** (`service_lapic_timer`/the `lapic_now_vns` edit are
private), so `tests/public-api.txt` is unchanged.

### Task 32 — review fixes (PR #59)

Interrupt-correctness fixes from the cross-model review (the IRR/ISR + multi-IRQ
semantics task 33's serial IRQ builds on):

- **Re-arbitrate every entry (the determinism + stale-vector fix, codex P2).**
  `service_lapic_timer` runs at every entry: `advance_to`, then **peek** the current
  highest deliverable vector (`Lapic::peek_interrupt` — it stays pending in the LAPIC
  IRR) and hand it (or `None`) to `Backend::set_pending_irq`, **overwriting** the
  backend's single slot. The LAPIC IRR is the real multi-IRQ queue, so the backend
  never injects a stale vector: if the guest raised TPR or a higher-priority IRQ
  arrived since the last entry (every LAPIC access exits to here), the re-peek passes
  the current vector or `None`, never the old one — and a lower/second IRQ is never
  dropped (it stays in the IRR). This replaced the round-1 backend FIFO queue +
  `lapic_irq_inflight` gating, which could inject a queued vector after a TPR raise.
- **Deferred IRR→ISR.** The IRR→ISR transition (interrupt *acceptance*) is done by
  `complete_lapic_delivery`, called after `backend.run()` and **before** dispatching
  the exit, only for vectors the backend reports it actually accepted
  (`Backend::take_accepted_interrupt`, i.e. `KVM_INTERRUPT` issued). So a
  snapshot/`state_hash` or a guest APIC read while a vector waits on the interrupt
  window sees it pending in IRR, never prematurely in-service. Pinned by
  `injected_vector_stays_in_irr_until_accepted` / `accepted_vector_moves_irr_to_isr`
  (mock `set_defer_accept` models the window wait) and
  `stale_vector_re_arbitrated_away_after_tpr_raise` (P2: TPR raised mid-window → the
  stale vector is re-arbitrated to `None`, not injected, retained in IRR).
- **Fail-closed work read.** `lapic_now_vns` returns `Result` and propagates a
  work-counter error as `VmmError::Work` (was `unwrap_or(stale anchor)`, which would
  freeze/shift the timer) — same posture as the TSC/RNG completions. Pinned by
  `lapic_now_vns_fails_closed_on_work_error`.

Box-verified post-refactor: a real boot traces `[inj-peek] vector=236` (re-arbitrated
each entry while pending) → `[inj-accept]` (accepted once, IRR→ISR completed), clean
reach-userspace, no delivery regression. `cargo mutants --in-diff` over the full PR
diff: 0 missed (26 caught, 2 unviable). Miri: vmm-core 128/0, vmm-backend 0-fail.

## Task 33 — reach GUEST_READY: fix the userspace serial-TX path (serial IRQ 4)

The final step to the headline goal. Task 30 boots Linux to userspace and task 32 delivers
interrupt injection, but `/init`'s `echo GUEST_READY` (a **userspace** 8250 TX write) failed —
exit 13, *immediately* — while the kernel's *polled* printk console worked. This task
diagnosed the exact mechanism and fixed it; on stock KVM the box now reaches `GUEST_READY` and
powers off cleanly. All changes are in `consonance/vmm-core/` (the 8250 model + a PIC-IMR latch
in `devices.rs`, the IRQ routing in `vmm.rs`); the task-32 `vmm-backend` injection seam is
reused unchanged (it is vector-agnostic).

### Gate 1 — diagnosis (the exact mechanism, box-confirmed)

The userspace write failed because the kernel chose the **NULL legacy PIC**, which strands COM1's
IRQ 4. The chain, every link confirmed in the box boot log (`<box-logs>/gate3*.log`):

1. **`probe_8259A` rejects our PIC.** Linux probes the 8259 by writing `~(1<<2) = 0xFB` to the
   master IMR (port `0x21`) and reading it back; a read that is not the value written means "no
   PIC". The old `LegacyPlatform` stubbed `0x21`/`0xA1` to **all-ones**, so the read-back was
   `0xFF ≠ 0xFB` → boot log: **`Using NULL legacy PIC`**.
2. **No PIC ⇒ no legacy IRQ controller.** `null_legacy_pic` has `nr_legacy_irqs() == 0`, so no
   legacy IRQ descriptors are allocated — boot log: **`preallocated irqs: 0`**. IRQ 4 has no
   `irq_chip`.
3. **`request_irq(4)` fails ⇒ the serial tty cannot open.** Opening `ttyS0`/`/dev/console` runs
   `serial8250_do_startup`, whose `setup_irq` → `request_irq(4)` returns `-EINVAL` (no desc).
   `do_startup` `goto out`s with the error, so the tty open fails and the userspace
   write returns an error **immediately** (matching task 32's "exit 13, the spin-loop after the
   echo never ran"). The kernel's printk console is the **polled** path (`uart_console_write` →
   `wait_for_xmitr`, no IRQ), which is why kernel logs appeared but userspace TX did not.
4. **Not a drain/needs-ticks problem.** A separate box run with the task-32 LAPIC timer firing
   (`[inj-peek] vector=236` … `[inj-accept]`) still failed at the same `echo` (exit 13) — so
   timer delivery does **not** fix it. The blocker is the serial **open**, upstream of any TX drain.

A secondary register-modeling bug compounds it: the model returned the **FCR shadow** on an `IIR`
read (they share port `+2` but are distinct read-only/write-only registers). After autoconfig
`FCR == 0`, so `IIR` read `0x00` — `NO_INT` *clear* — which would make the kernel's THRE/TXEN
probes believe an interrupt is always pending and mis-detect the TX path.

### Gate 2 — the fix (Linux-path-gated; `GUEST_READY` on stock, PROVEN)

Three pieces, all no-ops when the LAPIC/legacy platform are unwired (M1/M2/corpus):

- **`devices::LegacyPlatform`** models the 8259 **master/slave IMR** (`0x21`/`0xA1`) as read/write
  latches (reset all-masked `0xFF`) instead of all-ones. `probe_8259A`'s read-back now matches →
  the kernel installs the **real 8259** (`Using NULL legacy PIC` gone; **`preallocated irqs: 16`**)
  → IRQ 4 gets a chip → `request_irq(4)` succeeds → the serial tty opens → the write succeeds.
  `irq_masked(irq)` exposes the line state; both IMRs are folded into the `LEGY` state-hash chunk.
- **`devices::Uart8250`** reports the **THRE interrupt**: a read of `IIR` (`+2`) is *computed* from
  `IER.THRI` + THR-empty (`UART_IIR_THRI 0x02` when asserted, else `UART_IIR_NONE 0x01`) rather
  than echoing the FCR shadow, and `thre_irq_asserted()` exposes the COM1 line. The kernel's THRE
  test, TXEN-bug test, and IRQ handler all then read a faithful `IIR` (`NO_INT` clear ⇒ "the IRQ
  works"), so the driver uses the interrupt path and we deliver it.
- **`vmm::service_pending_irqs`** routes the THRE interrupt to **ISA vector `0x34`**
  (`ISA_IRQ_VECTOR(4) = 0x30 + 4`, the master-PIC ICW2 window; verified against the kernel source
  and the boot's virtual-wire-mode / no-IO-APIC config) through the task-32 `set_pending_irq` seam,
  **arbitrated under the LAPIC timer** (local-APIC interrupts outrank the legacy ExtINT line) and
  **gated by the 8259 mask**. The serial vector is an ExtINT EOI'd at the 8259, so it takes no LAPIC
  IRR/ISR transition (`complete_irq_delivery`'s `take_interrupt` is a provable no-op when a serial
  vector is the one accepted, because arbitration only injects it when the LAPIC has nothing
  deliverable). Delivery is **edge-driven by the guest's own `IER` write** — a deterministic
  function of guest execution, no V-time/wall-clock.

**Box evidence (stock `KvmBackend`, det-cfl-v1 i9-9900K, committed artifacts —
`initramfs.cpio.gz` rebuilt to the manifest `f0bb7c0d…`).**
`gate3_linux_guest_ready_and_clean_poweroff` **PASSES**:

```
Using NULL legacy PIC          <-- GONE; instead:
NR_IRQS: 4352, nr_irqs: 24, preallocated irqs: 16
serial8250: ttyS0 at I/O 0x3f8 (irq = 4, base_baud = 115200) is a 16450
Run /init as init process
GUEST_READY                    <-- userspace TX drained over the COM1 IRQ
[gate3] done: steps=24712 terminal=Some(Hlt) reached_userspace=true GUEST_READY=true step_error=None
test result: ok. 1 passed
```

Clean `Hlt` poweroff (was `Shutdown`/panic), bounded budget, no contract violation. Box run:

```sh
# on ssh <det-box>, stock KVM, CPU-pinned (det-cfl-v1 host); build the guest image first
make -C guest fetch && make -C guest/linux image
taskset -c 1 cargo test -p vmm-core --test live_linux_boot -- --ignored --nocapture \
    --test-threads=1 gate3_linux_guest_ready_and_clean_poweroff
```

### Gate 3 — deterministic-twice (patched): structurally sound; box run deferred to the integrator

The serial IRQ adds **no nondeterminism**: it asserts on the guest's `IER.THRI` write, is gated by
the guest's PIC-IMR writes, and is injected at the next injectable VM-entry — all deterministic
functions of guest execution (the `LEGY` hash now includes both IMRs). So two same-seed boots on
`PatchedKvmBackend` enable THRI at identical points and drain identical serial; the property holds
by construction, on top of the already-box-proven V-time/RNG determinism (P6, task 21/27) and the
task-32 timer-injection determinism.

**Box-practicality caveat (flagged for the integrator).** On the patched backend every `RDTSC`
traps to V-time, and V-time advances per retired branch — so jiffies tick only as V-time crosses
the timer period, and the **i8042 controller probe** (`i8042: Probing ports directly`, whose
flush/wait loops spin on a jiffies timeout) takes an enormous number of guest branches: a real boot
sat in that probe for >5 min of wall-clock without reaching userspace. This is a pre-existing
patched-boot/timer characteristic, **orthogonal to the serial IRQ** (the stock boot clears i8042 in
0.33 s and reaches `GUEST_READY`); the determinism gate (`c_linux_boot_deterministic_twice_patched`,
written and unchanged) was never run before (task 30/32 deferred it). To make it practical, run it
with the patched modules and either a very long timeout or a `BOOT_CMDLINE` that skips the probe,
e.g. `BOOT_CMDLINE="… i8042.noaux i8042.nokbd i8042.nopnp"`. Patched-module load/run/revert follows
the task-21 pattern (`trap revert EXIT` → always back to stock `kvm 1396736`); I verified the load
and the i8042 stall on the box, then reverted to stock cleanly.

### Gate 4 — no regression + standard gates

- **M1/M2/corpus byte-identical (by construction).** Neither the LAPIC nor the legacy platform is
  wired on those paths, so `service_pending_irqs` early-returns and emits no `LEGY`/`LAPC` chunk;
  the `IIR`-read and IRQ logic are never reached. The shared `common::uart` polled payloads only
  ever read `LSR` (never `IIR`) and keep `IER == 0`, so the `DEV`/`SERL` hash chunks are unchanged.
  Full `cargo nextest -p vmm-core` (incl. the event-loop state-hash tests): 183 passed.
- **Mutants:** `cargo mutants --in-diff` over the task-33 diff — **0 missed** (47 caught, 1
  unviable). The 8250 `IIR`/`IER`/THRE-assert logic and the PIC-IMR latch/`irq_masked` bit math are
  pinned by exact-value tests in `devices.rs`; the serial-vector routing/arbitration/`0x34` mapping
  by `serial_*` + `lapic_vector_outranks_the_serial_line` tests in `vmm.rs`.
- **Miri:** no new `unsafe`; the new logic is pure integer/bool over existing types. Clean under the
  pinned nightly (`-Zmiri-permissive-provenance`).
- **public-api:** unchanged — every new accessor is `pub(crate)` (`thre_irq_asserted`, `irq_masked`,
  `pic_imr`) or a module-private `const`; no fully-public item added.
- build / clippy `-D warnings` / fmt / `cargo deny`: green.

### Deviations considered and rejected

- **Trick the kernel into the *polled* TX path (set `UART_BUG_TXEN` via an always-`NO_INT` IIR).**
  Rejected: a 16450 has `tx_loadsz == 1`, so the bug-path `serial8250_start_tx` drains only **one**
  byte per `write()` — `GUEST_READY` would lose all but its first byte. The interrupt path drains
  byte-by-byte and is what the integrator directed. (Faithful `IIR` also keeps the kernel detecting
  a real 16450, matching the prior boot log.)
- **Route IRQ 4 through the LAPIC IRR/ISR (the task's first-cut hypothesis).** The diagnosis shows
  the guest uses the **8259 in virtual-wire mode** (no IO-APIC, no MADT), so IRQ 4 is an ExtINT the
  guest EOIs at the **PIC**, not the LAPIC — routing it through the LAPIC ISR would never get a
  LAPIC EOI and would wedge. Injected as a legacy vector via `KVM_INTERRUPT` (the seam already does
  exactly this), with EOI/re-assert handled by IF-gating + the PIC mask.
- **Model the PIC's internal in-service/EOI to prevent re-injection.** Unnecessary: the handler runs
  with `IF=0` (no injection mid-handler) and `handle_level_irq` masks IRQ 4 at the PIC for its
  duration (we honor that mask); the line de-asserts when the kernel clears `IER.THRI` after the TX
  drains. EOI writes to `0x20`/`0xA0` are accepted and dropped.

### Known limitations

- COM1/IRQ 4 only (and the task-32 timer); no IO-APIC/MSI, no other ISA lines (non-goals).
- The PIC in-service state is not modeled (IF-gating + IMR mask suffice for this single-vCPU guest);
  a guest that re-enables `IF` inside its IRQ-4 handler before EOIing could see an extra injection —
  Linux's level handler does not.
- Gate 3's patched determinism run is impractically slow at the i8042 probe (see above) — a
  patched-boot characteristic for the integrator to run with the noted mitigation.
  **(Resolved in task 34 below.)**

## Task 34 — deterministic Linux boot (Phase C): same seed ⇒ bit-identical to GUEST_READY

The headline milestone: two same-seed boots of the real `guest/linux` bzImage + initramfs on the
**patched** backend now produce **bit-identical** serial (including `GUEST_READY` + clean poweroff)
and `state_hash`. The serial IRQ (task 33) adds no nondeterminism, so determinism held *by
construction* once the patched boot **completed** — the only blocker was that it didn't complete in
bounded time. Task 34 removes that blocker. One source file changed (`devices.rs`); the loader,
inject seam, serial path, and LAPIC are reused unchanged.

### Gate 1 — the patched boot completes (the i8042 fix)

**The blocker (task 33 diagnosis, re-confirmed empirically here).** On the patched backend every
`RDTSC` traps to V-time, so the kernel's `delay_tsc` busy-wait advances V-time per branch and the
**i8042 keyboard-controller probe** spins enormously. The exact spin: our `LegacyPlatform` returned
`0` for the i8042 status port (`0x64`), i.e. **OBF clear** (output-buffer-full, bit 0). The i8042
driver's `i8042_controller_init` issues a *read-controller-command-byte* (`RCTR`), which calls
`i8042_wait_read()` — a loop of `I8042_CTL_TIMEOUT` (10000) × `udelay(50)` waiting for OBF to set.
OBF never sets, so it runs the full 10000-iteration timeout. On **stock** KVM that timeout clears in
~0.33 s; on the **patched** backend the same wait strands the boot for **minutes**.

**Option A (kernel cmdline) is insufficient — empirically disproven on the box.** I first tried the
task-33-suggested `BOOT_CMDLINE="… i8042.noaux i8042.nokbd i8042.nopnp"`. The patched boot **still
stranded** at the i8042 probe (log stuck at `i8042: PNP detection disabled`, no progress for >60 s).
Reason: `nokbd`/`noaux` only skip the *keyboard/mouse port* setup; `i8042_controller_init` (and its
spinning `RCTR`) runs regardless. So a cmdline alone cannot bound the spin.

**The fix — option B, model the i8042 status to fail fast (the one source change).**
`LegacyPlatform::read(0x64)` now returns `I8042_STATUS_FAST_CLEAR = 0x01` — **OBF set, IBF clear** —
on every read. With OBF set, the kernel's *first* i8042 step, `i8042_controller_check` →
`i8042_flush`, drains its **bounded** `I8042_BUFFER_SIZE` (16) slots and then reports **"No
controller found"** (`-ENODEV`), so `i8042_init` aborts **before** it creates the platform device or
runs the spinning `RCTR`. The i8042 cost is capped at the 16-iteration flush; IBF-clear also keeps
any `i8042_wait_write` instant. The guest has no keyboard/mouse (a non-goal), so "no controller" is
the honest outcome. The status is a **constant** — no device state, so nothing folds into the
`state_hash` (no `KBD` chunk needed), and the read is a pure function of the port, so both patched
runs read it identically (determinism preserved). **No cmdline change** was needed — the device fix
bounds the boot on both stock and patched with the unchanged `DEFAULT_CMDLINE`.

No other jiffies-timeout spin blows up downstream: the calibration loops were already pinned by
task 33's `lpj=`/`tsc=reliable`/`no_timer_check`, and the patched boot now runs straight through to
`GUEST_READY` (293,943 steps, ~14 s/boot wall) with no further stall.

**Box evidence — the i8042 fast-fail (stock + patched).**
```
i8042: PNP: No PS/2 controller found.
i8042: Probing ports directly.
i8042: No controller found        <-- the controller-check fail-fast (was a multi-minute spin)
Run /init as init process
GUEST_READY
```

### Gate 2 — deterministic-twice (patched, the milestone) — PROVEN

`tests/live_linux_boot.rs::c_linux_boot_deterministic_twice_patched` **passes on the box** with the
LOADED patched KVM modules (det-cfl-v1 i9-9900K), the manifest-pinned guest image
(`initramfs f0bb7c0d…`), and the unchanged default cmdline:

```
[boot] run A: steps=293943 terminal=Some(Hlt) reached_userspace=true GUEST_READY=true
[boot] determinism: serial_len A/B = 6312/6312,
       state_hash A = 773a301bbff5879d5e5f2ad6f03f4b1d60528c3425e8c2d3e2dbdc9b633465d6
       state_hash B = 773a301bbff5879d5e5f2ad6f03f4b1d60528c3425e8c2d3e2dbdc9b633465d6
test result: ok. 1 passed; finished in 28.03s
```

Two same-seed patched boots → **identical** 6312-byte serial capture (incl. `GUEST_READY` and the
clean `Hlt` poweroff) and **identical** `state_hash`
`773a301bbff5879d5e5f2ad6f03f4b1d60528c3425e8c2d3e2dbdc9b633465d6`. **Same seed ⇒ bit-identical
Linux.** I strengthened the gate to also assert `GUEST_READY` is present (so two identical-but-
stranded boots cannot pass vacuously) and to print the equal digests for the record.

Box run (patched-module load → run → **always revert to stock**, per [[box-patched-kvm-ops]]):
```sh
# ssh <det-box>; load patched kvm/kvm-intel.ko (proxy size 1400832), CPU-pinned
taskset -c 1 timeout 340 cargo test -p vmm-core --test live_linux_boot -- --ignored --nocapture \
    --test-threads=1 c_linux_boot_deterministic_twice_patched
# rmmod kvm_intel kvm && modprobe kvm_intel  → verify stock `kvm 1396736`
```

### Bonus — preserve `IER` across the divisor-latch window (codex follow-up from #60)

In `Uart8250`, offset 1 is the **IER** when `LCR.DLAB=0` but the **divisor-latch-high byte (DLM)**
when `DLAB=1`. The model previously shadowed both in `regs[1]`, so programming the divisor (a DLM
write in the DLAB window) **clobbered the IER** — a later `thre_irq_asserted()` would then read the
divisor byte as if it were `IER.THRI`. The DLM now lives in a separate `dlm` field; `regs[1]`
holds only the true IER. This was latent for the current boot (115200 baud ⇒ divisor `0x0001`,
DLM `0x00`, so no corruption), which is why task 33 reached `GUEST_READY` despite it, but it is a
real correctness fix for any nonzero divisor-high. `dlm` is **not** hashed: it drives no model logic
and is `0` on the polled M1/M2/corpus paths, so omitting it keeps those `DEV`-chunk hashes
byte-identical (a regression-free fix). Pinned by `ier_preserved_across_divisor_latch_window`; the
prior test that *encoded* the bug (`thri_ignored_…`) is corrected to assert the separation.

### Gate 3 — no regression (all box-verified where it counts)

- **Stock `GUEST_READY` gate (task 33 gate3) — PASS on the box.** With the i8042 fix and the
  unchanged cmdline: `i8042: No controller found` → `Run /init` → `GUEST_READY`, clean `Hlt`
  poweroff, no contract violation (`steps=14559`, was 24712 — fewer because the i8042 probe no
  longer times out).
- **M1/M2 — PASS on the box (stock):** `live_m1_m2` 4 passed, 0 failed (`m1_hello_boots_and_prints`,
  `m2_compute_deterministic_twice`, `m2_hello_deterministic_twice`) — serial goldens + `state_hash`
  byte-identical. The i8042 fix is **Linux-path-gated** (`LegacyPlatform` is `None` for M1/M2/corpus,
  so `read(0x64)` is never reached), and the IER/DLM split leaves `regs[1]` identical when `DLM=0`.
- **P6 — PASS on the box (patched):** `live_determinism` 2 passed, 0 failed
  (`p6_rdtsc_rng_are_deterministic_and_vtime_backed`, `p6_snapshot_restore_resumes_both_clocks_exactly`)
  — the V-time/RNG determinism + snapshot-clock-resume the milestone sits on, untouched (the change
  is orthogonal to V-time/RNG/snapshot).
- **det-corpus O1/O2 — PASS on the box (patched):** `box_corpus::c1_corpus_o1_o2_on_the_patched_backend`
  → `test result: ok. 1 passed (276.72s)` — 6 conformance items O1+O2 green, deterministic twice
  (4 timer/IRQ items O2-deferred as before), aggregate `edbed419…`. Every item's digest is
  **bit-identical across runs and matches the committed task-28 goldens** (`insn-rdtsc 1065ab4c…`,
  `insn-rng 0fe06bf4…`, `insn-cpuid 746d8bbb…`, `insn-rdpmc 25222db6…`, `msr-allowed c1ebdcd7…`,
  `msr-denied b29a3e85…`). The corpus path wires neither the legacy platform nor the xAPIC, and its
  polled UART writes `DLM=0`, so the report-stream O2 goldens and `state_hash` are unchanged.
- **Mutants — 0 missed:** `cargo mutants --in-diff` over the `devices.rs` change — **13 caught / 13**.
  The i8042 status (exact `0x01`, OBF-set/IBF-clear) and the IER/DLM split are pinned by
  `i8042_status_reports_obf_set_so_the_probe_fails_fast`, `ier_preserved_across_divisor_latch_window`,
  and the corrected `thri_ignored_…` / `legacy_reads_give_absent_idle_values` tests.
- **Miri — clean:** no new `unsafe` (the change is pure integer/bool over existing types); the 18
  `devices` tests run clean under the pinned nightly (`-Zmiri-permissive-provenance`).
- **public-api — unchanged:** no new fully-public item (`I8042_STATUS_*` are private `const`s, `dlm`
  is a private field). `public_api_matches_snapshot` passes on the box; no snapshot refresh needed.
- build / clippy `-D warnings` / fmt / `cargo deny` / `cargo nextest -p vmm-core` (185 passed): green.

### Gate 4 — box hygiene

Every patched-module run reverted to **stock KVM (`kvm 1396736`)** afterward via a `trap revert EXIT`
(`rmmod kvm_intel kvm; modprobe kvm kvm_intel`), verified by `lsmod` size each time. No `/dev/kvm`
left held, no patched proxy (1400832) left loaded.

### Deviations considered and rejected

- **Option A — skip the probe via `i8042.noaux i8042.nokbd i8042.nopnp` cmdline.** Rejected:
  empirically disproven on the box (still strands at `i8042_controller_init`'s `RCTR` spin — those
  flags only skip kbd/aux *port* setup, not the controller init). The device-model fail-fast is the
  actual fix; it also needs no change to the stock boot's cmdline.
- **Model the i8042 as a present-but-empty controller (respond to self-test/`RCTR`).** Rejected:
  more code and it introduces controller state to fold into the hash, for no benefit — the guest
  needs no keyboard, so "no controller found" is honest and stateless. The OBF-set fast-fail makes
  the *presence check* fail in ≤16 `udelay`s, which is all that's required.
- **Fold `DLM` into the `state_hash`.** Rejected: it would add a byte to the `DEV` chunk and change
  the M1/M2/corpus `state_hash` (a golden regression) for zero determinism benefit — `DLM` drives no
  logic and same-seed runs produce the same value regardless. The IER (which *does* drive the THRE
  line) remains hashed via `regs[1]`.

### Known limitations

- "No controller found" is the deliberate i8042 outcome (the guest has no keyboard/mouse). A future
  guest that genuinely needs a PS/2 device would require modeling the controller, not failing its
  probe — out of scope (non-goal).
- The patched boot reaches `GUEST_READY` in ~293,943 steps / ~14 s wall per boot; this is bounded
  and deterministic, not optimized. Faster boot (e.g. `run_until`-precise timer stepping) is task 07,
  not this milestone.

---

# Task 39 — live VM snapshot / branch

vmm-core is now "the elsewhere" that `snapshot-store` and `vm-state` defer their KVM side to.
Three pieces:

1. **`src/snapshot.rs` — the `vm_state` adapter + the `SnapshotEngine`.**
   - `SnapshotEngine` owns a `snapshot_store::Store`: `snapshot_base` (booted image →
     `begin_base`/`write_page` per frame/`seal`), `snapshot_derive` (pages dirtied since a parent →
     `derive`/`write_page`/`seal`), `materialize` (→ a private CoW `Mapping`), `vm_state` (decode the
     sealed blob), plus `retain`/`release`/`gc`/`store_stats`/`stats`.
   - Pure, bidirectional conversions between the live `vmm_backend` value types and `vm-state`'s
     plain-data records (segment pack/unpack, regs/sregs/events/debugregs/mp/xcr0/msrs/xsave), and a
     **vmm-core-owned device blob** carried inside `vm_state::DeviceBlob` (task 09's placeholder).
2. **`src/vmm.rs` — the live save/restore methods on `Vmm`:** `save_vm_state`/`restore_vm_state`
   (memory-less half), `restore_guest_memory`/`guest_memory` (memory half), `restore_snapshot`
   (both), `reseed_entropy` (branch), and the gated `wire_snapshot_hashing` that folds the canonical
   blob into `state_hash`.
3. **`src/devices.rs`** gained `pub(crate)` restore setters + a `dlm()` accessor for the 8250 UART
   and the legacy platform, so the device blob round-trips.

`Cargo.toml` adds `snapshot-store` + `vm-state` (a **reviewed dependency addition**, like the
`kvm-*` deps; both pinned `version` + `path` for `cargo deny`'s wildcard ban). No new third-party
deps — the CoW mmap lives in snapshot-store, the byte layout in vm-state.

## Snapshot contents map (INTEGRATION.md §4 / §5)

| §4 item | Source | `vm_state` home |
|---|---|---|
| GPRs / segments / CRs / EFER / APIC_BASE / debug / events / MP / MSRs / XSAVE / XCR0 | `Backend::save()` | the typed records |
| V-time clock + `snapshot_vns` | `VtimeWiring` (reuses `save_vtime`) | `vtime: VtimeState` |
| entropy PRNG position | `SeededEntropy::save_state()` | `hypercall: Vec<u8>` |
| `IA32_TSC_ADJUST` | `VtimeWiring::tsc_adjust` | **device blob** |
| userspace xAPIC | `lapic::Lapic::snapshot()` | **device blob** |
| 8259 IMRs + PCI latch | `LegacyPlatform` | **device blob** |
| 8250 UART (regs + serial capture) | `Uart8250` | **device blob** |
| report stream (`REPORT_PORT` writes — O2 output) | `self.report_stream` | **device blob** |
| `contract_hash` | `contract::contract_hash()` | `contract_hash` (compared on restore) |
| timer queue | — (vmm-core has no `vtime::TimerQueue`) | empty `TimerQueueState` |

**The device blob.** `vm-state`'s typed records have no field for the xAPIC, the 8259/PCI latches,
the UART, or `IA32_TSC_ADJUST`. Task 09 carries the device section as an opaque, length-delimited
placeholder ("the vmm-core adapter passes through whatever the device models emit"); this is that
emission — a small, versioned, little-endian TLV (`"DEV1"` magic) vmm-core owns end to end and the
codec never interprets. Decode is **total** (fuzzed by `device_blob_decode_is_total_on_garbage`).
When task 13 folds a typed `LapicState` into `vm-state` under a bumped `VM_STATE_VERSION`, the xAPIC
sub-record can move out; tsc_adjust/UART/legacy stay vmm-core-owned.

`IA32_TSC_ADJUST` rides the device blob (not the MSR map) because it is an `emulate-vtime` MSR
(userspace-serviced, *not* `allow-stateful`): it never appears in `Backend::save().msrs` and must
not be fed to `Backend::restore` (KVM rejects a filtered MSR). Keeping it in the vmm-core-owned blob
leaves the MSR map exactly "the allow-stateful set".

## Representable-subset lossiness (deliberate, sound at the quiescent point)

The live `VcpuState` is a superset of `vm-state`'s records: `VcpuEvents`'s full injection bookkeeping
is projected to the 6-field determinism subset; `kvm_sregs2` `flags`/`pdptrs` and the always-zero
`DebugRegs.flags` are not carried. These are **zero at the quiescent point a snapshot is taken**
(§4: after an exit is fully serviced, nothing armed; long-mode/paging-off guests use no PAE PDPTRs),
so the projection is faithful there — proven byte-exact over the subset by
`vcpu_state_round_trips_through_vm_state`. A future workload that snapshots mid-instruction with a
queued exception would grow §4's checklist and these records (tracks the workload, a non-goal here).

## Quiescent-point assertion + atomic restore

`save_vm_state` fails closed at an RNG mid-exit boundary and (V-time wired) at a non-synchronized
point — the same guards `save_vtime` enforces. The LAPIC IRR/pending state is *captured* (state, not
an armed plan); the backend's per-entry `set_pending_irq` slot is re-derived from the LAPIC on the
restored VM's first service, so there is no plan to serialize (why `vm-state` has no plan field).
`restore_vm_state` is **atomic on rejection**: contract-hash, device-blob decode, LAPIC coherence,
clock rebuild, and entropy validation all run before any live state mutates.

## state_hash folding — "gate the swap"

`wire_snapshot_hashing()` opts a VMM into appending a `VMST` chunk (`= build_vm_state().encode()`) to
`state_blob`. **Default off**, so M1/M2/P6/corpus/Linux-boot blobs are byte-for-byte unchanged (no
golden moves); the snapshot/branch path opts in so a snapshot's `vm_state` integrity drives the hash.

**Deviation considered and rejected:** *replacing* the existing `VCPU`/`LAPC`/`VTIM` chunks with the
single canonical `VMST` chunk. Rejected — it would move every existing `state_hash` and the spec's
explicit priority is "goldens don't move." Appending-behind-a-gate folds the blob into the hash with
zero golden churn (`wiring_snapshot_hashing_folds_the_canonical_blob_into_the_hash`).

## Restore mechanism — here vs. the `vmm-backend` follow-up

`restore_guest_memory` overwrites the owned `GuestRam` from the materialized image. KVM reads the
guest through that backing, so the restored memory is live on the next `KVM_RUN` — a **correct**,
**memcpy-class** (O(image)) restore, the right thing within this directory (rule 1).

The **O(dirty) memslot-swap** that beats full-`memcpy` (gate 2's headline) and the **dirty-log
harvest** that yields the precise per-snapshot dirty set are KVM-specific and live **below the
`Backend` trait, in `vmm-backend`**: `KVM_GET_DIRTY_LOG` + a `KVM_SET_USER_MEMORY_REGION` remap
pointing the memslot at `materialize()`'s CoW mapping (task 08's chosen mechanism). They are a
**reviewed `vmm-backend` follow-up** (a small `Backend::remap_memory` / dirty-log seam), out of this
task's directory. The engine here is ready: capture is already dirty-set-proportional
(`snapshot_derive(parent, mem, Some(dirty), …)`, and even the write-all path is dirty-proportional in
*storage* via the store's seal-time dedup), and `materialize()` yields the exact CoW `Mapping` the
memslot would point at. The box probe `gate2_restore_latency_probe` prints the current materialize +
copy + the full-memcpy baseline so the follow-up has a number to beat.

## Gates

**Mac (all green):** `build` / `clippy -D warnings` / `fmt` / `nextest` (213 tests) / `deny` (the
`NCSA` warning is pre-existing). **Miri** validates the device-blob byte-parsing + the conversions;
the `materialize` (mmap) tests are `#[cfg_attr(miri, ignore)]` (Miri cannot execute `mmap`, same as
snapshot-store's own materialize tests). **mutants** — exact-value tests pin the adapter field set
(segment pack/unpack bit positions, every device-blob field, the vcpu round-trip) and the restore
logic (contract-mismatch / RNG-boundary / clock-resume value tests). **public-api** — refreshed on
the box; the new surface is `Vmm::{save_vm_state, restore_vm_state, restore_snapshot,
restore_guest_memory, guest_memory, reseed_entropy, wire_snapshot_hashing, snapshot_hashing_wired}`,
`VmmError::Snapshot`, and the `snapshot` module.

**Box (`tests/live_snapshot_branch.rs`, `#[cfg(target_os="linux")]` + `#[ignore]`):**
- `gate1_restore_replays_bit_identical` — the milestone: snapshot a running patched VM at a clean
  V-time intercept, restore into a fresh VM, run forward; the restored continuation's `state_hash` +
  serial are **bit-identical** to the un-snapshotted reference. *Same state ⇒ same future.* (The
  snapshot point is an RDTSC intercept — its completion replays idempotently, the design's intended
  clean boundary; the model treats a literal `HLT` as terminal, so the conceptual "quiescent HLT" is
  the serviced-intercept boundary.)
- `gate3_n_vms_share_one_read_only_base` — N branches materialized from one base store the base's
  pages once store-wide.
- `gate2_restore_latency_probe` — prints capture / materialize / restore-copy / full-memcpy timings.

Run pinned per `docs/BOX-PINNING.md` (task-08's core 4), patched modules loaded, reverted to stock
after:

```sh
taskset -c 4 cargo test -p vmm-core --test live_snapshot_branch -- --ignored --test-threads=1
```

> **Box-run results (2026-06-26, patched KVM, `taskset -c 4`, sibling cpu12 idle).** All three
> gate tests passed; the box was reverted to stock afterward.
>
> - **Gate 1 (milestone) ✓** — reference and restored `state_hash` are **equal**:
>   `53d1be2770d78c2d4edfa9c01b4468304c969533ce0ef21a8d512b3e4068dd74` (both). The restored
>   continuation is bit-identical to the un-snapshotted reference — *same state ⇒ same future.*
> - **Gate 3 ✓** — 1 base unique page; after 8 branches still **1** store-wide (resident 51,094 B):
>   one read-only base shared, not 8× copied.
> - **Gate 2 (probe)** — for a 4 MiB image: base capture 5.84 ms, **`materialize` = 27.9 µs**
>   (dirty-set-proportional, sparse — only resident non-zero pages touch the tempfile), restore copy
>   9.38 ms, full-memcpy baseline 1.04 ms. The materialize is already O(dirty); the O(image) restore
>   *copy* is the memcpy-class part the `vmm-backend` memslot-swap follow-up (above) replaces to
>   "beat memcpy". (Debug build; relative shape is the point.)
> - **Gate 5 (hygiene) ✓** — patched `kvm 1400832` loaded for the run, reverted to stock
>   `kvm 1396736` after (verified via `lsmod`). Task 36 was idle; the box was free.
> - **public-api ✓** — `tests/public-api.txt` refreshed on the box (pinned nightly +
>   `cargo public-api`); the `public_api` gate is green (the +37 lines are this task's new surface).

## PR #7 cross-model review fixes

### Round 3 — and the class-closing audit

Rounds 1–2 were whack-a-mole on one root cause: *the snapshot omits a live field and the restore
silently zeros it.* Round 3 fixes the last three instances **and** audits the whole adapter so every
field is **captured, asserted-zero at save, or rejected at restore** — the blob is provably
lossless-or-rejected.

- **[P1] Restore refuses a staged backend completion (not just RNG).** Any read-style / MSR / CPUID /
  determinism exit leaves a reg-write/RIP-advance pending in `kvm_run` that `Backend::restore` does
  **not** clear, so restoring into such a backend would commit the *old* exit on the next run. A new
  `Vmm::completion_staged` flag (set each `step` from the serviced exit via `exit_stages_completion`,
  superset of `rng_completion_staged`) makes `restore_vm_state` require a fresh/committed backend.
  *(Save still allows a non-RNG staged completion — restore re-executes it idempotently into a fresh
  target; only RNG, non-idempotent, is refused at save.)*
- **[P2] Restore rejects a non-empty `timers` section.** vmm-core has no `vtime::TimerQueue` (the only
  timer is the xAPIC timer, in the device blob), so a non-default `timers` would be silently dropped —
  now fail-closed. (A vmm-core blob always seals it empty.)
- **[P2] Save rejects a non-zero `kvm_debugregs.flags`.** DR0..3/DR6/DR7 are carried; the `flags`
  field is not (KVM defines it as currently always 0) — added to `snapshot::unrepresentable_state`.

**The audit — every `VcpuState` / `vm_state` field:**

| Field | Disposition |
|---|---|
| `regs` (all GPRs/RIP/RFLAGS) | captured |
| `sregs` segments (+ all L/DB/G/AVL/unusable/present/dpl/s bits), GDT/IDT, CR0–CR8, EFER, APIC_BASE | captured |
| `sregs.flags`, `sregs.pdptrs` | **asserted zero at save** (PAE-only; 64-bit guest) |
| `xcr0` | captured |
| `debugregs.db`/`dr6`/`dr7` | captured |
| `debugregs.flags` | **asserted zero at save** (KVM always-0) |
| `events` — pending exc vector/code, NMI/SMI pending, interrupt shadow | captured (6-field typed subset) |
| `events` — in-flight injection / payload / SMM / triple-fault (14 fields) | **captured (device blob, full `kvm_vcpu_events` — task 41; was "asserted zero at save" under task 39)** |
| `events.flags` (KVM validity mask) | captured (device blob, task 41; was excluded under task 39) |
| `mp_state`, `msrs`, `xsave` | captured |
| V-time clock + `tsc_adjust` + entropy | captured (typed `vtime` + device blob + `hypercall`) |
| xAPIC, 8259 IMRs, PCI latch, 8250 UART, report stream | captured (device blob) |
| `contract_hash` | captured + compared at restore |
| `timers` | empty at save; **rejected at restore if non-empty** |
| staged backend completion (`kvm_run`) | **rejected at restore** (require fresh/committed backend) |
| backend pending-IRQ slot | re-derived from the LAPIC on the restored VM's first service (not stored) |

Anything added to `VcpuState`/`vm_state` later must extend `unrepresentable_state` (or be captured),
or `save_vm_state` would seal it lossily — the audit is the contract.

### Round 2

- **[P1 — determinism] Restore re-arms the first-entry work-counter gate.** `restore_vm_state` now
  resets `first_entry_done = false`, treating the restored VM as a **fresh spawn**: its next `step`
  re-runs `WorkSource::start_run` (the per-VM baseline) right before VM-entry. On the shared box
  `perf_event` counter, without this a coexisting VM's branches between the restore (which resets the
  counter) and the restored VM's entry would be miscounted into the restored V-time — a determinism
  bug on the explorer's N-concurrent-VM path. *(test: `start_run` fires again after restore)*
- **[P2 — fail-closed] `save_vm_state` rejects unrepresentable `kvm_sregs2` flags/pdptrs.** These are
  not carried by the subset and are zero for the 64-bit / paging-off determinism guest; a non-zero
  value now fails the snapshot closed instead of being silently zeroed on restore.
- **[P2 — fail-closed] `save_vm_state` rejects unrepresentable pending-event state.** The pending-event
  fields outside the captured 6-field subset (in-flight exception/interrupt/NMI injection, the
  exception payload, SMM state, a queued triple fault) fail the snapshot closed if non-zero — zero at
  a quiescent point. The KVM validity-mask `kvm_vcpu_events.flags` is **excluded** (ioctl metadata,
  normally non-zero). Both checks live in `snapshot::unrepresentable_state`. *(tests pin each field +
  the flags exclusion)*

### Round 1

- **[P1] `save_vm_state` fails closed on a `Backend::save` error.** It no longer reads the vCPU via
  `current_vcpu` (which swallows a save error into `VcpuState::default()` for the best-effort hash);
  it reads `saved_state` or `self.backend.save()?` and propagates — a snapshot can never seal a
  zeroed vCPU and return `Ok`. (`build_vm_state` now takes the vCPU as a parameter; the gated `VMST`
  hash chunk keeps the best-effort `current_vcpu`, unchanged.) Proven by a save-failing backend.
- **[P1] The report stream is captured + restored.** `self.report_stream` (the ordered `REPORT_PORT`
  output that feeds `observable_digest` / O2) rides the device blob (v2), so a branch taken after
  report writes resumes them instead of restoring an empty stream and diverging on O2. It does **not**
  reach the default `state_hash` (O1): that path emits no `VMST` chunk (snapshot-hashing is opt-in),
  so O1/O2 stay separate.
- **[P2] Legacy-platform wiring mismatch is rejected.** A blob whose legacy subrecord is absent (or
  present) where the VM's is not is refused (fail-closed, symmetric with the LAPIC check) rather than
  silently skipped — which would leave the 8259 IMRs / PCI latch stale.

## Known limitations / integrator notes

- The **dirty-log harvest + O(1) memslot-swap restore are the `vmm-backend` follow-up** (above).
  This task delivers the portable substrate (engine + adapter + correct memcpy-class restore) that
  the explorer (task 12) and the branching demo (task 40) build on; "branch" *is* `restore_snapshot`
  + `reseed_entropy`.
- **Timer queue is empty** in the blob: vmm-core has no `vtime::TimerQueue`; the only timer is the
  xAPIC timer (in the device blob). A future `TimerQueue` would fill `vm_state.timers`.
- **Restore wiring must match the snapshot source**: `restore_vm_state` refuses a V-time/xAPIC
  wiring mismatch loudly rather than silently dropping state.
- `contract_hash` is **compared** on restore (a blob from a different ratified contract is refused).

# Task 41 — non-quiescent snapshots: capture in-flight CPU event/interrupt state

**The unlock.** Task 40's branching demo measured the binding constraint: task 39's codec could only
serialize a **quiescent** machine, so a never-halting interrupt-driven guest (Postgres + the LAPIC
timer) was snapshottable at **0 of 8392** post-readiness V-time points (5280 non-synchronized, **3112
in-flight injection**). Task 41 makes **any V-time point** snapshottable by capturing the in-flight
CPU event/interrupt state task 39 dropped — the deferred PR#7 P2 done **properly** (capture, not
fail-closed-reject). It is a **single-crate change in `consonance/vmm-core/`** (rule 1): the gap was
the vmm-core snapshot codec, not the backend or `vm-state`.

## The precise gap and why it lives entirely in vmm-core

The full `kvm_vcpu_events` (all 21 fields) already exists on `vmm_backend::VcpuState.events`, already
round-trips through `Backend::save`/`restore` (`KVM_GET/SET_VCPU_EVENTS`), and is already hashed in
full by `encode_vcpu_state` (the `state_hash` `VCPU` chunk). The **only** thing dropping it was the
vmm-core snapshot codec: `snapshot::to_vm_events`/`from_vm_events` projected to the reduced 6-field
`vm_state::VcpuEvents`, and `snapshot::unrepresentable_state` **fail-closed-rejected** any of the 14
in-flight fields. So the entire fix is in `src/snapshot.rs` + `src/vmm.rs`; no contract change, no
`vm-state`/`vmm-backend`/`lapic` edit.

## What changed

1. **The full `kvm_vcpu_events` rides the vmm-core device blob (v2 → v3).** `DeviceState.events:
   vmm_backend::VcpuEvents` is encoded verbatim by `put_events` / decoded by `Reader::events` (fixed
   declaration order, the same byte order as `vmm::encode_events`, so the two never disagree on the
   field set). The device blob is the established vmm-core-owned escape hatch for "state the typed
   `vm-state` records have no field for" (it already carries the xAPIC/8259/PCI/UART/`tsc_adjust`); the
   full events join it. Zero at a quiescent point ⇒ M1/M2/corpus blobs carry an all-zero record.
2. **Restore makes the device-blob events authoritative.** `restore_vm_state` builds the vCPU from the
   typed records, then `vcpu.events = dev.events` (a strict superset of the typed subset) **before**
   `Backend::restore`, so `KVM_SET_VCPU_EVENTS` re-establishes the in-flight injection exactly. The
   typed reduced record is still filled on save (task-39 `vm-state` codec compatibility) — vestigial on
   the full restore path, documented as such.
3. **`unrepresentable_state` no longer rejects events.** It keeps only the genuinely-uncarried,
   always-zero-for-this-guest PAE fields (`sregs.flags`/`pdptrs`) and `debugregs.flags`. An interrupt
   in flight is now representable, so `save_vm_state` succeeds at a non-quiescent point.
4. **The inject-seam is re-derived, not serialized.** The backend's per-entry `set_pending_irq` slot
   (an IRQ raised+routed but not yet injected) is **not** stored: the restored LAPIC IRR (device blob)
   + the UART THRE / 8259 mask (device blob) fully determine it, and the restored VM's first
   `service_pending_irqs` re-peeks the identical vector. Proven by
   `snapshot_restore_re_derives_the_in_flight_lapic_irq` (defer-accept holds a timer vector in IRR
   across save → restore; the restored VM re-derives `pending_irq == Some(0x40)`).
5. **Staged completions stay defined-out, not captured.** `save_vm_state` still fails closed at an RNG
   mid-exit boundary and (V-time wired) a non-synchronized point — the *exactness* guards, unchanged. A
   non-idempotent staged completion is excluded by snapshotting only at a clean, synchronized boundary,
   of which an interrupt-driven guest has *many* (every RDTSC the workload retires). This is the spec's
   "capture it **or** be defined to exclude it" — we define it out; an RDTSC-intercept boundary's
   completion replays idempotently, so the in-flight interrupt at that boundary is what task 41 adds.
6. **Three new public items** (for the box gate's split, without reaching below the `Backend` trait):
   `Vmm::has_inflight_event_injection() -> bool` — `true` iff the live vCPU is at a point the OLD 14-field
   task-39 predicate rejected (a genuine injection **or** an inert residual); **this is the gate's SEAL
   condition and 0 → N flip count** (PR #12 round 3). `Vmm::has_active_event_injection() -> bool` — `true`
   iff a **genuine** `kvm_vcpu_events` injection is in flight (the *active* subset: an injected
   interrupt/exception/NMI, a pending exception/NMI/SMI, a queued triple fault, or a valid SIPI), excluding
   residuals. `Vmm::has_pending_guest_interrupt(&mut self) -> Result<bool>` — `true` iff a genuine
   interrupt is pending in the LAPIC IRR (re-arbitrated by `peek_interrupt`) or the serial ExtINT line is
   asserting; tracked as **bonus** evidence only (task 39 already serialized the LAPIC IRR, so an IRR-only
   point is not a task-39 win — see §5). (public-api refreshed on the box.)

## state_hash / goldens — no movement

The `state_hash` already hashed the full events (`encode_vcpu_state` → `encode_events`, all 21
fields), and those fields are **zero on the M1/M2/P6/corpus/Linux-boot paths**, so no golden moves.
The device blob only reaches the hash through the opt-in `VMST` chunk (`wire_snapshot_hashing`, default
off), so changing the blob format (v3) leaves every default `state_hash` byte-for-byte unchanged. The
spec's "re-bless goldens only if a non-Linux path's hash changes" → none does.

## Deviations considered and rejected

- **Extend `vm_state::VcpuEvents` to all 21 fields** (the "obvious" home). Rejected: it touches the
  `vm-state` crate (rule 1 — single directory), bumps that crate's wire format/version, and moves its
  goldens/tests — larger blast radius for no benefit. The device blob is vmm-core-owned, versioned, and
  exactly the documented place for "state the typed records can't express." Capturing there is additive
  and backward-compatible with the task-39 `vm-state` contract.
- **Serialize the inject-seam `pending_irq`.** Rejected as redundant and a determinism risk: the
  restored LAPIC IRR + UART/8259 already determine it, and the first post-restore service overwrites
  any stored slot anyway — storing it could only *disagree* with re-derivation. (Re-derivation is the
  task-39 design, now proven across save/restore for an in-flight vector.)
- **Drop the reduced typed `vm_state::VcpuEvents`.** Rejected: keeping it filled preserves task-39's
  `vm-state` codec output unchanged; the device-blob full record simply supersedes it on restore.

## Known limitations / integrator notes

- **Snapshot points are still V-time-intercept boundaries**, not literal `HLT`: `save_vm_state`
  requires `vtime_synchronized` (the restored TSC resumes from an exact V-time). An interrupt-driven
  workload hits these constantly (every RDTSC), so this is not a practical limit — it is why the
  mid-Postgres seal works. A skid-free cumulative-work read at an arbitrary `HLT` remains a
  `vmm-backend` question, unchanged by this task.
- **RNG mid-exit staged completion** is still refused at save (non-idempotent); step once to a clean
  boundary first. Capturing the staged `complete_userspace_io` value itself is still out of scope (and
  unnecessary for a non-quiescent snapshot — the in-flight *interrupt* is the gap this closes).
- The reduced typed `vm_state::VcpuEvents` is now **vestigial on the full restore path** (the device
  blob is authoritative). A future `vm-state` revision could fold the full record into the typed
  schema and retire it; left in place here to honor rule 1.
- **Gate 2 compares the restored continuation to the live (un-snapshotted) continuation from the same
  seal — the spec's exact wording.** A from-boot run reaches `GUEST_READY` then power-offs via `HLT`
  (terminal); a continuation from the **same mid-workload seal** runs the remaining workload and reaches
  the **same `GUEST_READY` + clean shutdown `HLT`** (box-confirmed: gate 1's restored continuation shows
  `final_row=true GUEST_READY=true`, terminal `Hlt`). The reference is the **live continuation from that
  seal** (not a separate from-boot run) so the V-time origin matches exactly — both share `vns_base` and
  the same retired-work timeline — making the comparison a clean bit-for-bit restore-transparency check
  rather than one muddied by two runs' differing total work. A box-debugging RIP-level live-vs-restored
  trace confirmed **bit-identical RIP traces all the way to the same terminal** (no divergence).
## Restore-transparency on the **full** `state_hash` — the two don't-care fields, canonicalized

Box debugging found the restored mid-Postgres continuation **bit-identical** to the un-snapshotted one
in every execution-relevant component (RAM, GPRs, control regs, descriptor tables, XSAVE, MSRs,
MP-state, V-time, device/serial) **and** its guest-observable output (serial + `observable_digest`),
with the **full `state_hash`** differing in exactly **two architecturally-don't-care fields** that a
KVM `GET → SET → GET` round-trip perturbs. Both are now **canonicalized in the hash** so the full
`state_hash` matches bit-for-bit — a strictly stronger property than "matches in every field but two we
argue don't matter." For each:

**1. The `type` of an *unusable* segment (`encode_segment`).** In the restored vCPU, the unusable data
segments (`ds`/`es`/`fs`/`gs`, with the VMX **unusable** attribute set) read back `type = 1`, where the
live vCPU has `type = 0`. *Why it is don't-care:* a segment whose unusable bit is set is treated as
**absent** — the CPU never consults its hidden descriptor cache (type/limit/attr) on any reference (SDM
Vol. 3 §24.4.1 "Guest Register State": the access-rights of an unusable segment are ignored; §3.4.3 /
the segment-descriptor-cache rules). The 64-bit-relevant part (`fs`/`gs` **base**, used flat) round-trips
**exactly**. The `0 → 1` is purely KVM's `KVM_SET_SREGS` normalization of an inert field. *Fix:* mask the
`type` to `0` when `unusable != 0` in **both** segment encoders — `encode_segment` (the **VCPU** hash
chunk) *and* `pack_segment` (the **VMST** hash chunk, the typed `vm_state` record folded into `state_hash`
when `wire_snapshot_hashing()` is on). The VMST chunk was missed by rounds 2–3 (`encode_segment` only),
so with snapshot-hashing ON a `save → restore → save` still diverged through the VMST chunk even though the
VCPU chunk was canonical (**PR #12 round 4** — codex/GPT-5.5). *Cheap + correct + golden-safe:* every
live-`KVM_GET` value already reports `type = 0` for unusable segments, so masking is a no-op for every
existing golden (M1/M2/det-corpus all still green — `det_corpus_o2_digest_matches_the_observable_golden`
unmoved); the segment **distinguishing** test keys on `cs.base` (a usable segment's base), unaffected.
Pinned by `vmst_chunk_masks_an_unusable_segments_type` (snapshot_branch.rs — two VMs differing only in an
unusable segment's raw `type` hash identically **with the VMST chunk wired**; verified to FAIL without the
`pack_segment` mask) and the adjusted `segment_pack_unpack_round_trips_every_bit` (canonical inputs).

**2. Inert `kvm_vcpu_events` modifier residuals (`encode_events`).** KVM leaves a stale `interrupt.nr`
(the last-delivered vector), a stale `exception.nr`/`has_error_code`, and the GET-only validity-mask
`flags` bits (`VALID_NMI_PENDING|SHADOW|SMM`, reported even with all-zero sub-fields) set after an
injection completes. *Why it is don't-care:* the VM-entry interruption-information and exception fields
are **consumed only when their valid bit is set** (SDM Vol. 3 §24.8.3 "VM-Entry Controls for Event
Injection" / §26.5 "Event Injection") — an injection with `injected = 0` is not delivered, so its
`nr`/`error_code` have no architectural effect; the `flags` validity bits are KVM ioctl metadata, not
guest state. The restore **must** canonicalize these (replaying them raw into `KVM_SET_VCPU_EVENTS`
corrupts the resumed guest — the original box bug), so the restored events legitimately differ from the
live raw residuals. *Fix:* hash the **canonical** form — `encode_events` applies `canonical_events`, so a
restored VM hashes identically to a never-restored one. *Cheap + correct + golden-safe:* `canonical_events`
is a pure function (determinism preserved — two same-seed runs share identical raw events ⇒ identical
canonical), the M1/M2/corpus paths carry **all-zero** events (`canonical == raw`, no change), and the
event **distinguishing** test keys on `nmi_pending` (an *active* field `canonical_events` preserves),
unaffected. **The canonicalization is applied at *every* place the events reach a hash** — not just the
default `state_hash`'s `encode_events`, but also the **typed `vm_state::VcpuEvents` record** (`fill_vcpu_state`
projects `canonical_events(&vcpu.events)`, mirroring the device blob). The typed record rides the opt-in
`VMST` chunk (`wire_snapshot_hashing()`), so without canonicalizing it too, a raw residual would survive a
`save → restore → save` round-trip there and break the full-hash match *with snapshot-hashing on* at a
residual point (PR #12 review). Pinned by `fill_vcpu_state_canonicalizes_the_typed_events_record` (unit)
and `snapshot_hashing_round_trips_at_a_residual_events_point` (end-to-end, snapshot-hashing ON).

**3. SIPI validity derives from the validity bit, never from the vector value (`canonical_events`, PR #12
round 2).** The earlier draft set `VALID_SIPI_VECTOR` (and carried `sipi_vector`) from `sipi_vector != 0`.
That is wrong in two directions: **vector 0 is a legal SIPI** (a `!= 0` test silently drops a genuine
SIPI-to-vector-0), and a **nonzero vector with `VALID_SIPI_VECTOR` clear is a stale residual** (a `!= 0`
test replays it). *Fix:* gate both the carry and the rebuilt bit on the **original** `flags &
VALID_SIPI_VECTOR`. *Correctness, golden-safe by construction:* `KVM_GET_VCPU_EVENTS` zeroes `sipi_vector`
and clears `VALID_SIPI_VECTOR` on every GET (the vector is SET-only — for injecting into a wait-for-SIPI
AP), so for every captured snapshot the value is already 0 and the bit already clear — the fix changes
nothing on the real path; it makes a *synthetic/relayed* events record round-trip faithfully. Our
single-vCPU guest never carries a SIPI. Pinned by the SIPI cases in
`canonical_events_collapses_residuals_and_reconstructs_flags` (residual-with-vector dropped, valid-with-
vector carried, **valid-with-vector-0 kept**).

**4. The restore path canonicalizes too — restore/save symmetry (`restore_vm_state`, PR #12 round 3/6).**
The save path stores `canonical_events` in the device blob (§2); `restore_vm_state` applies the canonical
events before `KVM_SET_VCPU_EVENTS`. For a self-produced (already-canonical) blob this is idempotent —
restore-transparency is unchanged — but an **external or older v3 blob** (hand-built, or from a
different/buggy encoder) could carry RAW residuals; canonicalizing on restore too means a foreign/corrupt
blob can never reintroduce the exact residuals the save path strips. Pinned by
`restore_canonicalizes_raw_events_from_an_external_blob`.

**Round 6 sharpened this into a real determinism leak (`events_for_restore`, codex/GPT-5.5).** KVM treats a
*clear* validity bit on `KVM_SET_VCPU_EVENTS` as **"leave that sub-record UNCHANGED"**, not "clear it". The
active-only mask `canonical_events` builds (a quiescent record → `flags = 0`) is correct for the
**`state_hash`**, but replaying it onto a **non-fresh** vCPU — a committed / previously-run vCPU, i.e. the
**branch or restore-in-place** case — would leave the *prior occupant's* stale NMI-pending /
interrupt-shadow / SMM / triple-fault intact: the restored VM would depend on its predecessor. *Fix:*
`restore_vm_state` uses **`events_for_restore`** (not `canonical_events`), which forces the
`NMI_PENDING | SHADOW | SMM | TRIPLE_FAULT` validity bits **on** with the canonical (zero-when-inactive)
payloads, so KVM explicitly **clears** that state — restore is idempotent w.r.t. target state. `SIPI` stays
gated (SET-only); `PAYLOAD` stays gated on `exception_has_payload` (the exception sub-record is applied by
KVM unconditionally). *Golden-safe:* the **`state_hash` still uses `canonical_events`** (active-only), so no
hashed byte and no M1/M2/det-corpus golden moves (verified — `det_corpus_o2` observable golden unmoved); on
a fresh box target the restored state is byte-identical (gate 2's `66b4d4b4…` is unchanged). Pinned by
`events_for_restore_clears_stale_target_state_regardless_of_freshness` — it models `KVM_SET_VCPU_EVENTS`
semantics, pre-loads a stale vCPU, and proves the restore clears it + equals a fresh-target restore, while
the old `canonical_events` form **leaks** (verified to FAIL without the forced bits).

**5. What gate 2 proves — and what it does *not* yet prove (the honest headline; PR #12 rounds 2–3).**
The task-41 unlock is the **`kvm_vcpu_events` capture**: task 39 fail-closed-**rejected** every point whose
`kvm_vcpu_events` carried in-flight state (its `has_inflight_injection` predicate); task 41 captures the
full record, canonicalizes it, and restores it exactly. The gate therefore **seals on the OLD task-39
rejection predicate** (`has_inflight_event_injection`) — a state task 39 could not represent — and gates
the "0 → N flip" count on it. (An earlier draft sealed on a *genuine in-flight event* including a vector
pending in the LAPIC IRR; that is **not** a task-39 win — task 39 already serialized the LAPIC IRR, so an
IRR-only point is one task 39 would have snapshotted fine. The LAPIC-pending count is kept only as *bonus*
evidence.)

*What gate 2 proves:* a running Postgres snapshotted **mid-workload at a synchronized boundary the OLD
task-39 predicate rejected** restores **bit-identically** on the full `state_hash` — *same state ⇒ same
future, while the system is doing work*. For this live workload those boundaries carry inert
`kvm_vcpu_events` **residuals** (a stale post-delivery `interrupt.nr`), which task 41 captures + canonicalizes
+ restores transparently.

*What gate 2 does **not** yet prove:* that a **genuine in-flight `kvm_vcpu_events` *injection*** (the
literal `interrupt_injected = 1` mid-window-delivery point) round-trips **from the live run** — because
such a point occurs only at a **non-synchronized interrupt-window VM-exit**, where `save_vm_state` fails
closed (the determinism/exactness guard). Measured on the box: **0** genuine `kvm_vcpu_events` injections
at snapshottable boundaries across the whole workload. Per the chosen direction we **deliberately do not
relax the sync guard** (preserving the exact-restore guarantee matters more than the literal phrasing);
making the `interrupt_injected = 1` point itself snapshottable is a **deferred follow-up** (it would
require admitting interrupt-window exits as snapshot boundaries and proving they replay deterministically).

*The genuine-injection capture is instead proven definitively, independent of the live run*, by the
constructed **`task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact`** (snapshot_branch.rs): a
VM with an injected `#GP`-with-error-code **and** an injected NMI (`has_inflight` **and** `has_active` —
not a residual) is snapshotted through the full engine path with the canonical-blob hash wired, restored
into a fresh VM, and its restored **full `state_hash` equals the source's** (and the events round-trip
field-for-field). Task 39 dropped this state (0/N snapshottable); task 41 captures it and restores it
bit-for-bit. The test also asserts the in-flight state **reaches** the hash (it differs from an otherwise
identical quiescent VM), so "identical hash" is a real claim, not a no-op.

**Why this is golden-safe without a re-bless (the key check).** No test pins an **absolute** Linux
`state_hash` value — every `state_hash` golden is **relative**: deterministic-twice (`a == b` across two
same-seed boots: `live_m1_m2`, `live_postgres` p2, `live_linux_boot`, `unison::determinism`) or
distinguishing (`a != b` for a changed input). Canonicalizing a *deterministic* function of the events
leaves every same-seed pair equal and every distinguishing pair (which uses active fields) unequal, so
**no pinned value moves**; the non-Linux M1/M2/corpus paths are byte-identical (all-zero events). The
change therefore satisfies the spec's "re-bless goldens only if a non-Linux path's hash changes — it
should not." Verified on Mac: `vmm-core` (242), `unison`/`det-corpus` determinism (92) all green; and
**on the box** the full-hash gate 2 (below) now passes with `live == restored` bit-for-bit.

## Gates

**Mac (all green):** `build` / `clippy -D warnings` / `fmt` / `nextest` (**242** tests) / `deny`. **Miri**
validates the new `put_events`/`Reader::events`/`canonical_events`/`has_inflight_injection` byte-parsing +
predicates (pure, no new `unsafe` — the granted mmap unsafe is unchanged). **mutants** (`cargo mutants
--no-shuffle --in-diff origin/main...HEAD`, CI's exact invocation) — **0 missed / 0 timeout**.
Exact-value tests pin the new surface: the full in-flight `kvm_vcpu_events` device-blob round-trip (every
field distinct, non-zero), the `has_inflight_injection` 14-field predicate (each field alone flips it), the
`has_active_event_injection` active/residual split (each genuine bit alone fires; each of the 13 residual
modifiers alone does not), `has_pending_guest_interrupt` (quiescent-LAPIC false / IRR-pending true),
`canonical_events` (each SMI/NMI OR-chain operand individually, plus the SIPI validity-bit cases incl.
valid-vector-0), the `encode_segment` unusable-`type` mask — `state_hash_masks_only_an_unusable_segments_type`
pins **both halves** of `if seg.unusable != 0 { 0 } else { seg.type_ }` (killing the `!= -> ==` mutant the
round-1 fix first surfaced) — and **the round-3 restore-symmetry + constructed-unlock tests** (below).
**public-api** — three new lines (`Vmm::has_active_event_injection`, `Vmm::has_inflight_event_injection`,
`Vmm::has_pending_guest_interrupt`), `tests/public-api.txt` matches on the box.

Portable coverage of the mechanism (Mac + Linux): `src/snapshot.rs`
(`device_blob_round_trips_a_full_in_flight_events_record`,
`has_inflight_injection_flags_exactly_the_non_quiescent_fields`,
`has_active_event_injection_flags_only_genuine_injections_not_residuals`,
`canonical_events_collapses_residuals_and_reconstructs_flags`,
**`events_for_restore_clears_stale_target_state_regardless_of_freshness`** — *the round-6 determinism-leak
fix, §4: restore clears a non-fresh target's stale NMI/SMM/shadow/triple-fault; verified to FAIL without the
forced validity bits*; `fill_vcpu_state_canonicalizes_the_typed_events_record`), `src/vmm.rs`
(`save_vm_state_captures_in_flight_events_at_a_non_quiescent_point`,
`restore_canonicalizes_raw_events_from_an_external_blob` — **the round-3 restore/save symmetry, §4**;
`snapshot_restore_re_derives_the_in_flight_lapic_irq`,
`has_inflight_event_injection_reflects_the_live_vcpu`,
`has_active_event_injection_reflects_the_live_vcpu`,
`has_pending_guest_interrupt_reflects_a_pending_lapic_vector`), `tests/event_loop.rs`
(`state_hash_masks_only_an_unusable_segments_type`), `tests/snapshot_branch.rs`
(`non_quiescent_in_flight_events_round_trip_through_the_engine` — the full engine path;
**`task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact`** — *the definitive task-41 unlock
proof, §5: a genuine task-39-rejected in-flight injection → capture → restore → identical full
`state_hash`, independent of the live run*; `snapshot_hashing_round_trips_at_a_residual_events_point` — the
VMST chunk is residual-clean; **`vmst_chunk_masks_an_unusable_segments_type`** — *the round-4 determinism
fix, §1: with the VMST chunk wired, an unusable segment's raw `type` does not move `state_hash` — verified
to FAIL without the `pack_segment` mask*).

**Box (`tests/live_nonquiescent_snapshot.rs`, `#[cfg(target_os="linux")]` + `#[ignore]`):**
- `gate1_nonquiescent_point_is_snapshottable` — scans the post-readiness Postgres workload, **seals at the
  first synchronized boundary whose `kvm_vcpu_events` the OLD task-39 predicate rejected**
  (`has_inflight_event_injection` — the state task 39 could not represent), and quotes the **before/after
  split on the same run**: `task39_rejected` points (the `0 → N` flip, gated on the OLD predicate), of
  which `genuine_inflight` also carried a genuine in-flight *event* (LAPIC-IRR-pending / kve injection — see §5),
  plus quiescent. Asserts `task39_rejected > 0` **and** (the scan reaches the workload's terminal)
  `genuine_inflight >= 1` — so the gate cannot "pass" on residual canonicalization alone; the V-time
  **deterministic** workload genuinely reaches ≥1 in-flight event every run (PR #12 round 4). Then confirms
  `restore_vm_state` produces a **runnable** VM that **cleanly completes the workload**
  (`internally_consistent`: final row + `GUEST_READY` + a real terminal + no step error). *The
  genuine-injection capture→exact-restore is proven by the constructed test (§5), not the live seal — see
  "what gate 2 does not prove."*
- `gate2_mid_postgres_roundtrip_is_deterministic` — **the milestone:** a running Postgres is snapshotted
  **mid-workload at a task-39-rejected non-quiescent point**; the **un-snapshotted (live) continuation**
  from that seal is the reference (the spec's "un-snapshotted run"). The snapshot is restored into a fresh
  VM and resumed **twice** → deterministic-twice, and each restored continuation is **bit-identical** to the
  live continuation on the **full `state_hash`** + serial + `observable_digest`. **Both the live and each
  restored continuation must be `internally_consistent`** (final row + `GUEST_READY` + real terminal +
  no step error) — so the milestone cannot "pass" by comparing a shared *failed* prefix of two runs that
  both broke the same way (a step-error / wall-budget break leaves `reason == None`; PR #12 review).
  **Also calls `assert_run_reaches_genuine_inflight`** (PR #12 round 5): the seal lands at an inert residual
  for this workload, so a fresh full boot-to-terminal scan separately proves the live run reaches **≥1
  genuine in-flight point** (`genuine_inflight >= 1`) — this headline gate is not residual-only. Restore is
  exact at a non-quiescent point — *same state ⇒ same future* (see §5 for exactly what this proves, and what
  is a deferred follow-up).
- `gate3_branching_from_a_mid_postgres_snapshot` — re-runs task 40's matrix sealed at a **mid-Postgres**
  point: each seeded fork reproducible across N replays, ≥1 divergent (a reseeded entropy stream makes
  the terminal `state_hash` distinct), one shared read-only base. The base continuation must be
  `internally_consistent`; each branch must reach a **clean shutdown** (`GUEST_READY` + real terminal +
  no step error) *without* pinning the workload row — a branch is allowed to diverge into a different
  guest-observable future. Also calls `assert_run_reaches_genuine_inflight` (PR #12 round 5 — the
  branching headline gate is not residual-only either).

`live_branching_demo.rs` (task 40) is left intact as the boot-entry baseline, with a doc note pointing
to the task-41 gate for the mid-workload capability.

Run pinned per `docs/BOX-PINNING.md` (the box briefing assigns task 41 **core 4**; task 38 owns core
2, CI owns 5–7/13–15), patched modules loaded, **reverted to stock after** (`lsmod | grep '^kvm '`
must read `1396736`; check `lsmod` **first** to coordinate with task 38 — never revert while another
patched run is live):

```sh
make -C guest fetch && make -C guest/linux postgres-image
# load patched kvm.ko/kvm-intel.ko, then (core 4):
taskset -c 4 timeout 3600 cargo test -p vmm-core --test live_nonquiescent_snapshot \
    -- --ignored --nocapture --test-threads=1 --exact <gateN_...>
# revert to stock + verify lsmod kvm == 1396736
```

> **Box-run results (2026-06-26, patched KVM `kvm 1400832`, `taskset -c 4`, reverted to stock
> `kvm 1396736` after each run — `lsmod` checked first to coordinate with task 38, which was idle).**
> Self-served via git (push branch → `/root/ht41` checkout → run), per the box briefing.
>
> - **Gate 2 (the milestone) ✓ — clean FULL-`state_hash` match.** Postgres snapshotted **mid-workload at
>   step 154221** (right after `database system is ready to accept connections`, a non-quiescent point).
>   The restored continuation is **deterministic-twice** (two restores reach a bit-identical terminal) and
>   **bit-identical** to the un-snapshotted (live) continuation on the **full `state_hash`**, serial, and
>   `observable_digest`:
>   `live = restored = 66b4d4b4a7b189606ced32568c9ed7292d259912a6ee48b4b98e157d85884164`.
>   (With the `encode_segment`/`encode_events` canonicalization of the two don't-care fields above; before
>   it, the sole `state_hash` delta was the inert `events` residuals — segments matched at the terminal,
>   and the `vtim:last-intercept`/`vtim:work-raw` pair is diagnostic-only, not in the hash.) The Mac
>   determinism suites (`unison`/`det-corpus`, 92) confirm the canonicalization preserves determinism.
>   Re-run with the **PR-#12 clean-continuation assertions** (the milestone gate now requires both the live
>   and each restored continuation to be `internally_consistent` — `final_row=true GUEST_READY=true
>   step_error=None`, observed for all three): still **`live = restored` full-hash match**, `test result:
>   ok`. **Same state ⇒ same future — while the system is doing work.**
> - **Gate 1 (0→N flip, gated on the OLD task-39 predicate) ✓** — on one Postgres run, **3112 of the
>   post-readiness V-time-sync boundaries carried `kvm_vcpu_events` in-flight state the OLD task-39
>   predicate (`has_inflight_event_injection`) fail-closed-rejected**; task 41 makes **all 3112
>   snapshottable**, sealing at the first such point, and the restore resumes into a runnable VM that runs
>   the workload to its **final row and `GUEST_READY`** (clean `Hlt` power-off terminal). (Reproduces task
>   40's `0 of 8392` and flips the 3112.) *Honest note (PR #12 round 3):* these captured states are KVM
>   **modifier residuals** (a stale `interrupt.nr`, the GET-only validity flags) — `canonical_events`
>   collapses them, which is what makes the restore sound. Of the 3112, **1 also carried a genuine
>   in-flight *event*** (a vector pending in the LAPIC IRR — bonus evidence; the LAPIC IRR was already
>   serialized by task 39, so it is not itself the unlock). A **genuine `kvm_vcpu_events` *injection***
>   (`interrupt_injected=1`) lands only at a non-synchronized interrupt-window exit and so is **0** at
>   snapshottable boundaries — its capture→exact-restore is proven by the constructed
>   `task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact` (§5), not the live seal. (PR #12
>   round 5 re-run, commit `22cb9f3`: sealed at the first task-39-rejected point, step 154221;
>   `task39_rejected=3112`, `genuine_inflight=1` (the V-time-deterministic workload reaches exactly one
>   LAPIC-IRR-pending point every run). **All three gates now enforce `genuine_inflight >= 1`** — gate 1
>   over its own scan, gates 2/3 via the standalone `assert_run_reaches_genuine_inflight` full-run presence
>   check (`genuine-presence check ✓ — the run reached 1 genuine in-flight point(s) over 162609 steps`), so
>   no headline gate is residual-only. gate 1/2/3 all ✓, full-hash `66b4d4b4…85884164`, KVM reverted to
>   stock `1396736`.)
> - **Gate 3 (mid-Postgres branching matrix) ✓** — sealed at the **mid-Postgres** non-quiescent point
>   (the matrix task 40 could only seal at boot entry); base continuation + 3 entropy-fork branches,
>   each replayed twice: **every fork reproducible** across its replays, **all four terminal digests
>   distinct** (base `0d104755…`, branches `2f534b0e…`/`2e968027…`/`187cdba5…`) → ≥1 divergent, and the
>   forks **share one read-only base** (no unique pages added). Each fork runs the remaining workload to
>   `GUEST_READY`. (The seed-fork divergence here surfaces in the `vtim:entropy` bookkeeping — the
>   workload after this seal does not re-derive guest-observable state from the forked stream — exactly
>   the trade-off `live_branching_demo.rs` documents for a post-CRNG-seed seal.)
> - **Restore fidelity (measured during box debugging) ✓** — a RIP-level live-vs-restored trace showed
>   **identical RIP traces to terminal, no divergence**; a component-level comparison showed the only
>   state the restore changes is the benign `{segments (unusable-segment `type`), events (canonical)}`.
>   (These diagnostics were temporary scaffolding and are not shipped; gate 2 asserts the resulting
>   property.)
> - **Hygiene ✓** — patched `kvm 1400832` loaded per run, reverted to stock `kvm 1396736` after each
>   (verified via `lsmod`), `kvm_intel` users checked `== 0` before loading (task 38 idle).
> - **Standard gates on the box ✓** — `clippy -D warnings` (the box-only test file included), `fmt`,
>   `nextest` (242 non-ignored), and **public-api** (`tests/public-api.txt` matches
>   `cargo +nightly public-api` exactly — the three new lines `Vmm::has_active_event_injection`,
>   `Vmm::has_inflight_event_injection`, `Vmm::has_pending_guest_interrupt`).
