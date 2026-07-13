# The architecture boundary — ISA seam design

Status: **design ruling (2026-07-03).** Supersedes the codebase survey in `docs/ARM-PORT.md`
("What a port costs, by component" and its "no arch seam exists yet" premise), which predates
Wave 4/5 and undercounts the tree by most of `vmm-core`, `vmm-backend`, `lapic`, `vm-state`,
and all seven dissonance crates. ARM-PORT.md's **hardware facts and viability principle
stand**: no production ARM backend gets built before the applicable substrate's PMU/exact-
landing spike returns GO on real silicon. What this document rules is the *boundary* — where
the ISA seam goes, what shape it takes, and which parts are justified now on x86-hygiene
grounds versus frozen until spike data exists.

**Apple-silicon refinement (2026-07-09):** `docs/APPLE-SILICON.md` is the primary ARM
hardware target and de-risk plan. Its launcher, EL2 monitor, Linux subject, device models, and
snapshot path may be built as a complete parallel implementation without first applying this
boundary. This ruling becomes an integration input only after that standalone hypervisor is
working and a separate human decision chooses consolidation.

Bottom line up front: a fresh file-level audit (2026-07-02, five-zone sweep of consonance +
dissonance + guest) shows **~85% of the tree is already arch-blind** — including the vtime
planner, the snapshot store, the control protocol/server, unison/det-corpus, hypercall-proto,
and the entire dissonance layer. The x86 coupling is concentrated, not smeared: the
`vmm-backend` value-type vocabulary, five nameable modules of `vmm-core`, the `lapic` crate,
and the guest payloads. The restructure is therefore **promoting an implicit boundary into a
compiler-enforced one**, not an untangling.

## The boundary, stated crisply

> **Arch = the vocabulary of guest-observable CPU events and state**: exit kinds, the
> register/sysreg record set, the CPU-contract policy tables, interrupt identities, the
> boot/entry protocol, and the interrupt-fabric device model.
>
> **Everything else** — V-time planning, run-until orchestration, snapshot CoW + hashing
> framework, the control protocol, exploration, journaling, the hypercall protocol — speaks
> only `(Gpa, Vtime, Moment, bytes, hashes)` and must be compiler-provably arch-blind.

The codebase already obeys this de facto everywhere except one place: the `Backend` trait's
monomorphic x86 vocabulary and the `vmm-core` code that consumes it. The existing R-Backend
split (`docs/R-BACKEND.md`) is a *substrate* seam — stock-KVM vs patched-KVM vs mock, "portable"
meaning macOS-buildable — not an ISA seam. Its central invariant ("nothing above the trait may
branch on which backend is in use") is exactly the invariant to extend: nothing above the arch
seam may branch on which ISA is in use.

## Audit: where the coupling actually is (supersedes ARM-PORT.md's survey)

### Arch-blind today — ports as-is, zero changes

- **`consonance/vtime`** — `CpuBackend` (`planner.rs`: `work()` / `run_until_overflow()` /
  `single_step()` over a monotonic, 0-or-1-per-instruction `u64` counter) is exactly as valid
  for ARM `BR_RETIRED` (taken branches) as for Intel conditional branches. ARM-PORT.md's claim
  that this trait "already models the one hard hardware seam correctly" is **verified**.
  `PlannerConfig::skid_margin` is a re-measured *value*, not structure. `VClock::tsc()`
  (`clock.rs`) is structurally a generic Hz-scaled counter mapping unchanged to
  `CNTVCT`/`CNTFRQ`; the leak is the field *name* only. `SimCpu` validates an ARM backend
  unchanged (re-parameterize density/max_skid).
- **`unison`, `det-corpus`** — abstract over `Machine` (`run_to`/`work`/`state_hash`); no
  register knowledge. The x86 coupling point is the `Machine` impl over `Vmm`
  (`vmm-core/src/corpus.rs`), which is the intended seam.
- **`hypercall-proto`** — byte-framed, code-pinned little-endian, no register ABI on the wire.
- **`snapshot-store`**, the `SnapshotEngine` half of `vmm-core/src/snapshot.rs`,
  **`vmm-core/src/control.rs`** (task-58 server), `corpus.rs`, `work.rs` (the `WorkSource`
  trait seam), and `vm-state`'s TLV container codec (records are x86; machinery is not).
- **All of dissonance.** `explorer`, `flow`, `matcher`, `runtrace` (scrape decodes the serial
  console byte stream, never guest state), `conductor` lib/record, and `control-proto`'s wire
  (registers ride as opaque `CrashInfo.detail`; host faults as opaque blobs; state addressed by
  `SnapId`/`Moment`/`VTime`/GPA ranges). Total in-layer arch surface: one field —
  `HostFault::InjectInterrupt { vector: u8 }` (`environment/src/host.rs`) — and one name,
  `CrashKind::TripleFault` (`control-proto/src/types.rs`).

### Where the x86 lives — the complete list

1. **`vmm-backend`'s value types** (the load-bearing leak).
   `Exit::{Io, Rdmsr, Wrmsr, Cpuid, Rdtsc, Rdtscp, Rdrand, Rdseed, Hlt}` (`exit.rs`),
   `Event::{Interrupt{u8}, Nmi}`, `HypercallRegs{rax..rdx}`, all of `state.rs`
   (`VcpuRegs`/`VcpuSregs`/segments/GDT-IDT/CR*/XSAVE/MSR map/`VcpuEvents` incl. SMM),
   all of `config.rs` (`CpuidModel`/`MsrFilter`), two `Capabilities` flags
   (`deterministic_tsc`, `enforces_tsc_deadline_msr`), and seven trait methods whose own
   signatures name x86 (`set_cpuid`, `set_msr_filter`, `complete_cpuid`,
   `complete_hypercall(rax)`, the `u8`-vector IRQ trio). Meanwhile `run_until.rs` (the whole
   planner-inversion orchestration), `error.rs`, `Gpa`/`Vtime`/`Mmio`/`Deadline`/`MpState`,
   the exit-count machinery, and the perf ring-buffer plumbing are neutral. The Intel pin in
   `pmu.rs` is one raw event constant (`0x1c4` = `BR_INST_RETIRED.CONDITIONAL`).
2. **Five modules of `vmm-core`**: `contract/*` (CPUID/MSR dispositions over the embedded
   `cpu-msr-contract.toml`), the boot path (`entry.rs`, `linux_loader.rs`, `multiboot.rs`),
   `devices.rs` (`LegacyPlatform`: 8259 IMR latches, PIT/CMOS/POST absent-value shims, PCI
   CF8/CFC latch; the 8250 UART pattern itself carries), `hostassert.rs`, `work_perf.rs`
   (same Intel event pin). Plus the `Exit`-dispatch match and LAPIC/IRQ arbitration inside
   `vmm.rs`, and the field-copy half of `snapshot.rs` (`to_vm_regs`/`to_vm_sregs`).
3. **`lapic`** — x86 xAPIC by definition; ARM analogue is a GICv3 + generic-timer model. Its
   *seam shape* is already perfect: pure V-time-ns in (`mmio_read/write(.., now_vns)`,
   `advance_to(now_vns)`), deadlines + deliverable vectors out; zero crate dependency on vtime
   or vice versa — the vmm run loop joins them. A GIC model drops into the identical shape.
4. **Guest side.** `vmcall-transport`'s arch surface is one ~20-line
   `#[cfg(target_arch = "x86_64")]` `IoDoorbell::ring` impl (`out dx, eax`); ARM is `hvc` or an
   MMIO store behind the same trait — but note the *host* dispatch consequence: on arm64 a
   doorbell surfaces as `KVM_EXIT_MMIO`/hypercall-class, not `KVM_EXIT_IO`, so "which exit is
   the doorbell" is per-arch personality knowledge, and `DOORBELL_PORT = 0x0CA1` becomes a
   reserved MMIO GPA. The bare-metal payloads + goldens are x86 *by purpose* (they test the x86
   CPU contract): ARM gets **new** payloads against a new contract, not ports. Reusable:
   `compute-core`/`contract-data`'s host-derived-golden harness pattern, the Linux build
   scripts (mostly `ARCH`-parametric), `kata/common`. Needs audit: `linux/config-fragment`
   (carries `CONFIG_X86_*` symbols).
5. **The KVM patches** (0004 force-exit, 0005 MTF single-step) — host-kernel, VMX-specific.
   arm64 KVM already exposes hardware single-step via `KVM_GUESTDBG_SINGLESTEP`
   (`MDSCR_EL1.SS`), so the 0005 analogue may be nearly free; the 0004 analogue
   (deterministic in-kernel force-exit at PMI) is real kernel work.
   On Apple silicon there is no KVM analogue: the virtual-EL2 Harmony monitor must own PMU
   overflow routing and L2 debug stepping, while Hypervisor.framework remains L0. Whether
   that is sufficient is exactly `APPLE-SILICON.md` AS-2 through AS-4.

## The seam design

### A. `Arch` trait + generic `Backend` (the one real design decision)

```rust
trait Arch {
    type Exit;        // arch-specific exit variants only
    type Event;       // injectable events (x86: Interrupt{vec}/Nmi; arm: GIC INTID class)
    type VcpuState;   // full register record set
    type Policy;      // x86: CpuidModel + MsrFilter; arm: IdRegModel + SysregTrapPolicy
    type IntId;       // u8 vector vs GIC INTID
    type Caps;        // arch capability flags
}
```

The load-bearing choice is how `Exit` splits. **Ruling: a two-level enum** —

```rust
enum Exit<A: Arch> {
    Common(CommonExit),   // Mmio, Hypercall(HypercallFrame), Idle, Shutdown, Deadline
    Arch(A::Exit),        // x86: Io, Rdmsr, Wrmsr, Cpuid, Rdtsc(p), Rdrand/Rdseed
}
```

with two deliberate neutralizations:

- `HypercallRegs{rax,rbx,rcx,rdx}` → `HypercallFrame{args: [u64; 4]}` (the names were only
  ever labels; also fixes `complete_hypercall(rax)`).
- `Hlt` → `Idle` (HLT and WFI are one concept to everything above the trait).

**Why this shape and not the alternatives.** A superset enum (one `Exit` with every arch's
variants) is rejected: it is the one way to genericize this that silently weakens R-Backend's
default-deny — an unhandled ARM variant could fall through an x86-written wildcard arm. The
two-level shape keeps default-deny *structural*: each arch's exit enum is exhaustively matched
by that arch's own dispatch. An opaque/message-passing exit (push handling below the trait) is
rejected because it would move the contract *dispositions* below the seam, violating R-Backend's
"dispositions live above" division.

Trait method regrouping: `set_cpuid`/`set_msr_filter` collapse into `set_policy(&A::Policy)`;
the completion methods keep the neutral read/ok/fault trio and carry arch payloads via an
associated completion type; the IRQ trio takes `A::IntId`; `run`/`run_until`/`save`/`restore`/
`map_memory`/`exit_counts` keep their shapes with generic returns. `run_until.rs`, `error.rs`,
`MockBackend`, and the perf ring machinery move unmodified; the `0x1c4` event pin becomes a
per-arch constant supplied with the backend. `Capabilities`' `deterministic_tsc` /
`enforces_tsc_deadline_msr` become arch-named flags in `A::Caps` (the *concepts* —
deterministic guest clock, enforced timer-deadline register — recur per-arch; the names don't).

### B. `vmm-core` = engine + personality

The **engine** (arch-neutral, generic over `Arch`): run-loop skeleton, `GuestRam`,
`SnapshotEngine`, the state-hash *framework* (canonical record list → hash), `control.rs`,
`corpus.rs`, `work.rs`, the `DeviceBlob` mechanism, V-time/idle wiring.

The **personality** (per-arch; x86 is the first and, until spike GO, only one):

| Responsibility | x86 today | ARM later |
|---|---|---|
| CPU contract → installed policy | `contract/*` + `cpu-msr-contract.toml` | ID-reg freeze + trapped-sysreg table — same data-driven table→model→enforce shape, new schema + new contract doc |
| Exit dispatch + dispositions | the `vmm.rs` `Exit` match, MSR/CPUID handlers | sysreg-trap dispositions |
| Boot: loader + entry state | `entry.rs`, `linux_loader.rs`, `multiboot.rs` | arm64 `Image` header + DTB + PSTATE/`x0=dtb` entry; multiboot is deleted for ARM, not ported |
| Interrupt fabric + V-time timer | `lapic` crate + 8259 shims + `service_pending_irqs` | GICv3 + generic-timer model, same pure `now_vns`-in/deadline-out shape |
| Host homogeneity probe | `hostassert.rs` (CPUID/MXCSR/microcode) | MIDR/`ID_AA64*`/errata behind the same `enforce()` |
| Work counter | `work_perf.rs` (Intel `0x1c4`) | `BR_RETIRED` `0x21` behind the unchanged `WorkSource` trait |
| State records (hash + snapshot) | `to_vm_*` adapters + `vm-state` x86 records | arm64 record set; same TLV container; `VM_STATE_VERSION` bump + arch tag in the header |

**Module split first, crate split when ARM lands.** The boundary is the trait, not the crate
wall; moving ~10k lines between crates while task branches are in flight is churn with no
gate-visible payoff. When an ARM backend is actually added, the crate split (so adding a
backend doesn't compile x86 code) falls out along the already-drawn module lines. The
composition-root discipline (`docs/BRINGUP.md`: `fn main` is the one place a concrete backend
is named) extends naturally: main names the `(Backend impl, Arch personality)` pair.

### C. Upstream fixes (cheap, justified regardless of ARM)

1. `environment`: widen `InjectInterrupt { vector: u8 }` — GIC INTIDs exceed 8 bits. One codec
   change (`environment/src/codec.rs`), one demo constant (`explorer/src/adapter.rs`). This is
   the **only** dissonance code change the entire port requires.
2. `control-proto`: `CrashKind::TripleFault` → a portable crash-taxonomy name (cosmetic; the
   wire is otherwise clean).
3. `vtime`: rename `VClockConfig::{tsc_hz, tsc_base}` → guest-clock naming, whenever vtime is
   next touched (naming-only; the arithmetic is arch-free).
4. `conductor/main.rs` box-mode defaults (`bzImage`, x86 cmdline, `BackendKind`) become
   per-arch config data.

### D. Explicitly NOT restructuring — the ARM new-build (post-spike only)

Additive crates/artifacts, zero edits to the neutral spine: KVM/arm64 backend impl; GICv3 +
generic-timer models; the ARM CPU-contract document (the x86 one is the template for *rigor*,
not content); `Image`/DTB loader; arm64 payload runtime (boot shim, exception vectors, PL011,
GIC init) + new contract payloads + regenerated goldens; the 0004-analogue kernel patch;
re-measured `skid_margin` / `SimCpu` parameters; arm64 kernel-config audit + `kata/arm64`.

For the Apple target, substitute a Hypervisor.framework launcher/backend plus the
virtual-EL2 monitor for “KVM/arm64 backend” and “0004-analogue kernel patch.” The GIC,
generic-timer, ARM contract, Image/DTB, payload, state-record, and Linux work remain shared ARM
personality work. Do not force the monitor below `Backend`: contract dispositions remain above
the substrate seam even if the low-level trap is first caught at EL2.

## Sequencing, cost, risks

**Order.** For the Apple program, complete `APPLE-SILICON.md` AS-0 through AS-9 in its
parallel workspace; none of those stages requires this refactor. If a later human ruling
chooses consolidation: (1) the C-list + `HypercallFrame`/`Idle` neutralizations — small, land
any time.
(2) Mechanical extraction of x86 value types into an arch module inside `vmm-backend`, no
semantics change, all gates green. (3) **The keystone**: `Arch` trait + generic `Backend` +
engine/personality module split in `vmm-core`, x86 as the sole implementation, every existing
portable + box gate passing unchanged through it. (4) `vm-state` arch-tagged records + version
bump. Then the D-list becomes an additive backend wave. For Apple silicon this sequence is
optional and post-success; do not interrupt the standalone implementation to prepare it.

**Cost.** Steps 1–4 ≈ four tasks, mostly mechanical. The creative parts: the `Arch` trait
shape (ruled above; freeze per the spike caveat below) and keeping the state-hash canonical
form stable through the record-set refactor — the determinism gate exists to catch exactly
that slip.

**Risks, in order of realness.**

- **Branch churn.** `vmm.rs` and `vmm-backend` are touched by most in-flight task branches.
  The keystone (step 3) must be sequenced into the queue **after the current merge window**,
  not alongside it, or every open PR pays a rebase.
- **Generics creep.** Keep `A` an associated type; personalities are ZSTs. Nothing above
  vmm-core goes generic — dissonance and control-proto stay non-generic (they already prove
  they can, via opaque bytes). A `<A: Arch>` parameter appearing in a dissonance crate is a
  review-blocking smell.
- **Default-deny erosion.** Handled structurally by the two-level `Exit`; this is the thing to
  be paranoid about in review of step 3.
- **The spike still gates one trait decision.** ARM's PMU-overflow-to-exit path (no MTF; PMI
  delivery differs; the N1-lineage missed-PMI-on-migration bug in ARM-PORT.md §evidence) may
  pressure `run_until_overflow`'s late-only-stop contract. Design the trait now; **freeze it
  only with spike data in hand.**

## Relationship to ARM-PORT.md

Still true and still binding: the hardware table (Spark vs Grace, ECV), the three
load-bearing-mechanism analysis, the rr evidence base, the LL/SC vs LSE hazard, and the gate —
**the applicable substrate spike on real silicon decides whether ARM happens; no D-list work
before GO.** For Apple silicon, that is the AS-2 through AS-7 sequence, not the Linux/KVM
Spark/Grace spike.

Superseded by this document: the "What a port costs, by component" survey and its premises
("no arch seam exists", "`vmm-core` unwritten", the ~60/40 split) — the audit above replaces
them; and the blanket "do not build the arch abstraction pre-emptively" is **refined**, not
reversed: the A–C restructure is justified on x86-hygiene grounds alone (it makes the
R-Backend boundary compiler-enforced and the product thesis explicit — a deterministic-execution
engine with an x86 backend, not an x86 hypervisor with a bug-finder attached). For a future
Linux/KVM ARM personality, trait freeze remains spike-gated. The standalone Apple program is
the explicit exception: it may build independently without adopting A–C, and this document
does not become binding on it unless a later consolidation ruling says so.

## Relationship to APPLE-SILICON.md

`docs/APPLE-SILICON.md` selects the first ARM substrate and owns a complete parallel
implementation through deterministic Linux, snapshot/branching, and performance
characterization. This document does not constrain that code's internal abstractions. It
becomes relevant only if a later human ruling chooses to consolidate the standalone Apple
implementation with consonance. A general `Arch` refactor in consonance before then is
premature unless independently justified and separately ruled.
