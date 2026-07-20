# The architecture boundary — ISA seam design

Status: **design ruling (2026-07-03); vendor programs ruled 2026-07-12/13** (`docs/ARM-ALTRA.md`,
`docs/AMD-EPYC.md` — the reserved engine/vendor split names activate with the ARM window);
**pre-build ruled 2026-07-13** (§Pre-build ruling below — building no longer waits for spike
GO; trust still does). **§Sequencing steps 1–4 LANDED 2026-07-14** (`tasks/108`, `hm-b5n`):
the C-list neutralizations, the x86 value-type extraction, the keystone (`Arch` trait +
generic `Backend` + the engine/vendor module split in `vmm-core`, x86 the sole vendor), and
`vm-state`'s arch-tagged records (`VM_STATE_VERSION` 2). The seam is now
**compiler-enforced**: the engine names no vendor device, register, exit, loader, or error
— not in any signature, field, or error variant — and each vendor's exit enum is
exhaustively matched by its own dispatch. The concrete `(Backend impl, Arch vendor)` pair
is named only in a vendor's own composition root (`vendor::x86::bringup`), never in the
engine. **The additive property is itself gated**: CI cross-checks the workspace for
`aarch64-unknown-linux-gnu`, where the x86 KVM substrate is `cfg`'d out — so an x86 leak
into the engine fails CI rather than surfacing on the day the ARM backend starts.

**One stated exception, ruled and deferred (2026-07-14, PR #109) — the snapshot-state
seam.** `Vendor`'s three snapshot hooks (`build_vm_state` / `validate_restore` /
`commit_restore`) are typed against the concrete `vm_state::VmState`, whose register
records are x86-64's. A second vendor therefore **cannot** implement them without a trait
change (an associated `type Snapshot`, or a vendor-parameterized `VmState`). So the
additive-sibling promise above is exact for exit dispatch, devices, the interrupt fabric,
policy, boot, and errors — and **carries this one boundary** for snapshot state. It is
deferred deliberately: the trait is *designed, not frozen* (AA-3 owns the freeze; the
pre-build ruling accepts rework), the ARM record set is **AA-6's measured decision** rather
than something to guess at now, and step 4's arch tag already makes the *format*
extensible (`VM_STATE_VERSION` 2 + `ARCH_X86_64`; a foreign record set is rejected loudly
as `UnsupportedArch`, never reinterpreted). The CI arch gate **cannot** catch this class —
no vendor exists on the aarch64 leg to instantiate the trait — so the structural check is
the ARM skeleton itself (`hm-cbt`), the first real second implementor. (A stub "dummy
vendor" purely to force the check was considered and rejected as redundant: `hm-cbt`
supplies a real one.) **The D-list and the trait
freeze are unchanged** — the trait is *designed, not frozen*; AA-3's trait-freeze memo (the
ARM spike) still owns the freeze, and §D stays additive-and-spike-trusted. Supersedes the
codebase survey in `docs/ARM-PORT.md`
("What a port costs, by component" and its "no arch seam exists yet" premise), which predates
Wave 4/5 and undercounts the tree by most of `vmm-core`, `vmm-backend`, `lapic`, `vm-state`,
and all seven dissonance crates. ARM-PORT.md's **hardware facts and its viability gate stand
unchanged in what they decide** — the spike still rules whether ARM ships and what may be
trusted — but since the 2026-07-13 pre-build ruling the gate no longer sequences
*construction* (it originally read: no ARM backend gets built before the PMU spike returns GO
on real silicon). What this document rules is the *boundary* — where the ISA seam goes, what shape
it takes, and which parts are justified now on x86-hygiene grounds versus frozen until spike
data exists.

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
  for ARM `BR_RETIRED` (all executed branch instructions, AA1-F1) as for Intel conditional
  branches. ARM-PORT.md's claim
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
  console byte stream, never guest state), `campaign-runner` lib/record, and `control-proto`'s wire
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

The **personality** (per-arch; x86 is the first — the ARM one is the pre-build wave's
deliverable, `hm-cbt`):

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
4. `campaign-runner/main.rs` box-mode defaults (`bzImage`, x86 cmdline, `BackendKind`) become
   per-arch config data.

### D. Explicitly NOT restructuring — the ARM new-build (pre-buildable since the 2026-07-13 ruling; trusted only post-spike)

Additive crates/artifacts, zero edits to the neutral spine: KVM/arm64 backend impl; GICv3 +
generic-timer models; the ARM CPU-contract document (the x86 one is the template for *rigor*,
not content); `Image`/DTB loader; arm64 payload runtime (boot shim, exception vectors, PL011,
GIC init) + new contract payloads + regenerated goldens; the 0004-analogue kernel patch;
re-measured `skid_margin` / `SimCpu` parameters; arm64 kernel-config audit + `kata/arm64`.

**"Zero edits to the neutral spine" has one ruled exception (2026-07-14, PR #109): the
snapshot-state seam.** `Vendor`'s snapshot hooks are typed against the concrete
`vm_state::VmState` (x86 records), so `hm-cbt` — the ARM skeleton — **will** have to change
that signature (an associated `type Snapshot`, or a vendor-parameterized `VmState`), and
`vm-state` will gain an arm64 record set under a new arch tag. That trait rework is
**accepted, not an escape hatch**: the trait is designed-not-frozen (AA-3 owns the freeze),
the ARM state shape is **AA-6's measured** decision, and the v2 arch tag is the format's
extension point (the container/version/tag machinery is already arch-neutral and rejects a
foreign record set loudly). `hm-cbt` is also the *structural check* for the whole seam —
being the first real second implementor, it is what proves the rest of `Vendor` is
genuinely additive in a way no cross-compile gate can.

## Sequencing, cost, risks

**Order.** (1) The C-list + `HypercallFrame`/`Idle` neutralizations — small, land any time.
(2) Mechanical extraction of x86 value types into an arch module inside `vmm-backend`, no
semantics change, all gates green. (3) **The keystone**: `Arch` trait + generic `Backend` +
engine/personality module split in `vmm-core`, x86 as the sole implementation, every existing
portable + box gate passing unchanged through it. (4) `vm-state` arch-tagged records + version
bump. Then the D-list as an additive backend wave — originally gated on ARM-PORT.md's spike #1
returning GO on real silicon, un-gated by the pre-build ruling below (2026-07-13).

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
  only with spike data in hand.** — **RESULTS RETAINED; certification pending.**

## AA-3 trait-freeze memo (certificate voided 2026-07-18; results retained)

AA-3 ran the patched-KVM (`-aa3preempt`) force-exit +
`run_until_overflow` + `single_step` exact landing at **1,010,800 armed deadlines** on the
Ampere Altra (Neoverse N1), sharded 76-wide, aggregate `floor-check` **PASS (1371 checks)**,
solo-vs-co-tenant determinism **MATCH** (evidence: `spikes/arm-altra/results/aa-3/exact-evidence/`,
disposition: `docs/ARM-ALTRA.md` §AA-3). Verification later found the campaign did not invoke
the comparator and the original comparator accepted intersections. Full-join recomputation over
the retained records still MATCHed 5,700/5,700 keys with zero divergences, so the physical findings
below are retained and the mechanism is presumed sound; however, the GO certificate and trait
freeze are **void** until the repaired apparatus completes re-verification:

- **`run_until_overflow`'s late-only-stop contract HOLDS on N1 — no `Arch`-trait change forced.**
  The armed overflow is late-only: the in-kernel `Preempt` fires at or after the armed point,
  never before. On arm64 the vCPU also exits on *any* host IRQ, so spurious exits *below* the
  armed point do occur — but they are distinguished by the work counter (`work < arm_point`)
  and re-armed, never mistaken for the overflow. Across all 1.01M armed landings, multiplicity
  held at exactly **one delivery** each: the N1 missed-PMI-on-migration hazard did **not**
  manifest in the pinned (non-migrating) configuration the deterministic contract requires.
- **The step primitive is `KVM_GUESTDBG_SINGLESTEP`, not MTF** (AA-2). `run_until_overflow`
  stops late (below target by the arm-early margin), then `single_step` walks `BR_RETIRED`
  forward to the exact target. The two-level `Exit<A>` already carries `Preempt` as an
  arch-exit; the ARM PMU path fits the *designed* trait without a new method.
- **One contract CLARIFICATION the freeze must carry (AA3-F1), not a trait change.** ARM's work
  clock is `BR_RETIRED` — branch **instructions** (AA1-F1) — so it ticks only on branches and a
  branchless run is a `PC`-**plateau**: a `Moment` named by a work count is a `PC`-*interval*,
  not a point. The exact landing must therefore land at the **canonical representative** of that
  interval — the *first* instruction at which `work == target` (immediately after the target-th
  retiring branch) — which the single-step-up loop reaches from any start strictly below the
  target. The realization is a measured `LANDING_HEADROOM` added to the measured `skid_margin`
  so the `Preempt` fires strictly below the target with room to reach the canonical `PC`; an
  async stop *at* the target (BR-exact but PC-arbitrary) is refused fail-closed. Any backend
  whose work clock is a subset-of-instructions counter (branches, not all-retired) inherits this
  plateau property and must define its landing canonically; a per-retired-instruction counter
  does not. This is a documented property of the `work()`/`run_until_overflow()` contract, not a
  new trait shape. **This is a retained measured conclusion, not a current freeze authorization;
  §D and the `Arch`/`CpuBackend` trait remain designed-but-unfrozen pending re-verification.**

## Pre-build ruling (Paul, 2026-07-13) — build-first; the spike gates trust, not construction

With two vendor boxes incoming on unknown arrival dates (Altra `hm-7pb`, Epyc `hm-9wt`), the
integrator reversed the "no ARM-side building before the spike GOes" cost hedge: **everything
pre-buildable gets built now**, so box-wait converts into worker throughput and arrival day
stays experiment day — with better tooling.

**The risk acceptance, recorded.** The hedge existed because Altra's PMU can fail the AA-1/AA-3
kill conditions. Pre-building reverses it: if the ARM work clock NO-GOes, the ARM-specific
slice (the §D backend/GIC/boot/vendor work, `hm-cbt`) is sunk — a few worker-weeks, accepted
against zero box idle time. The seam restructure (`hm-b5n`) and the paravirt clock (`hm-rk5`)
sit outside that risk: both pay for themselves on x86 regardless of any ARM verdict.

**What the spike still gates (unchanged):** the `Arch`/`CpuBackend` trait *freeze* (AA-3's
trait-freeze memo — pre-built code accepts rework against the unfrozen trait); every measured
constant (`skid_margin`, event density, count offsets — never inherited, never invented);
on-silicon validation and the reach-matrix cell fill itself; and both spike programs'
execution + evidence-integrity discipline, verbatim. GO/NO-GO now decides whether the
pre-built work is *kept and trusted*, not whether it may be written.

**Mechanics.** Pre-build lands via normal task branches and reviewed PRs to main: spike
apparatus under `spikes/arm-altra/` + `spikes/amd-epyc/` (as both programs' offline-work
clauses already sanction), production code as additive crates/modules behind the seam. The
spike-*execution* discipline (dedicated worktree, evidence rules, never-push-during-execution)
is untouched.

**The ruled queue (dispatch order as slots and gates clear):**

1. **This document's steps 1–4** (`hm-b5n`, tasks/108) — the seam + engine/vendor split, the
   single biggest enabler for both vendors. Fable-tier, dispatched 2026-07-13.
2. **Appliance build + host-qualification preflight** (`hm-tn9` / `hm-69y`, ← `hm-l2g` =
   PR #98 lands), with the AA-0/AE-0 capability-truth-table probes absorbed into the preflight
   as machine-readable GO/refuse checks (rider recorded on `hm-69y`).
3. **The paravirt work-derived clock, x86-first** (`hm-rk5`, ← `hm-b5n`) — ratified-to-build;
   perf win + backstop oracle on x86 now, correctness requirement on ARM later (AA-5
   validates the design on silicon).
4. **The harness pre-build lane** — AMD: parameterized exactness-hammer variants dry-run on
   the Intel box + `svm.c` 0004-analogue draft (`hm-8v4`, ← `hm-l2g`); ARM: arm64 oracle
   payloads + minimal KVM harness, aarch64-cross + TCG-smoked, + kvm/arm64 0004-analogue
   draft (`hm-2kj`, tasks/109, dispatched 2026-07-13). Plus the dev-loop probe `hm-8l3`
   (nested KVM in an aarch64 VM on the Mac).
5. **The ARM backend skeleton behind the new seam** (`hm-cbt`, ← `hm-b5n`) — the §D reversal
   proper. Sibling lane: the contract vendor column, AE-4's shape (`hm-0nf`, ← `hm-b5n`).

## Relationship to ARM-PORT.md

Still true and still binding: the hardware table (Spark vs Grace, ECV), the three
load-bearing-mechanism analysis, the rr evidence base, the LL/SC vs LSE hazard, and the gate's
verdict authority — **spike #1 on real silicon decides whether ARM happens.** Its "no D-list
work before GO" sequencing clause is superseded by the pre-build ruling above (2026-07-13):
D-list work may be *built* pre-GO; only the spike can make it *trusted*.

Superseded by this document: the "What a port costs, by component" survey and its premises
("no arch seam exists", "`vmm-core` unwritten", the ~60/40 split) — the audit above replaces
them; and the blanket "do not build the arch abstraction pre-emptively" is now superseded in
two dated steps: the A–C restructure was justified on x86-hygiene grounds alone (2026-07-03 —
it makes the R-Backend boundary compiler-enforced and the product thesis explicit — a
deterministic-execution engine with an x86 backend, not an x86 hypervisor with a bug-finder
attached), and ARM-side *building* was un-gated by the pre-build ruling (2026-07-13). The
trait *freeze* remains spike-gated exactly as ARM-PORT.md demands.
