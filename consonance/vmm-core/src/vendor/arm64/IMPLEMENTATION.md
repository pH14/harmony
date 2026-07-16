# tasks/112 — ARM backend skeleton behind the seam (`hm-cbt`)

The first real **second vendor** behind the `Arch`/`Backend`/`Vendor` seam
`tasks/108` landed: a KVM/arm64 backend, an `Image`+DTB boot path, GICv3 +
generic-timer models, and the `Arm64` vendor behind the engine — **additive
code, built now, trusted only once the Altra spike (`docs/ARM-ALTRA.md`)
returns GO** (the 2026-07-13 pre-build ruling; the sunk-cost risk is accepted).

All Mac-local gates are green; the real-KVM boot + `state_hash` determinism
gates are **specified and edged to `hm-7pb`** (the Altra) — there is no local
KVM loop (`hm-8l3` REFUSE).

## What landed, by milestone

- **M0 — the snapshot-state seam (the one sanctioned spine edit), x86-only.**
  `vm-state` gains an arch-neutral `SnapshotRecords` codec trait (`ARCH_TAG` +
  `encode`/`decode` + the neutral engine blocks `vtime`/`timers`/
  `entropy_bytes`); `VmState` implements it under `ARCH_X86_64`; `ARCH_AARCH64
  = 2` is reserved. `Vendor` gains `type Snapshot: SnapshotRecords`, and the
  three snapshot hooks re-type against it. The engine glue (`Vmm::{save,restore}
  _vm_state`, `restore_snapshot`, `SnapshotEngine::vm_state`) is generic over
  `<B::A as Vendor>::Snapshot`; the engine reads only the neutral blocks through
  the trait accessors. x86 is the sole implementation, and **every existing x86
  test passes unchanged** (the neutrality proof).
- **M1 — the `Arm64` vendor skeleton (the keystone).** `Arm64` (ZST) implements
  `Arch` + `Vendor`; `MockArm64Backend` drives `Vendor::dispatch_arch` + a
  snapshot build→seal→decode→restore round-trip that is `state_hash`-transparent.
  This is the structural check no cross-compile gate can perform.
- **M2 — the interrupt fabric + generic timer.** A new `consonance/gicv3` crate
  (the `lapic` sibling) models a GICv3 distributor/redistributor + the EL1
  virtual timer in the pure `now_vns`-in / deadlines-out shape; wired into
  `Arm64Devices` via `Vmm::wire_gic`. Arbitration + deadlines are complete and
  property-tested; **delivery into a real guest is offline** (`TODO(AA-6)`).
- **M3 — the boot path.** `image_loader` (arm64 `Image` header, total over
  untrusted bytes), `dtb` (a hand-rolled FDT writer + a round-trip reader),
  `entry` (`PC`/`x0=dtb`/`PSTATE=EL1h+DAIF`), `board` (the machine memory map),
  `bringup::{boot,compose}`, `hostassert`, and PL011 MMIO routing. Portable
  tests + a **TCG smoke verified on real QEMU** (`HARMONY-ARM64-BOOT`, clean
  PSCI poweroff).
- **M4 — the stock KVM/arm64 backend + composition root.** `arm64_kvm` (pure:
  the `KVM_RUN`⇄`Exit` decode, the register-ID save/restore table, and
  `Arm64KvmBackend<K>` over the `Arm64Kvm` syscall seam, all portable +
  Miri-tested against `FakeKvm`) and `arm64_kvm_sys` (box-only `LiveKvm`, real
  ioctls). `vendor::arm64::bringup::boot_selected` names the
  `(Arm64KvmBackend<LiveKvm>, Arm64)` pair, Linux+aarch64-gated.

## The sanctioned spine edits — the §D exception, and why each is not scope creep

`docs/ARCH-BOUNDARY.md` §D rules "zero edits to the neutral spine" with **one
ruled exception**: the snapshot-state seam (PR #109). Every non-additive edge
below is that exception or a mechanical consequence of it; nothing else touches
the engine, and no `<A: Arch>` parameter appears in any dissonance crate.

1. **`vm-state`: the `SnapshotRecords` trait + `ARCH_AARCH64` tag + the
   `Arm64VmState` record set.** This *is* the §D exception. `Vendor`'s snapshot
   hooks were typed against the concrete `VmState` (x86 records); a second
   vendor cannot implement them without this trait change. The wire format was
   already extensible (step 4's v2 arch tag); only the Rust type seam was
   pinned. A foreign arch tag is rejected loudly (`UnsupportedArch`), both ways.
2. **`vmm-core/src/vendor/mod.rs`: `type Snapshot` on `Vendor`, and the three
   hooks re-typed.** The trait-level half of the same exception.
3. **`vmm-core/src/vmm.rs` + `snapshot.rs` + one line in `control.rs`: the
   engine glue made generic over `Vendor::Snapshot`.** Mechanical: the engine
   now holds/seals `<B::A as Vendor>::Snapshot` through `SnapshotRecords` and
   reads only the neutral blocks (`vtime()`/`timers()`/`entropy_bytes()`) — it
   still never names a register record. This is the *engine glue* the §D
   exception explicitly sanctions ("seals encoded bytes — already opaque").
4. **`vmm-core/src/vendor/x86/mod.rs`: `type Snapshot = VmState;`.** Mechanical,
   zero behavior change.

Not touched (confirmed): `vmm.rs`'s run loop, `control.rs` beyond the one
accessor line, `corpus.rs`, `work.rs`, `vtime`, and every dissonance crate.

**One additive roster growth, pre-authorized:** `vmm-backend`'s `ExitReason`/
`ExitCounts` gain an appended `Sysreg` entry (`entries()` 13 → 14). `exit.rs`
already documents this as the ARM window's additive evolution ("this roster
gains vendor variants additively when a new vendor lands"); the pre-arm64
prefix and every existing report line are byte-unchanged.

## designed-not-frozen — which AA stage may force rework

The trait is **designed, not frozen** (`AA-3`'s trait-freeze memo owns the
freeze). Seams that carry rework risk:

- **`Backend::run_until` (arm64) = `Unsupported`, and the whole `Arm64Kvm`
  decode of a WFx/overflow exit — `AA-3`.** arm64's PMU-overflow-to-exit physics
  may pressure `run_until`'s late-only-stop contract; the 0004/0005-analogue
  patch + the patched backend are a later bead. The patched decode arms
  (`WFx→Idle`, `HVC→Hypercall`, `sysreg→Sysreg`) exist so that backend drops in
  without reshaping the decode.
- **`Arm64VmState` / `Arm64VcpuState` / `Arm64SysregFile` — `AA-6`.** Which
  sysregs a snapshot must carry is the spike's *measured* decision; the current
  set is the minimal round-trippable subset (`TODO(AA-6)`).
- **`Arm64Policy` (`IdRegModel` + `SysregTrapPolicy`) — `AA-6` + patched-abi.**
  The row set is `AA-6`'s enforcement-mechanism truth table; the trap-group
  *enforcement* (a denied sysreg → a userspace exit) is the AA-3 patched
  backend's. The stock backend installs the `ID_AA64*` freeze config-time
  (works now) and records the trap-table shape only.
- **GICv3 delivery into a guest — `AA-6`.** The arbitration/deadline model is
  complete; whether delivery uses the in-kernel vGICv3 (its bit-identical
  round-trip is AA-6's open question) or a patched userspace-injection seam is
  the spike's verdict. Every skeleton path fails closed on delivery.
- **The MP-state mapping (`Halted`↔`STOPPED`) — `AA-6`.** A skeleton mapping.

## Spike-measured quantities — every one a `TODO(AA-N)`, never invented

- `RAW_BR_RETIRED = 0x21` is a **documented hardware fact** (Arm PMU event
  enumeration), cited, not measured. Every count offset / density / `skid_margin`
  derived from it is `TODO(AA-1)`. `SimCpu`/`PlannerConfig` stay
  x86-parameterized (untouched).
- The board's device addresses and `CNTFRQ_HZ` are **composition choices**
  (like x86's `LAPIC_TIMER_HZ`), documented as such — not measured constants.
- The contract rows, the snapshot record set, the vGIC verdict, the MP-state
  contract: all `TODO(AA-6)`.
- No default is ever inherited from x86; no placeholder number could be mistaken
  for a measurement.

## Deviations considered and rejected

- **A "dummy vendor" to force the structural check.** Rejected as redundant
  (`docs/ARCH-BOUNDARY.md` §D) — `hm-cbt` supplies a real second vendor.
- **Wiring the userspace GICv3 in the stock boot root.** Rejected: the stock
  backend's `set_pending_irq` is `Unsupported`, so a wired fabric would error at
  the first `service_pending_irqs`. The boot root leaves it unwired
  (stock-safe); the DTB still advertises the GIC so a guest can program it.
- **A vetted FDT crate for the DTB.** Deferred to an ask-by-comment (judgment
  call #2): the hand-rolled writer matches the x86 hand-built-boot-struct
  precedent and stays inside the dependency whitelist. It is round-trip-tested
  and QEMU-accepted.
- **Reusing `kvm-ioctls`' `VcpuExit` decode in `LiveKvm`.** Rejected: its
  MMIO-read buffer is a transient borrow, incompatible with the deferred
  `complete_read` model. `LiveKvm` retains a raw mmap'd `kvm_run` (like x86's
  `KvmBackend`) so the pure `decode_exit` stays the single source of truth.

## Known limitations

- **No silicon claims.** Nothing boots on real KVM here; the TCG smoke is
  QEMU's own VMM (not `Arm64KvmBackend`) and proves the *artifacts* boot,
  liveness/shape only — never counts/PMIs/skid.
- **`LiveKvm` has no local oracle.** It compiles (the aarch64-linux
  cross-check) but never runs locally; its runtime correctness is arrival-day
  (`hm-7pb`). Its shape (ioctl ordering, the reg-ID set, the exit decode) is
  asserted portably via `FakeKvm`.
- **The public-api goldens are x86_64-linux-generated.** `LiveKvm`
  (`cfg(all(linux, aarch64))`) is therefore **absent** from the committed
  golden — the CI `public-api` job runs on the x86_64 box and won't see it
  either, so the two agree. This is the surface-list row's acknowledged
  Linux-frozen limitation (the `hm-rk5`/PR #110 precedent). No single target
  captures both the x86 KVM surface and `LiveKvm`.
- **No paravirt clock-page protocol.** The skeleton reserves a named DTB
  `reserved-memory` region and routes the generic-timer *deadline* through the
  fabric; the seqlock page protocol is `hm-rk5`'s.

## Judgment calls (for the foreman)

1. **The engine/vendor *crate* split (non-goal 6) is deferred** to its own
   follow-on, per the spec — landed as additive modules in the existing crates
   plus the one new `gicv3` crate, so no in-flight branch pays a mid-skeleton
   rebase.
2. **The DTB is hand-rolled** (above) — a vetted crate is an ask-by-comment.
3. **`consonance/vmm-core/src/vendor/arm64/board.rs` exposes the machine memory
   map as `pub` constants** (`RAM_BASE`, the device frames, `CNTFRQ_HZ`, …) plus
   `gic_config()`/`new_gic()`. The board is the machine's public description
   (the DTB, the MMIO dispatch, and the TCG-smoke tooling all reference it); the
   golden growth is additive.

## `nightly.yml` (Miri) — no change required

The surface-list row is satisfied without an edit. The one **new** crate,
`gicv3`, is pure `no_std` logic with **no `unsafe`** — so, exactly like `lapic`,
it is deliberately excluded from the per-crate Miri jobs (Miri's bar is the
unsafe crates). The new `unsafe` this task adds lives in `vmm-backend`
(`arm64_kvm`'s `map_memory` forward — Miri-exercised via `FakeKvm`; `LiveKvm`'s
raw ioctls — box-only, un-Miri-able like x86's `kvm_sys`), and `vmm-backend` is
**already** in the `nightly.yml` Miri list, so the coverage is in place.

## Box gates — specified, edged to `hm-7pb` (arrival-day)

On the Altra, over `ARM_BOX_SSH` (the `DET_BOX_SSH` convention extended; the
repo hard-codes no host): a real `KVM_RUN` boots the `Image`+DTB path
(`boot_selected`) to a console marker, and a same-seed pair holds a
bit-identical `state_hash`. Every count/PMI/skid claim they could make is the
spike's (`docs/ARM-ALTRA.md` AA-1/AA-3), never this task's. The **M0
x86-neutrality box gate** (the existing x86 determinism box, `DET_BOX_SSH`) is
BLOCKING-and-runnable-today; it is handed to the foreman for the merge (the M0
spine edit's default paths are byte-identical, but a live-KVM snapshot
save/restore regression the mock cannot see must not merge on Mac greens).

## Surface touched (the frontier boundary)

`vm-state/src/{records,arm64}.rs` + `lib.rs`/`codec.rs` (pub(crate) codec
primitives shared) + `tests/public-api.txt`; `vmm-backend/src/arch/arm64/`,
`mock_arm64.rs`, `arm64_kvm.rs`, `arm64_kvm_sys.rs`, `exit.rs` (roster),
`lib.rs`, `arch/mod.rs`, `tests/{exhaustive,public-api}`; `vmm-core/src/vendor/
arm64/` (this dir), `vendor/{mod,x86/mod}.rs`, `vmm.rs`/`snapshot.rs`/
`control.rs` (the sanctioned glue), `Cargo.toml` (gicv3 dep + tempfile dev-dep),
`tests/{arm64_skeleton,arm64_tcg_smoke,public-api}`; the new `consonance/gicv3/`
crate; `.github/workflows/quality.yml` (gicv3 → the public-api job).
