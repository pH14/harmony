# Task 112 — ARM backend skeleton behind the seam

**Bead:** `hm-cbt` (P2, the §D pre-build lane). **Dispatch authority:** the pre-build ruling
(Paul, 2026-07-13; `docs/ARCH-BOUNDARY.md` §Pre-build ruling), queue lane 5 — "the §D reversal
proper," `← hm-b5n`. **Class:** frontier (it touches the seam and the box substrate — its
surface list below stands in place of the single-directory rule, per
`tasks/00-CONVENTIONS.md` §Task classes), but **arrival-gated**: every runnable gate is
Mac-local, and the real-KVM gates are edged to the Altra window (`hm-7pb`).

This is the first real **second vendor** behind the `Arch`/`Backend`/`Vendor` seam that
`tasks/108` (`hm-b5n`) landed. It is the additive ARM wave of `docs/ARCH-BOUNDARY.md` §D:
a KVM/arm64 backend, an `Image`+DTB boot path, GICv3 + generic-timer device models, and the
ARM vendor behind the engine — built now, **trusted only once the Altra spike (`docs/ARM-ALTRA.md`)
returns GO**. The pre-build ruling accepts the sunk-cost risk: if AA-1/AA-3 NO-GO, this
ARM-specific slice is discarded; the seam restructure it exercises pays for itself on x86
regardless.

## Read first, in full

- `docs/ARCH-BOUNDARY.md` — the whole seam ruling. **§A** (the `Arch` trait + generic
  `Backend` + the two-level `Exit<A>`), **§B** (engine/vendor split; the ARM row of the
  responsibility table is this task's checklist), **§D** (the additive ARM wave + the *one
  ruled exception*, the snapshot-state seam), and **§Pre-build ruling** (build-first; the spike
  gates trust, not construction; rework-against-the-unfrozen-trait accepted).
- The `hm-cbt` bead (`bd show hm-cbt`) and its 2026-07-15 comment — the ruled D-list scope and
  the dev-loop verdict.
- `docs/ARM-ALTRA.md` — the AA-0..AA-6 spike program this skeleton must **not front-run**.
  Every measured constant (`skid_margin`, event density, count offsets) is a spike deliverable;
  the trait *freeze* is AA-3's; the ARM snapshot record set is AA-6's. `BR_RETIRED` (raw
  `0x21`, retired *taken* branches) is a documented hardware fact, not a measured constant.
- `docs/PARAVIRT-CLOCK.md` §3.2 / §4.2 / §8 — the arm64 clock page the guest will *eventually*
  use (`hm-rk5`/`hm-2kj` own it). **This skeleton only reserves the seam** — a named DTB region
  and a device-model deadline path — it implements no clock-page protocol.
- `docs/ARM-PORT.md` — hardware facts, the three load-bearing mechanisms, the LL/SC vs LSE
  hazard, and the viability gate's authority (still binding; still the arbiter of *trust*).
- `consonance/vmm-backend/src/arch/x86/` and `consonance/vmm-core/src/vendor/x86/` — the shapes
  the ARM analogues mirror (value types, the `Vendor` impl, dispatch, bringup composition root).
- `tasks/00-CONVENTIONS.md` — gates, style, determinism discipline, the `unsafe`⇒Miri bar.
- `tasks/109-arm-prebuild-apparatus.md` (`hm-2kj`) — the sibling offline apparatus under
  `spikes/arm-altra/`. **No file overlap:** that is spike apparatus (untested-on-silicon,
  non-workspace); this is production code behind the seam. They do not depend on each other.

## Goal

Land the ARM vendor as **additive code behind the existing seam**, compiling green against the
arch-neutral engine, so that Altra arrival day is *measure + validate + freeze*, not *build*.
The terminal state: an `Arm64` vendor whose `Vendor` and `Backend` impls the engine drives
through the same generic types it drives x86 through; a GICv3 + generic-timer fabric in the
ruled pure `now_vns`-in / deadlines-out shape; an `Image`+DTB boot path; all Mac-local gates
green; the real-KVM boot and determinism gates specified and edged to `hm-7pb`.

The **keystone deliverable** is structural, and it is why this task exists beyond the code it
writes: being the first real second implementor of `Vendor`, it is the only thing that can
prove the rest of the seam is genuinely additive in a way no cross-compile gate can
(`docs/ARCH-BOUNDARY.md` §D, "the structural check is the ARM skeleton itself"). A signature
only a second vendor could refute stays invisible until this vendor instantiates the trait.

## Non-goals (explicit)

1. **No silicon claims.** This task boots nothing on real KVM and measures nothing. It produces
   no dispositions, no evidence manifests, no `skid_margin`, no density table. Those are the
   Altra spike's (`docs/ARM-ALTRA.md`). TCG proves liveness/shape only — never counts, PMIs, or
   skid.
2. **No invented constants.** `BR_RETIRED = 0x21` is documented (cite it). Everything the spike
   measures is a **named TODO bound to its AA stage**, never a default, never inherited from
   x86. `SimCpu`/`PlannerConfig` stay x86-parameterized until the AA constants pack exists.
3. **No exact-landing path.** `Backend::run_until` on arm64 returns
   `Unsupported` in the skeleton (exactly as the x86 stock `KvmBackend` does). The deterministic
   force-exit + single-step landing is the 0004-analogue kernel patch (AA-3) plus the patched
   backend — a later bead, not this one.
4. **No clock-page protocol.** The paravirt work-derived clock (`docs/PARAVIRT-CLOCK.md`) is
   `hm-rk5`'s. The skeleton reserves a DTB region and routes the generic-timer *deadline*
   through the existing `TimerQueue`/idle seams; it implements no seqlock page.
5. **No full CPU contract.** The ARM CPU-contract *document* and its enforcement truth table are
   port work / AA-6. The skeleton installs a **default-deny policy skeleton** (a synthetic
   `ID_AA64*` freeze shape + a trapped-sysreg table shape) with the concrete row set a TODO
   bound to AA-6; it does not claim enforcement completeness.
6. **No engine/vendor *crate* split.** `docs/ARCH-BOUNDARY.md` §B rules "module split first,
   crate split when ARM lands," but immediately qualifies it as churn with "no gate-visible
   payoff" that "falls out along the already-drawn module lines." The skeleton lands as
   **additive modules** in the existing crates plus one new device crate (see §Surface list);
   the mechanical crate extraction is deferred to its own follow-on so it does not force a rebase
   on every in-flight branch mid-skeleton. (Flagged for the foreman — judgment call #1.)
7. **No dissonance edits.** The only two the port ever required — the `InjectInterrupt` `u8→u32`
   widening and the `CrashKind` portable rename — **already landed** with the C-list in
   `tasks/108`. A generic `<A: Arch>` parameter appearing in a dissonance crate remains a
   review-blocking smell.

## The constraints (ruled, not negotiable)

These are the `hm-cbt` / §Pre-build constraints, encoded so the implementer cannot drift:

1. **Additive only; one sanctioned spine edit.** Zero edits to the arch-neutral engine or any
   dissonance crate — **except** the ruled snapshot-state seam (`docs/ARCH-BOUNDARY.md` §D
   exception, PR #109): `Vendor`'s three snapshot hooks are typed against the concrete
   `vm_state::VmState` (x86 records), so a second vendor cannot implement them without a trait
   change. That change (M0) is *accepted, not an escape hatch*. Any other spine touch is a bug.
2. **The trait is designed, NOT frozen.** AA-3's trait-freeze memo may force rework of
   `run_until`'s late-only-stop contract once arm64 PMI-delivery physics is known
   (`docs/ARM-ALTRA.md` §3, AA-3). The skeleton is written *against the unfrozen trait*, and the
   pre-build ruling **accepts that rework**. Do not treat "compiles for arm64" as "frozen for
   arm64." Every seam this task adds carries a one-line `designed-not-frozen (AA-3)` note where a
   freeze would otherwise be assumed.
3. **No invented constants** (non-goal 2, restated as a hard rule): anything the spike measures
   is a `// TODO(AA-N):` named against its stage, never a placeholder number that could be
   mistaken for a measurement.
4. **Gates are TCG-first** (§Gates): portable tests + clippy run natively — the Mac *is*
   aarch64, so all pure logic is a first-class target, not a cross-build. TCG (`qemu-system-
   aarch64`) is the local oracle for boot/ioctl *shape*. Miri covers every `unsafe` with an
   allocation-backed seam (PR #99 precedent; the PR #108 payload carve-out for genuinely
   uninterpretable byte regions). **Box/KVM gates are arrival-day, edged to `hm-7pb`.** There is
   no local KVM ioctl dev loop (the dev-loop probe `hm-8l3` returned REFUSE: this Mac is an M1
   Max, pre-M3, so Virtualization.framework nested virt is unavailable and an aarch64 Linux guest
   cannot expose `/dev/kvm`).
5. **Milestone so the keystone lands before devices** (§Milestones): the snapshot-seam change
   (M0) and the `Vendor` skeleton compiling against the engine (M1) come **before** any device
   model (M2+). Each milestone is independently green — a reviewer can stop after any one and the
   tree builds and passes.

## Surface list (the frontier boundary in place of the single-directory rule)

Additive unless marked. Nothing else may be touched.

| Path | Change | Kind |
|---|---|---|
| `consonance/vmm-backend/src/arch/arm64/` | new: `Arm64` `Arch` impl + value types (`Arm64Exit`, `GicIntId`, `Arm64Policy`, `Arm64Caps`, `Arm64Completion`, the vCPU record set) | additive module |
| `consonance/vmm-backend/src/arch/mod.rs` | add `pub mod arm64;` | additive line |
| `consonance/vmm-backend/src/arm64_kvm.rs` (+ `arm64_kvm/`) | new: the `Backend` impl over KVM/arm64 (Linux+aarch64-gated) and a portable `MockArm64Backend` | additive module |
| `consonance/vmm-backend/src/lib.rs` | add the `arm64` re-exports (mirroring the `X86*` re-exports) | additive lines |
| `consonance/vmm-core/src/vendor/arm64/` | new: `Vendor for Arm64` + `dispatch`, `contract`, `devices`, `image_loader`, `dtb`, `entry`, `hostassert`, `records`, `work_perf`, `bringup` | additive module |
| `consonance/vmm-core/src/vendor/mod.rs` | add `pub mod arm64;` **and the `Vendor::Snapshot` seam change** | additive line + **sanctioned spine edit** |
| `consonance/vmm-core/src/vendor/x86/mod.rs` | mechanical: `type Snapshot = VmState;` (no behavior change) | mechanical |
| `consonance/vmm-core/src/snapshot.rs` | the engine snapshot glue made generic over `Vendor::Snapshot` (seals encoded bytes — already opaque) | **sanctioned spine edit** |
| `consonance/vm-state/src/` | add `ARCH_AARCH64 = 2`, a `SnapshotRecords` codec trait (arch-neutral), and a **minimal** `Arm64VmState` record set (full sysreg set TODO→AA-6) | **sanctioned spine edit** |
| `consonance/gicv3/` | new crate: GICv3 distributor/redistributor + generic-timer model (the ARM analogue of the `lapic` crate) | additive crate |
| `consonance/{vm-state,vmm-backend,vmm-core}/tests/public-api.txt` | regenerate: M0/M1 add public API (`SnapshotRecords`, `Vendor::Snapshot`, the `Arm64*` types + re-exports), so the `cargo public-api` goldens the `quality.yml` `public-api` job diffs must be updated — **Linux-frozen** (regenerated on the box: the KVM-gated `Arm64KvmBackend` surface only appears on the aarch64-linux leg, so a Mac-regenerated snapshot would drop it; the `hm-rk5`/PR #110 precedent) | golden update |
| `consonance/gicv3/tests/{public-api.txt,public_api.rs}` | new: the new public crate joins the `public-api` gate, mirroring `lapic/tests/` (a new public crate silently outside the frozen-API gate is the smell this row closes) | additive |
| `.github/workflows/nightly.yml` | add the new/`unsafe`-bearing crates to the per-crate Miri jobs — **Miri lives here, not `quality.yml`** (moved 2026-06-24 for OOM/timeout headroom); pinned `nightly-2026-06-16` + `MIRIFLAGS=-Zmiri-permissive-provenance` | additive lines |

The sanctioned spine edits (the `Vendor::Snapshot` associated type, its engine glue, and the
`vm-state` arm64 record set) are the §D exception and **nothing more**. If the implementer finds
themselves editing `vmm.rs`'s run loop, `control.rs`, `corpus.rs`, `work.rs`, `vtime`, or any
dissonance crate, they have left the seam — stop and escalate.

## Milestones

Each milestone: deliverable · public-API sketch (grounded in the real trait shapes) · gate. Do
them in order; each is independently green.

### M0 — the snapshot-state seam change (the one sanctioned spine edit), x86-only

**Why first.** `Vendor::build_vm_state`/`validate_restore`/`commit_restore` are typed against the
concrete `vm_state::VmState`, whose `regs`/`sregs`/`xsave` records are x86-64's. An arm64 vendor
cannot implement them as-is. This milestone makes the seam vendor-associable **with zero arm64
code**, so the spine risk is isolated and proven x86-neutral before the ARM work begins — the
same discipline `tasks/108` step 3 used (x86 as sole implementation, every gate passing unchanged
through the change).

**Deliverable.** Introduce an arch-neutral snapshot codec seam and route the engine through it:

```rust
// vm-state: an arch-neutral canonical codec the engine seals bytes through (it already
// "seals encoded bytes and never reads a record" — ARCH-BOUNDARY §D). Both vendors' record
// sets implement it; the arch tag rides in the v2 container header that step 4 already added.
pub trait SnapshotRecords: Sized {
    const ARCH_TAG: u16;                              // ARCH_X86_64 = 1; ARCH_AARCH64 = 2
    fn encode(&self) -> Vec<u8>;                      // canonical, byte-deterministic
    fn decode(bytes: &[u8]) -> Result<Self, VmStateError>;  // total; UnsupportedArch on tag mismatch
}
impl SnapshotRecords for VmState { const ARCH_TAG: u16 = ARCH_X86_64; /* existing codec */ }

// vmm-core Vendor: the three hooks re-typed against an associated Snapshot (default-free).
pub trait Vendor: Arch + Sized {
    type Snapshot: vm_state::SnapshotRecords;
    // designed-not-frozen (AA-6 owns the arm64 record set; this seam is the extension point)
    fn build_vm_state<B: Backend<A = Self>>(vmm: &Vmm<B>, vcpu: &Self::VcpuState) -> Self::Snapshot;
    fn validate_restore<B: Backend<A = Self>>(vmm: &Vmm<B>, s: &Self::Snapshot)
        -> Result<(Self::VcpuState, u64, Self::RestorePrep), VmmError>;
    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep);
    // ... all other hooks unchanged ...
}
impl Vendor for X86 { type Snapshot = VmState; /* bodies unchanged */ }
```

The engine's `snapshot.rs` holds/seals `<B::A as Vendor>::Snapshot` via `SnapshotRecords`
(encode on seal, `decode` + `ARCH_TAG` gate on restore) — it never names a record. The
`ARCH_AARCH64 = 2` tag is reserved here; a blob with a foreign tag is rejected loudly
(`UnsupportedArch`), never reinterpreted.

**Gate (independently green, x86 only — no arm64 exists yet):**
- `cargo build -p vm-state -p vmm-core --all-features` (+ the box crates on the Linux leg).
- `cargo nextest run -p vm-state -p vmm-core --all-features` — **every existing x86 test green**,
  including the snapshot round-trip and the `state_hash` determinism tests. The `state_hash`
  canonical form is **byte-identical** before and after this change (the determinism gate is the
  one that catches a canonical-form slip — `docs/ARCH-BOUNDARY.md` §Cost).
- `cargo clippy … -- -D warnings`, `cargo fmt --check`, `cargo deny check`.
- The CI aarch64 cross-check (`aarch64-unknown-linux-gnu`, x86 KVM `cfg`'d out) still compiles.
- **Box — BLOCKING, runnable today (NOT edged to `hm-7pb`):** the existing x86 live-boot +
  `state_hash` determinism box gates re-run unchanged through the re-typed hooks, on the existing
  x86 determinism box (`DET_BOX_SSH`), which is available now. This is the proof the seam edit is
  x86-behavior-neutral **on real KVM**, not just in the mock — a live-KVM-only snapshot save/restore
  regression the mock cannot see must **not** merge on Mac-local greens. M0 does not merge until this
  passes. Only the *ARM*-KVM gates (M4) edge to Altra arrival; the M0 x86 box gate does not.

### M1 — the `Arm64` `Arch` value types + `Vendor` skeleton compiling against the engine (the keystone check)

**Deliverable.** The `Arm64` vendor as a zero-sized type implementing `Arch` and `Vendor`,
mirroring `arch/x86/mod.rs` and `vendor/x86/mod.rs`, with a **minimal, unwired** `Devices` so it
compiles and default-denies everything. This is the structural check.

```rust
// vmm-backend/src/arch/arm64/mod.rs
pub struct Arm64;                                    // ZST vendor, mirrors `X86`
impl Arch for Arm64 {
    type Exit = Arm64Exit;
    type Injection = Injection;                      // Interrupt{ intid: GicIntId } | (no NMI on arm64)
    type VcpuState = Arm64VcpuState;                 // x0..x30, sp, pc, pstate, EL1 sysregs (skeleton subset; full set TODO(AA-6))
    type Policy = Arm64Policy;                       // IdRegModel + SysregTrapPolicy (default-deny skeleton; rows TODO(AA-6))
    type IntId = GicIntId;                           // u32-wide (GICv3): SGI 0..16, PPI 16..32, SPI 32..=impl-limit (arch max 1019; 1020..1024 special, 1023 = spurious). Exceeds x86's u8.
    type Caps = Arm64Caps;                           // deterministic_clock via the work-derived guest clock (AA-5 validates)
    type Completion = Arm64Completion;               // arch-payload completions (skeleton: none/minimal)
}

// The arch-specific half of the two-level Exit<Arm64>. Cross-arch exits (Mmio, Hypercall,
// Idle == WFI, Shutdown, Deadline) stay in CommonExit — do NOT duplicate them here.
pub enum Arm64Exit {
    // PATCHED-ABI surface, NOT stock: stock KVM/arm64 emulates supported sysregs and UNDEFs
    // unsupported ones IN-KERNEL — it never surfaces a sysreg trap to userspace (there is no
    // MSR-filter analogue). So `Sysreg` is unreachable on the stock backend, exactly as x86's
    // `Cpuid`/`Rdtsc`/`Hypercall` are patched-only. It exists for the AA-3 patched backend.
    Sysreg { /* trapped ID/PMU/timer sysreg read or write */ },  // TODO(patched-abi)
    // (skeleton starts minimal; grow exactly as the AA-6 contract truth table dictates,
    //  each variant exhaustively matched by dispatch_arch — no wildcard arm, default-deny structural)
}
```

Key seam points the skeleton must get right (all grounded in the read-first files):

- **Idle stays `CommonExit::Idle`, but is patched-ABI on arm64.** WFI and HLT are one concept
  above the trait (`exit.rs`); do not add an arm64 idle variant. But the mechanism is asymmetric:
  x86 *stock* KVM surfaces `HLT` as `KVM_EXIT_HLT` → `Idle`, whereas *stock* KVM/arm64 handles WFI
  **in-kernel** (the vCPU is descheduled/blocked — there is no `KVM_EXIT_WFI`). Surfacing WFI as a
  deterministic `Idle` exit needs the opt-in/patched trap surface (the AA-3 bead), so the stock
  `Arm64KvmBackend` **never returns `Idle`** — do not claim idle-skip on the stock path.
- **The hypercall doorbell is a reserved-MMIO-GPA store** → surfaces as `KVM_EXIT_MMIO` on **stock**
  KVM/arm64 (so bring-up boot needs no patch), recognized by the vendor's `dispatch_mmio` at the
  reserved GPA and handled as the hypercall doorbell (`docs/ARCH-BOUNDARY.md` §4: "on arm64 a
  doorbell surfaces as `KVM_EXIT_MMIO`/hypercall-class, not `KVM_EXIT_IO` … `DOORBELL_PORT` becomes
  a reserved MMIO GPA"). The same `HypercallFrame { args: [u64; 4] }` applies (`x0..x3` name the
  slots via the request/response pages; the transport magic is unchanged). An **`HVC`-based**
  doorbell surfacing as `CommonExit::Hypercall` is the patched alternative — stock KVM services
  guest `HVC` (PSCI) in-kernel, so it is not a stock path.
- **`check_wire_interrupt`/`inject_wire_interrupt` take the `u32` wire already.** The arm64 vendor
  validates against the **implemented, distributor-bounded** GICv3 identity space, not a bare
  `≥ 32 ⇒ SPI` rule: SGIs `0..16` (deliverable — **not** reserved as on x86), PPIs `16..32`, SPIs
  `32..=impl_limit` where `impl_limit` is the distributor-configured maximum (`GICD_TYPER.
  ITLinesNumber`, architectural max **1019**); `1020..1024` are special INTIDs (`1023` = spurious);
  `1024..` (extended SPI / LPI) require extended/LPI support the skeleton does not model. Anything
  past the implemented range → `InterruptReject::OutOfRange`. Never bake x86's `< 16 reserved` into
  the control plane (`vendor/mod.rs` `InterruptReject` docs).
- **`Vendor::Snapshot = Arm64VmState`** (the M0 seam), a **minimal** record set — enough to
  encode/decode a trivial vCPU state and round-trip through the container — with the full sysreg
  record set a `// TODO(AA-6):` (AA-6 owns which sysregs a snapshot must carry). `ARCH_TAG =
  ARCH_AARCH64`.
- **Devices skeleton.** `Arm64Devices` starts with an **unwired** fabric: `service_pending_irqs`,
  `pending_deliverable_interrupt`, `next_timer_deadline_vns`, etc. behave as "no fabric" (mirrors
  x86 before `wire_lapic`) and a PL011 stub for `serial_capture`. M2 fills in the real GICv3.
- **`run_until` = `Unsupported`** (non-goal 3), with the `designed-not-frozen (AA-3)` note.

**Gate (independently green):**
- `cargo build/clippy/fmt/nextest -p vmm-backend -p vmm-core --all-features` green **and every
  x86 gate from M0 still green** (additivity proof).
- A portable `MockArm64Backend` (scripted exits, mirroring `MockBackend`) drives a trivial
  `Vendor::dispatch_arch` + snapshot build→seal→restore round-trip on the Mac natively and under
  Miri. **This is the keystone assertion:** the second vendor instantiates every `Vendor` and
  `Backend` method the engine calls.
- The CI aarch64 cross-check compiles the new modules with the KVM layer `cfg`'d out.

### M2 — the interrupt fabric + generic timer (additive `gicv3` crate)

**Deliverable.** A new `consonance/gicv3/` crate (Cargo layout mirroring `lapic/`) modeling a
GICv3 distributor + redistributor and the generic timer (virtual timer, PPI 27) in the ruled
**pure `now_vns`-in / deadlines-out shape** (`docs/ARCH-BOUNDARY.md` §B ARM row): `mmio_read/
write(.., now_vns)`, `advance_to(now_vns)` → armed deadlines + the arbitrated deliverable INTID
out; **zero crate dependency on `vtime`** (the vmm run loop joins them, exactly as with `lapic`).
Wire it into `Arm64Devices` and fill the fabric methods of `Vendor for Arm64`
(`service_pending_irqs`, `complete_irq_delivery`, `pending_deliverable_interrupt`,
`next_timer_deadline_vns`, `deliverable_timer_deadline_vns`, `has_pending_guest_interrupt`,
`inject_wire_interrupt`, `guest_interruptible` via `PSTATE.{I,F}`). **These compute
arbitration and deadlines — they do not, in the skeleton, deliver into the guest** (see the
delivery bullet).

- **INTID model:** SGIs/PPIs/SPIs over the implemented, distributor-bounded range (M1 §INTID;
  `≤ impl_limit ≤ 1019`); priority + enable + pending/active register files; arbitration returns
  the one highest-priority deliverable INTID (the `set_pending_irq` slot the backend re-arbitrates
  each entry). This arbitration is **pure, deterministic, and fully testable now** — it is the
  half of the fabric that the skeleton actually finishes.
- **Delivery is explicitly OFFLINE in the skeleton, pending AA-6.** Unlike x86 (userspace LAPIC
  under `KVM_IRQCHIP_NONE`, injected via `KVM_INTERRUPT`), **stock KVM/arm64 has no arbitrary-INTID
  queue into a userspace GIC model**: the GIC CPU interface and the generic-timer PPI couple to the
  *in-kernel* vGICv3. Real delivery therefore requires either (a) the in-kernel vGICv3
  (`KVM_CREATE_DEVICE`/`KVM_DEV_TYPE_ARM_VGIC_V3` + `KVM_IRQ_LINE`), whose **bit-identical
  save/restore is exactly AA-6's measured open question** (`docs/ARM-ALTRA.md` §5/AA-6), or (b) a
  userspace model with a patched injection seam. So the stock `Arm64KvmBackend`'s
  `inject`/`set_pending_irq` return `Unsupported` and `Vendor::inject_wire_interrupt` reports "no
  delivery fabric wired" — mirroring x86 **stock** `KvmBackend::inject` being `Unsupported` at
  bring-up (the `bringup.rs` note: interrupt injection is moot until it lands). The `gicv3` crate's
  arbitration logic is complete and tested; wiring it to a real guest interrupt is a `// TODO(AA-6):`
  gated on the vGIC round-trip verdict. Do not claim guest interrupt delivery on any skeleton path.
- **Generic-timer deadline** flows through the existing `TimerQueue`/idle seams as a *computed
  deadline* (the same pure output as the arbitration above — not a delivered interrupt). The timer's
  *counter read* is the clock-page's job later (`hm-rk5`); the skeleton models the timer
  *deadline* only (`docs/PARAVIRT-CLOCK.md` §3.2). **No timing constants invented** —
  `CNTFRQ`/timer-input Hz is a documented DTB value the composition root fixes (like x86's
  `LAPIC_TIMER_HZ`), and it governs deadline arithmetic that is moot until delivery lands (AA-6).

**Gate (independently green):** `-p gicv3` build/clippy/fmt/nextest; property tests (≥256 cases,
reduced under Miri) over the register-file + arbitration + deadline logic; Miri clean for any
`unsafe` (expect none — pure logic, like `lapic`); `-p vmm-core` still green with the fabric
wired. Determinism discipline: `BTreeMap`/sorted iteration only in anything reaching a hash or a
deadline; no float.

### M3 — the boot path: `Image` header + DTB + PSTATE/`x0=dtb` entry (+ PL011)

**Deliverable.** The arm64 boot composition, mirroring `vendor/x86/{linux_loader,entry}.rs` — but
**Multiboot is deleted for ARM, not ported** (`docs/ARCH-BOUNDARY.md` §B):

- **`Image` loader** (`image_loader.rs`): parse the arm64 kernel `Image` header (magic
  `ARM\x64` = `0x644d5241` at offset 56, `text_offset`, `image_size`, `flags`), total over
  untrusted bytes (never panics on arbitrary input — rule #4), flat-load at `text_offset`.
- **DTB builder** (`dtb.rs`): a minimal hand-rolled flattened-device-tree writer (FDT magic
  `0xd00dfeed`) describing the CPU (`enable-method`), memory, GICv3 (`arm,gic-v3`, distributor +
  redistributor regions), the PL011 console, the generic timer (`arm,armv8-timer`, the PPIs),
  and a **reserved region for the paravirt clock page** (the seam `hm-rk5` will use — reserved,
  not populated). Hand-rolled to match the x86 hand-built-boot-struct precedent and stay inside
  the dependency whitelist (judgment call #2 — a vetted FDT crate is an ask-by-comment if the
  foreman prefers it).
- **Entry state** (`entry.rs`): PC = load addr + `text_offset`; `x0` = DTB GPA; `PSTATE` = EL1h
  with `DAIF` masked; the arm64 boot protocol's zeroed `x1..x3`. Built + restored onto a
  `Backend::save()` template exactly as the x86 `compose`/`compose_linux` do (the get→modify→set
  pattern; order load-bearing: policy before first run, map before restore, RAM moves into the
  `Vmm`).
- **PL011 UART** as the serial device (the 8250 *pattern* carries — `docs/ARCH-BOUNDARY.md` §B);
  feeds `Vendor::serial_capture`/`inject_serial_input`.

**Gate (independently green):** portable unit tests for `Image` parsing (valid/garbage/truncated),
the DTB writer (structure + a round-trip parse-back check), and the entry overlay — all native +
Miri, mock-backed like `bringup.rs`'s tests. **TCG smoke** (`qemu-system-aarch64`, local): the
`Image`+DTB **boot artifacts** this milestone produces are booted on qemu's own emulated aarch64
machine to a console marker — proving the *guest image is well-formed and boots*, **liveness/shape
only, no counts**. Note precisely what it is **not**: TCG is qemu's own VMM, so it does **not**
exercise `Arm64KvmBackend` (that path talks to `/dev/kvm`; it is M4's, arrival-day). This gate
validates the artifacts, not our ioctls (mirrors `tasks/109`'s TCG discipline; propagate every
gate RC — a done-marker is never success).

### M4 — the KVM/arm64 stock backend + composition root (Linux+aarch64-gated); box gates edged

**Deliverable.** `Arm64KvmBackend`: a `Backend<A = Arm64>` impl against the **documented**
kvm/arm64 ABI, mirroring the x86 stock `KvmBackend`, gated
`#[cfg(all(target_os = "linux", target_arch = "aarch64"))]` (the pure logic above it is portable;
this is box code, exactly as the x86 `KvmBackend`/`work_perf` are Linux+x86-gated — the arch
slice legitimately spans box code behind the established cfg pattern, not a rule-6 delegated
crate):

- `KVM_CREATE_VM` → `KVM_CREATE_VCPU` → `KVM_ARM_VCPU_INIT` (with `KVM_ARM_PREFERRED_TARGET`),
  single vCPU; `KVM_SET_USER_MEMORY_REGION` for the `unsafe` `map_memory` seam (page-aligned,
  pinned, pre-populated — no demand paging, the determinism choice; SAFETY comment per the
  `Backend::map_memory` contract).
- Register save/restore via `KVM_GET_ONE_REG`/`KVM_SET_ONE_REG` over the core + EL1 sysreg IDs
  the `Arm64VcpuState` subset carries (full set TODO→AA-6).
- `KVM_RUN` exit decode — **the stock/patched split is load-bearing and must be honest** (mirrors
  x86, where stock surfaces Io/Mmio/MSR/Shutdown and Hypercall/Cpuid/instruction exits are
  patched-only). **Reachable on the STOCK backend:** `KVM_EXIT_MMIO` → `CommonExit::Mmio`
  (including the reserved-GPA doorbell store, which `dispatch_mmio` recognizes and handles as the
  hypercall doorbell — so the bring-up boot needs no patch); `KVM_EXIT_SYSTEM_EVENT` (PSCI
  `SYSTEM_OFF`/`RESET`) → `CommonExit::Shutdown`. That is the **entire** stock surface — the stock
  `run` returns only `Mmio`/`Shutdown`. **Patched-ABI only (the decode arms exist for the AA-3
  backend, `// TODO(patched-abi)`, and the stock backend never returns them):** WFx → `Idle`
  (stock blocks WFI in-kernel); trapped sysregs → `Arm64Exit::Sysreg` (stock emulates/UNDEFs
  in-kernel, no userspace trap); an `HVC`-based doorbell → `CommonExit::Hypercall` (stock services
  guest `HVC`/PSCI in-kernel). Each mapping cites the documented ABI; the box confirms.
- `inject`/`set_pending_irq` return `Unsupported`, `take_accepted_interrupt` returns `None`: the
  stock backend has **no delivery path** into the guest for a userspace GIC (M2 §Delivery) — this
  mirrors stock x86 `KvmBackend::inject` at bring-up. Delivery is AA-6-gated, not this backend's.
- `set_policy` installs the `Arm64Policy` skeleton. **What actually works on stock:** the
  `ID_AA64*` freeze is a **config-time** write via KVM's writable-ID-register surface
  (`KVM_SET_ONE_REG` on the ID regs before the first `KVM_RUN`) — reachable now. **What is
  patched-only:** the `HCR_EL2`/`MDCR_EL2` trap-group *enforcement* that turns a denied sysreg into
  a userspace `Sysreg` exit — the skeleton records the trap-table shape but its runtime exits are
  AA-3's (`// TODO(patched-abi)`; full row set AA-6). `run_until` = `Unsupported` (non-goal 3).
  `capabilities()` reports every determinism field **honestly false** for the stock backend
  (mirrors stock `KvmBackend`).
- **Composition root** `vendor::arm64::bringup::boot_selected` — the one place the
  `(Arm64KvmBackend, Arm64)` pair is named — mirroring x86's `boot_selected`, Linux+aarch64-gated.

**Gate:**
- **Local:** `cargo build -p vmm-backend --all-features` on the Mac (KVM layer `cfg`'d out — the
  pure decode/mock logic compiles native) **and** the CI aarch64-linux cross-check compiles the
  full KVM layer (without running it). A `MockArm64Backend`-driven exit-decode unit test covers the
  pure `kvm_run` → `Exit` mapping logic. For **ioctl-sequence shape** (request numbers, argument
  struct layout, call ordering: `KVM_ARM_VCPU_INIT` before the first `KVM_SET_ONE_REG`, etc.), seam
  the syscall boundary behind a thin trait and assert against a **syscall-fake/ioctl-trace** double
  — portable + Miri, no `/dev/kvm`. **TCG does NOT enter here:** qemu emulates its own machine, so
  it never issues our ioctls; the real ioctl path against `/dev/kvm` is **arrival-day-only** on the
  Altra (there is no local KVM loop — `hm-8l3` REFUSE).
- **Box (edged to `hm-7pb`, ARRIVAL-DAY — not runnable now):** on the Altra, a real
  `KVM_RUN` boots the `Image`+DTB path to a console marker, and a same-seed pair holds a
  bit-identical `state_hash` (the determinism gate). These gates are **specified here but
  dispatched by the Altra window** — and every count/PMI/skid claim they would make is the
  *spike's* (`docs/ARM-ALTRA.md` AA-1/AA-3), never this task's.

## Gates (summary — the acceptance bar)

Run per-crate for the surface list (`-p vm-state -p vmm-core -p vmm-backend -p gicv3`):

```sh
cargo build   -p <crate> --all-features
cargo nextest run -p <crate> --all-features        # ≥256 proptest cases; total test time < ~3 min
cargo clippy  -p <crate> --all-features --all-targets -- -D warnings   # workspace clippy.toml determinism lints
cargo fmt     -p <crate> -- --check
cargo deny check
# public-api: regenerate the frozen goldens the quality.yml `public-api` job diffs (M0/M1 move the
# public surface). Linux-frozen — regenerate on the box, else the KVM-gated Arm64KvmBackend surface
# is dropped (hm-rk5/PR #110 precedent). New crate `gicv3` gets its own tests/public-api.txt.
cargo test -p <crate> --test public_api -- --ignored --nocapture
# Miri lives in .github/workflows/nightly.yml (NOT quality.yml), pinned toolchain + MIRIFLAGS:
MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p <crate>   # unsafe + alloc-backed seam (PR #99)
```

Plus the task-specific gates: the CI `aarch64-unknown-linux-gnu` cross-check (additivity — the
x86 substrate `cfg`'d out must still compile the tree); the **TCG smoke** (**M3 boot artifacts
only** — the `Image`+DTB payload boots on qemu's own VMM; TCG never runs `Arm64KvmBackend`); and
every **x86 gate green unchanged through M0's spine edit** (the neutrality proof). Box classification
is **not uniform**: the **M0 x86-neutrality box gate is BLOCKING and runnable today** (existing x86
determinism box — a live-KVM snapshot regression must not merge on Mac greens); **only the M4 arm64
boot + `state_hash` determinism gates are arrival-day**, edged to `hm-7pb`. Both are stated in
Environment.

Determinism discipline is non-negotiable (rule #4): no `HashMap`/`HashSet` reaching a hash,
deadline, or encoded byte (`BTreeMap`/sorted); no float in anything affecting state; library code
never panics on untrusted input (the `Image`/DTB/exit decoders are total). Every `unsafe` gets a
`// SAFETY:` and is Miri-reachable via the in-process mock loopback; the `asm!`/ioctl/privileged
paths sit behind a seam and are `#[cfg(not(miri))]`-excluded.

## Environment

- **Fully Mac-local (runnable now):** native aarch64 build + all portable/Miri gates (this Mac is
  aarch64 — pure logic is the target arch, not a cross-build); the CI aarch64-linux cross-check
  (`rustup target add aarch64-unknown-linux-gnu`); TCG via `qemu-system-aarch64` (Homebrew).
- **No local KVM ioctl dev loop.** `hm-8l3` = REFUSE: this host is an Apple M1 Max (pre-M3), so
  Virtualization.framework nested virt is unavailable and an aarch64 Linux guest cannot expose a
  hardware `/dev/kvm`. TCG is the local oracle for the guest **boot artifacts** only (it runs
  qemu's own VMM, not our backend); the `Arm64KvmBackend` **ioctl path has no local oracle** — its
  *shape* is asserted by a syscall-fake/trace, and the real ioctls are arrival-day-only.
- **x86 determinism box — BLOCKING, available now:** the existing box (`hetzner`/`DET_BOX_SSH`,
  `docs/BOX-PINNING.md`) runs **M0's x86-neutrality re-run** (live-boot + `state_hash` through the
  re-typed snapshot hooks). This is not arrival-day — it gates M0's merge today.
- **Altra box — arrival-day, edged to `hm-7pb`:** the Ampere Altra (Neoverse N1), reached via
  `ARM_BOX_SSH` (the `DET_BOX_SSH` convention extended; the repo hard-codes no host). **Only** the
  M4 arm64 boot + `state_hash` determinism gates run here, and every count/PMI/skid claim they
  could make is the spike's (`docs/ARM-ALTRA.md`), never this task's.

## Deliverables (definition of done for the IMPLEMENTATION task this spec spawns)

A branch `task/arm-backend-skeleton` (or as the foreman dispatches) containing only the surface
list, with:
1. M0–M4 all green on every Mac-local gate; **M0's x86-neutrality box gate green on the existing
   x86 box (blocking, now)**; only the M4 arm64-KVM box gates edged to `hm-7pb`.
2. The keystone proven: `Arm64` instantiates every `Vendor`/`Backend`/`Arch` method the engine
   calls; x86 gates green unchanged through the M0 spine edit.
3. Every spike-measured quantity a `// TODO(AA-N):` bound to its stage; every would-be-frozen seam
   carrying the `designed-not-frozen (AA-3)` note.
4. A short `IMPLEMENTATION.md` in the ARM vendor directory (deviations considered/rejected, known
   limitations, and — critically — the exact list of sanctioned spine edits made and why each is
   the §D exception and not scope creep, plus which seams AA-1/AA-3/AA-6 may force rework of).

Do not open follow-on work; stop when the gates pass. The crate split (non-goal 6), the full CPU
contract (non-goal 5), the exact-landing patched backend (non-goal 3), and the clock-page protocol
(non-goal 4) are named follow-on beads, not this task's.

## Vocabulary & discipline

GLOSSARY register throughout prose and identifiers (`docs/GLOSSARY.md`): **Subject**, **Moment /
Span**, **Reproducer**, **V-time**, **state_hash**; **vendor**, never "personality"; no
"(formerly X)" / "renamed from" comment residue (provenance lives in git + GLOSSARY — code reads
as if the arm64 names were always the names). Worker effort per the ruling (`worker-effort`
memory): the implementer runs at explicit high/xhigh effort, not the CLI default.
