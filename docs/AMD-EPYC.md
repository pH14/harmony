# AMD vendor spike program — SVM on Epyc

Status: **spike program, authored 2026-07-12; execution gated on hardware arrival.** This is
the AMD sibling of `docs/NESTED-X86.md` and `docs/ARM-ALTRA.md`: a risk-ordered,
GO/NO-GO-gated de-risking program for the **AMD cells of the reach matrix** (vendors × forms —
the Consonance north star, `docs/QUEUE.md`). Target hardware: an incoming **AMD Epyc** server
(exact SKU unknown on arrival — stage AE-0 discovers and pins the microarchitecture and Zen
generation). The doc exists so that the day the box is racked is experiment day: every stage
below is specified to the level of "run this, retain that, decide on these criteria."

Unlike the ARM program, this spike does **not** cross an ISA boundary. Intel and AMD are the
**same architecture** (x86-64); the Arch seam of `docs/ARCH-BOUNDARY.md` does **not** split
them. The vendor split lives one level down — in the **substrate/backend** (VMX → SVM), in the
**contract tables** (AuthenticAMD CPUID leaves and the AMD MSR set), and in the **PMU event
pin** (`ex_ret_brn_tkn` vs Intel's `0x1c4`). The consequence, stated up front because it sets
the whole tone: most of the x86 engine, the boot path, the LAPIC/device model, and the
bare-metal payloads **carry over unchanged** — this spike measures the **substrate deltas**,
not a new architecture. It is closer in spirit to `docs/NESTED-X86.md` (the existing x86 stack
runs; only the layer beneath it changes) than to the ARM program (which had to build
everything new).

Vocabulary note (binding): per the north-star ruling, **"vendor" replaces "personality"**
throughout — where `docs/ARCH-BOUNDARY.md` says "personality," read "vendor"; the
engine/vendor crate-split names stay reserved for exactly this window (`docs/GLOSSARY.md`
§Reserved). `docs/GLOSSARY.md` otherwise governs: `Subject`, `Moment` / `Span`, `Reproducer`,
`state_hash`, V-time as the name of the work-derived clock.

## Read first (binding context)

`docs/NESTED-X86.md` — the x86 sibling whose evidence standards, execution constraints, and
"the whole existing stack transfers" thesis this document inherits; its nested-vPMU program is
transposed nearly verbatim as AE-6 (§5). `docs/ARM-ALTRA.md` — the freshest sibling: this
document **inherits its Evidence-integrity section verbatim as binding stage criteria**
(gate-RC propagation, machine-checked floors, hash-verified boots, mechanism attestation,
independent oracles, multiplicity + totality accounting) and its structural shape (load-bearing
questions → bet/kill conditions → execution constraints → box discipline → stages → decision
ladder → execution packet). `docs/ARCH-BOUNDARY.md` — the ISA seam ruling; it establishes that
Intel↔AMD share the `Arch` vocabulary and that the vendor difference is a **substrate** concern
under the R-Backend seam (`docs/R-BACKEND.md`), not an `Arch` split. `docs/ARM-PORT.md` — the
mechanism-analysis register this document writes in (three-mechanism, rr-evidence-grounded).
`docs/CPU-MSR-CONTRACT.md` — the frozen Intel contract (`det-cfl-v1` baseline); §4 below adds a
vendor column to it, never a fork. `docs/BOX-PINNING.md` — the pinning discipline, which
transfers with a new core map and an SMT caveat the Intel box shares.

## Topology and thesis

```text
AMD Epyc box (bare metal, Zen — generation pinned at AE-0)
└── host: Linux + KVM (kvm_amd / SVM), patched with the AMD determinism analogues
    └── guest: deterministic Subject — the SAME x86-64 guest kernel + payloads as Intel
```

Same thesis as the Intel bare-metal product that ships today: a KVM-based deterministic
hypervisor — same seed ⇒ bit-identical execution, V-time derived from a retired-branch
performance counter, hypercall-only I/O, default-deny guest CPU contract. What changes between
Intel and AMD is **not** the guest-observable ISA — it is the four substrate mechanisms that
sit beneath the engine:

1. the **work event** (`ex_ret_brn_tkn`, retired *taken* branches — a different event than
   Intel's retired *conditional* branches, and its own erratum surface);
2. the **exact-landing primitive** (SVM has **no Monitor Trap Flag** — patch 0005's mechanism
   does not exist and must be replaced);
3. the **force-exit-at-PMI kernel patch** (patch 0004's analogue targets `svm.c`, not
   `vmx.c`, with AVIC in the way);
4. the **CPU contract vocabulary** (AuthenticAMD CPUID space, the AMD MSR set, and a different
   PMU programming model).

`docs/ARCH-BOUNDARY.md`'s audit says ~85% of the tree is already arch-blind, and everything the
Intel↔AMD delta touches lives in the remaining substrate layer — the `WorkSource` event pin,
the two KVM patches, and the contract tables. **No new `Arch` is created**: AMD is the same
x86-64 `Arch`, a second substrate under the existing `Backend`/R-Backend seam, with a
vendor-parameterized PMU event constant and a vendor column in the frozen contract. That is why
this spike is cheap relative to ARM — and why its GO directly unblocks the AMD×metal and
AMD×virtualized reach-matrix cells rather than a whole new backend wave.

The **time-virtualization story is inherited wholesale from Intel and is not a load-bearing
question here.** SVM intercepts RDTSC/RDTSCP (VMCB intercept, APM Vol 2 ch. 15) exactly as VMX
does; RDRAND/RDSEED intercepts likewise exist; the guest's clock is `f(V-time)` via the same
trap-and-map mechanism the Intel backend already implements. There is **no** ARM-style
untrappable-counter crisis — the paravirt work-derived clock (`docs/PARAVIRT-CLOCK.md`,
`hm-8h8`) is a **performance** lever on AMD (removing RDTSC exits from the hot path, exactly as
in `docs/NESTED-X86.md` N-4), never a correctness necessity. This program does not validate it;
it notes the mechanism as an out-of-scope optimization input.

The six load-bearing questions, front and center, in risk order:

## 1. Exact landing without MTF — the hard one

The Intel determinism ABI lands `work == target` by **overflow-early + single-step**: patch
0005 exposes an MTF-based single-step exit (`KVM_ARM_MTF_STEP` → `KVM_EXIT_DET_STEP`), and the
Monitor Trap Flag guarantees a clean, exactly-one-instruction VM-exit with no guest-visible
side effect. **SVM has no Monitor Trap Flag** — MTF is a VMX-only control. The patch-0005
analogue therefore needs a *different* single-step primitive built from AMD architectural
debug facilities, and none of the candidates is a free drop-in. This is the highest-risk
mechanism in the program; it gets the most care, its own stage (**AE-2**), and this ranked
analysis, argued from AMD64 APM specifics rather than by analogy.

The candidates, ranked, each with the APM mechanism and its failure modes:

- **(A) `DebugCtl.BTF` — branch single-stepping (possibly the natural fit).** APM Vol 2 ch. 13
  (Debug and Performance Resources): with `DebugCtl.BTF = 1` and `RFLAGS.TF = 1`, the processor
  raises a `#DB` **only on the next taken branch**, not on every instruction. This is
  potentially the *elegant* answer, because our V-time event **is** the retired taken branch
  (§2): BTF single-stepping lands on exactly the granularity we count, so the "step to the next
  counted event" primitive and the "count one work unit" primitive are the same operation. The
  landing loop needs to advance to a taken-branch boundary near the overflow, which is what BTF
  gives natively. *Failure modes to characterize:* BTF grants **branch** granularity, not
  instruction granularity — landing on a target that falls between two taken branches requires
  a fallback to TF stepping for the residual straight-line stretch; interrupt-shadow and
  `iret`/`mov ss` hazards apply to BTF's `#DB` exactly as to TF's; and whether BTF is honored
  cleanly inside SVM guest context (its `#DB` reaching a VMCB `#DB` intercept deterministically
  across the VMRUN boundary, and its interaction with the guest's own `DebugCtl`) is the
  open empirical question AE-2 answers.
- **(B) `RFLAGS.TF` — classic single-step (the general primitive).** APM Vol 2 ch. 13: `TF = 1`
  raises a trap-type `#DB` after **every** instruction. This is the instruction-granularity
  workhorse, but it carries the exact hazards MTF was designed to avoid, and each is a concrete
  determinism or contract risk here:
  - **Interrupt-shadow deferral.** After `MOV SS`/`POP SS` and `STI`, the single-step `#DB` is
    deferred one instruction (the interrupt shadow, VMCB `GUEST_INTERRUPT_SHADOW`, APM Vol 2
    ch. 15). A stepping loop that assumes exactly-one-instruction-per-`#DB` miscounts across
    these boundaries — the same class of pending-debug subtlety VMX MTF papers over.
  - **`iret` / task-switch / exception-entry boundaries** perturb `TF` and the pending-`#DB`
    state; each must be shown to step exactly once (vs the analytical oracle) or characterized.
  - **Guest visibility of `TF` (a contract leak, not just a counting bug).** `PUSHF` exposes
    `RFLAGS.TF` to the guest, and `POPF` lets the guest clear it. A guest that reads `TF = 1`
    can branch on it → the frozen-contract guarantee ("the guest sees a clean architectural
    state, identical run-to-run") is violated, and worse, a guest that clears `TF` disarms the
    stepper. MTF is invisible to the guest by construction; TF is not. AE-2 must state whether
    SVM lets us **hide** this (intercept `PUSHF`/`POPF`, or a virtualized-`TF` posture) or
    whether it is a recorded contract limitation the frozen guest kernel is built to not depend
    on.
- **(C) `#DB` intercept + DR7 hardware breakpoints (a targeted-landing aid, not a stepper).**
  APM Vol 2 ch. 13 (breakpoints, DR0–DR3/DR7) + ch. 15 (SVM `#DB`/DR intercepts): a hardware
  breakpoint fires a `#DB` at a **known RIP**. Useful to *pin* a landing at a
  statically-known instruction address, but our target is expressed in **work units, not
  addresses** — we do not know the landing RIP a priori, and there are only four DR slots. This
  is an assist for specific landings (e.g., re-arming at a known re-entry point), not the
  general step-N-work primitive. Ranked last as a primary mechanism.
- **(D) Instruction-retired PMC single-stepping (the skid-limited fallback).** Arm a *second*
  PMC (retired instructions / retired micro-ops) to overflow after one, take the PMI, exit.
  *Failure mode, fatal for exactness:* PMC overflow delivery is **skid-prone** — the PMI
  arrives a variable number of instructions late (the same skid we fight for the work clock,
  §2). A stepper built on PMC overflow inherits that skid and **cannot land exactly**. This is
  why it is a fallback of last resort, viable only if A and B both fail, and only in
  combination with a bracket-and-re-measure landing strategy that pays for the skid.

**What decides between them, and where.** AE-2 measures, against analytical oracles (payloads
whose taken-branch and instruction counts are known by construction), whether BTF and TF `#DB`
fire **exactly once** per branch/instruction across straight-line, branch-dense, syscall,
exception-entry/return, `iret`, interrupt-shadow (`STI`/`MOV SS`), and injected-interrupt
boundaries **under SVM guest context**; whether guest-visible `TF` can be hidden or must be
declared a contract note; and whether the natural-fit BTF path (branch granularity + TF residual)
lands `work == target` with no overshoot. The stage's mandatory deliverable is a **ranked
ruling**: which primitive the patch-0005 analogue implements, with the failure modes of the
rejected candidates recorded — "we'll use TF and hope the shadows don't bite" is not a ruling.
The chosen primitive then becomes the single-step half of the exact-landing contract validated
in AE-3.

## 2. The work clock on Zen: `ex_ret_brn_tkn`

V-time on AMD counts **`ex_ret_brn_tkn` — retired *taken* branches** (the Zen PMC event; its
raw encoding, `0xC4`-family, is **pinned per Zen generation at AE-0**, never assumed). Three
facts, one favorable and two demanding:

- **Favorable:** retired-branch counting is **rr-proven on Zen** — rr's production AMD support
  runs on Zen-class cores, so precise branch counting is known to be physically achievable on
  this lineage (the AMD analogue of ARM-PORT's "N1 is the best-characterized aarch64 lineage").
- **Demanding — the SpecLockMap erratum.** rr does not count naively on Zen: locked
  instructions can **overcount** the retired-branch event because of the speculative lock-map
  optimization, and rr's workaround is a host-side **MSR write to `LS_CFG`** (`0xC001_1020`)
  that disables speculative locking. This is the direct AMD analogue of ARM's rr #3607
  missed-PMI-on-migration bug: a documented, mitigable silicon/microcode hazard that turns a
  hygiene knob into a **correctness condition**. AE-1 reproduces it deliberately — measure the
  run-to-run overcount with the workaround **off**, confirm determinism with it **on** — so the
  mitigation is evidence-backed, not folklore, and the `LS_CFG` write is then a **standing
  host-side condition recorded in the box baseline** (the AMD twin of the Intel box's
  revert-KVM-to-stock discipline). It is never silently applied and never silently omitted.
- **Demanding — a different event than Intel's, so no constant is inherited.** `ex_ret_brn_tkn`
  counts *taken* branches; Intel's `0x1c4` (`BR_INST_RETIRED.CONDITIONAL`) counts *conditional*
  branches. This is the **same semantic family as ARM's `BR_RETIRED`** (taken branches) and
  **different physics** from Intel. The `CpuBackend` trait contract (monotonic,
  0-or-1-per-instruction `u64` — `docs/ARCH-BOUNDARY.md` verified it ports unchanged) holds, but
  the ARM re-measurement discipline applies verbatim: **every `skid_margin`, event-density, and
  count-offset constant is re-measured on Zen and never inherited from Intel.** The Intel
  `skid_margin` is planning folklore here until AE-1/AE-3 produce the Zen numbers;
  `SimCpu`/`PlannerConfig` is re-parameterized from the measured density table, not copied. And
  because the event and its encoding differ **per Zen generation**, AE-0 pins the event identity
  for the delivered silicon as a first-class deliverable — a Zen 3 encoding is not assumed on a
  Zen 4 part, and PerfMonV2's counter model (below) is discovered, not presumed.

## 3. The 0004-analogue on the SVM side of KVM

The Intel force-exit patch (0004: guest-mode work-counter overflow → deterministic in-kernel
vCPU exit with a dedicated reason, `KVM_ARM_PREEMPT_EXIT` → `KVM_EXIT_PREEMPT`) lives in
`arch/x86/kvm/vmx/vmx.c`. Its AMD analogue lives in **`arch/x86/kvm/svm/svm.c`** and is **real
kernel patch work** (stage **AE-3**), not a nearly-free port:

- **The patch shape largely transfers**, because the determinism ABI — arm a work-counter
  overflow, convert the resulting PMI into a deterministic exit before any guest-visible side
  effect, hand control to the vmm — is substrate-agnostic above the interrupt-delivery detail.
  What changes is the PMI→exit plumbing: SVM delivers the counter-overflow interrupt through the
  local-APIC `LVTPC`, and the in-kernel force-exit hook attaches at SVM's exit/`VMRUN` boundary
  rather than VMX's. AE-3 builds the `svm.c` hook, gives it the same dedicated deterministic
  exit reason, and content-pins the patched `kvm_amd` module.
- **AVIC is in the way, and likely gets disabled — say so and why.** AMD's Advanced Virtual
  Interrupt Controller (AVIC, APM Vol 2 ch. 15) accelerates interrupt delivery **in hardware,
  bypassing the VM-exit** that our force-exit path depends on and moving interrupt state into a
  hardware-managed backing page. A deterministic force-exit at PMI needs the overflow interrupt
  to **reach the host exit path**, which AVIC is designed to avoid. The expected posture is
  therefore **AVIC disabled** for the deterministic backend (as the Intel design already keeps
  the interrupt fabric in userspace for determinism). Per the bet (below), **"works only with
  AVIC off" is a recorded limitation, not a NO-GO** — AVIC is a performance accelerator, not a
  correctness dependency, and disabling it costs interrupt-delivery latency the deterministic
  design already forgoes on Intel. AE-3 records the AVIC posture in every run's evidence and
  attests that the overflow took the intended exit path.

AE-3's data is what `docs/ARCH-BOUNDARY.md` deferred the trait freeze for (does the SVM
PMI-delivery path pressure `run_until_overflow`'s late-only-stop contract). The stage's
deliverables include an explicit **trait-freeze memo**.

## 4. Contract deltas: a vendor column, never a fork

The frozen Intel contract (`docs/CPU-MSR-CONTRACT.md`, `det-cfl-v1` baseline — a Coffee Lake
Core i9-9900K) is the **rigor template, and most of its content transfers** because the ISA is
shared; the deltas are concentrated and enumerable:

- **AuthenticAMD CPUID.** The vendor string (`CPUID.0:EBX/EDX/ECX = "AuthenticAMD"`) and the
  **extended leaf space `0x8000_0000`–`0x8000_00xx`** carry AMD's feature and topology
  enumeration where Intel uses standard leaves; the synthetic frozen model becomes a
  `det-zenN-v1` baseline (name pinned once the AE-0 generation is known) with AMD's leaves
  frozen and everything unlisted default-denied, exactly as `det-cfl-v1` does for Intel.
- **The AMD MSR set.** AMD-specific MSRs (`0xC000_00xx`/`0xC001_00xx` space — e.g. the
  `LS_CFG` of §2, `HWCR`, the SVM `VM_HSAVE_PA` that the Intel contract already default-denies
  at `0xc0010117`) get explicit dispositions. Today `docs/CPU-MSR-CONTRACT.md` denies the AMD
  and SVM MSRs as out-of-scope on the Intel baseline; the AMD column **flips the relevant rows
  to enumerated/allowed under an AuthenticAMD baseline**, with the same read/write disposition
  vocabulary.
- **The PMU programming model — PERF_CTL/PERF_CTR vs PERF_GLOBAL_CTRL.** Intel programs its PMU
  through `IA32_PERF_GLOBAL_CTRL` (one register gates all counters); AMD's legacy model has
  **no global control** — each counter is enabled by the `EN` bit in its own `PERF_CTL`
  select MSR (`0xC001_020x` core-perf-counter pairs). AE-0 discovers whether the delivered part
  has **PerfMonV2** (Zen 4+), which *adds* Intel-like global control/status MSRs — the counter
  model is therefore a **per-generation** contract fact, not an AMD constant. This delta touches
  both the contract (which MSRs the guest may observe) and the work-counter arming path.
- **Enforcement mechanism.** SVM's MSR intercept is the **MSR permission bitmap** in the VMCB
  (APM Vol 2 ch. 15), the AMD equivalent of Intel's `KVM_X86_SET_MSR_FILTER` surface; CPUID
  interception is via the VMCB `CPUID` intercept. The *dispositions* stay above the seam (the
  `contract/*` data-driven table→model→enforce shape, `docs/ARCH-BOUNDARY.md` §B); only the
  enforcement backend changes.

The rule, cross-referenced to `docs/GLOSSARY.md`: this is a **vendor column on the one frozen
contract, not a second forked document**. The enforcement machinery stays a single
data-driven pipeline with a vendor axis; the Reproducer artifact is never forked (the
never-fork-the-one-reproducer rule). AE-4 delivers the **enforcement-mechanism truth table**
(each contract row → the SVM trap/freeze that enforces it, or recorded as undeniable on this
silicon with a disposition); the full AMD contract document is downstream port work, not this
spike.

## 5. Nested SVM as the virtualized-form cell (deferred, gated on bare-metal GO)

The AMD×virtualized reach-matrix cell is served by **nested SVM**, and here AMD is in a
*stronger* position than ARM: KVM's **nested SVM is mature** (unlike still-maturing nested
arm64, and unlike ARM's total absence of nested-virt hardware on N1). The `docs/NESTED-X86.md`
program shape — the consonance stack as an L1 guest of a stock-KVM L0, three mechanisms
surviving by construction and the vPMU-through-one-layer bet as the empirical unknown —
**transposes nearly verbatim** to nested SVM, including its central bet:

> the vPMU L0 exposes to L1 counts only L2 work (`ex_ret_brn_tkn`, guest-filtered) with
> deterministic semantics, and its overflow PMI reaches L1's patch-0004-analogue force-exit
> reliably with bounded skid.

This is **deferred to stage AE-6, gated on the bare-metal GO (AE-5)** — the same sequencing the
reach matrix uses everywhere (virtualized-form cells follow their bare-metal parent). It is
never an assumption in the bare-metal stages, and no bare-metal stage may cite it.

## 6. Fallbacks and siblings

- **Rentable AMD metal is the zero-procurement fallback and the second-microarch data point.**
  Hetzner's AMD lines (Ryzen/EPYC dedicated servers, hourly/monthly) are real bare-metal SVM
  with vPMU access — the same provider family as the existing Intel determinism box (`ssh
  hetzner`). If the incoming Epyc slips or dies, AE-0/AE-1 run on a rented AMD box unchanged; on
  Epyc GO, a bounded AE-1 re-run on a *different* Zen generation (rented) is the cheap
  confirmation that the constants are Zen-lineage-stable versus SKU-specific — decision input
  for every future AMD host class, and the natural place the SpecLockMap/PerfMonV2 per-generation
  deltas (§2, §4) get their second data point.
- **The qualification harness carries over from the doctor/preflight work when it lands.** The
  host-capability qualification the doctor/preflight effort is building (the "does this box
  expose what the deterministic backend needs" probe) is exactly what AE-0's truth table
  demands; when that harness exists it supplies AE-0's capability enumeration rather than a
  bespoke script. This is a **recorded dependency, not an assumption** — AE-0 ships its own
  probe if the harness is not yet available.
- If the **work clock itself** fails on Zen (AE-1/AE-3 NO-GO), the fallback ladder mirrors the
  x86/ARM programs: (a) re-run on a second Zen generation (rented AMD metal) before concluding
  the failure is AMD-wide; (b) software work counter inside the owned guest kernel; (c) the
  deterministic-emulation replay tier. Fallbacks are recorded, not built, in this spike.

## The bet and its kill conditions

The AMD vendor thesis is **NO-GO on this silicon** if any of these survives the bounded
experiments and reasonable redesigns:

- equal guest instruction streams produce different `ex_ret_brn_tkn` counts on a pinned core
  (with the SpecLockMap/`LS_CFG` workaround applied — its *absence* is a known overcount, not a
  thesis failure);
- work-counter overflow PMIs can be lost, duplicated, or delayed without a defensible empirical
  bound;
- **no single-step primitive lands `work == target` exactly** — BTF and TF both prove
  unusable under SVM (miscount across interrupt-shadow/`iret`/injection boundaries, or
  guest-visible `TF` cannot be hidden *and* the guest depends on it) and the PMC-step fallback
  cannot bracket the skid;
- the 0004-analogue in `svm.c` cannot convert overflow into a deterministic exit;
- a guest-visible CPUID leaf or MSR that reaches state cannot be frozen or trapped on this part.

One unexplained count mismatch is blocking. **"Works only with AVIC off" is a recorded
limitation, not a NO-GO** (AVIC is a performance accelerator the deterministic design forgoes
anyway); likewise "requires the `LS_CFG` workaround" is a recorded standing condition, not a
failure. Never convert NO-GO into GO by relaxing "bit-identical," accepting a wall-clock
dependency, counting unverified or missing samples as successes, or quietly substituting a
different event, counter, kernel, or single-step mechanism than the one the ruling names.
Unsupported is a result.

## Definition of done

Not a feasibility essay. Because the x86 stack already exists (§Topology), the terminal
deliverable can go all the way to a working demo — closer to `docs/NESTED-X86.md`'s N-5 than to
the ARM program's mechanism-only exit:

1. dispositions (GO / PROVISIONAL GO / REDESIGN / NO-GO) with retained machine-readable evidence
   for stages AE-0 through AE-6;
2. the **measured-constants pack**: `ex_ret_brn_tkn` count offsets per payload class, the Zen
   `skid_margin`, the event-density table (the `SimCpu`/`PlannerConfig` re-parameterization
   inputs), the pinned per-generation event encoding, and the single-step semantics notes;
3. the **single-step ruling** (§1) — which primitive the patch-0005 analogue uses, with the
   rejected candidates' failure modes recorded;
4. the **trait-freeze memo** to `docs/ARCH-BOUNDARY.md` (does `run_until_overflow`'s
   late-only-stop contract hold on SVM PMI delivery; what — if anything — the trait must
   absorb);
5. the **contract vendor-column skeleton** and its enforcement-mechanism truth table (§4);
6. on bare-metal GO (AE-5): **one documented command** that, from a fresh checkout on the box,
   builds the content-pinned AMD-patched stack (patched `kvm_amd`, the guest images), boots the
   subject, and passes the same-seed determinism gate end-to-end; and on AE-6 GO, its nested
   twin;
7. the Epyc box's standing core assignments and baseline manifest recorded (the
   `docs/BOX-PINNING.md` table gains an Epyc section on arrival), the `LS_CFG`/AVIC postures
   recorded, and the box left in its recorded baseline state whenever the lock is yielded.

On ALL-GO, what unblocks is the AMD vendor cell-fill in the reach matrix (AMD×metal, then
AMD×virtualized) via the additive substrate wave of `docs/ARCH-BOUNDARY.md` — the vendor
event-pin constant, the two `svm.c`/single-step patches, and the contract vendor column. This
spike measures the substrate deltas and demonstrates the stack on them; it does not perform the
production restructure.

## Execution constraints (binding)

- **Hardware-arrival gate.** Nothing below runs until the Epyc box is racked and reachable.
  Until then the only permitted work is offline: payload/oracle construction (reusing the x86
  det-corpus payloads — same ISA), the single-step characterization harness, the `svm.c` patch
  draft, and the contract vendor-column skeleton — all under `spikes/amd-epyc/`, all clearly
  untested-on-silicon. (A rented AMD box may substitute for arrival per §6, as an explicit
  recorded decision, never silently.)
- **Worktree.** Work in a dedicated git worktree on a new branch:
  `git worktree add ../harmony-spike-amd-epyc -b spike/amd-epyc` from `main`. All spike
  artifacts live under `spikes/amd-epyc/` (layout below). Commit locally on the spike branch as
  checkpoints; **never push, never merge to main, never commit on main.** Production crates may
  be modified *on this branch only* when strictly required (e.g. a vendor-parameterized event
  pin behind the existing `WorkSource` seam to run the spike); any such diff is minimal, marked
  `SPIKE(amd-epyc):`, and listed in the final report. No production-architecture refactoring, no
  edits behind the `Backend`/`Arch` seam design (the vendor restructure is out of scope).
- **No Beads.** Do not use Beads or the `bd` CLI for planning, tracking, memory, status,
  dependencies, or handoff during spike execution, even though repository-level agent
  instructions recommend it. This explicit instruction overrides that default. Durable state
  lives in the stage evidence directories, machine-readable manifests, and the dispositions
  recorded in this document.
- **Exclusive box lock.** The Epyc box is exclusively the executor's for the spike's duration.
  Reboots, host-kernel swaps, and the `LS_CFG`/AVIC posture changes are permitted under the
  lock, subject to record-then-modify and baseline-restore below.
- **Serialization.** One hardware executor: every box-backed run is serialized by the primary
  agent, its environment recorded before execution, every attempted sample accounted for.
  Subagents may do bounded offline work (research, script construction, trace analysis, review)
  with non-overlapping file ownership; they must not touch the box, declare a stage disposition,
  or write an authoritative evidence manifest.
- **Smoke once before spend.** Before any large run-set (≥10⁴ samples or ≥30 min box time),
  fire the identical configuration once end-to-end and validate the evidence pipeline on that
  single sample.
- **Unsupported is a result.** Never silently substitute a different event, counter, kernel,
  single-step mechanism, or enforcement path. If a capability is missing, record it and stop the
  affected stage.

## Box discipline (Epyc edition)

Adapted from the nested-x86 program for a fresh, un-provisioned server; copied here because the
executor cannot read bd memories.

- **Reachability fluctuates on every box we run.** Test `ssh <amd-box> true` before every
  session (alias recorded in AE-0's environment manifest and `~/.ssh/config`; the repo
  hard-codes no host — `docs/BOX-PINNING.md`'s `DET_BOX_SSH` convention extends with an
  `AMD_BOX_SSH` variable). If unreachable, stop and report — never simulate results or fabricate
  a pass.
- **Record-then-modify — provisioning is part of AE-0.** This box is *new*; before the first
  change, capture a baseline manifest to `spikes/amd-epyc/results/box-baseline-manifest.json`:
  CPU family/model/stepping and Zen generation, microcode revision, running kernel, kvm/kvm_amd
  module identity (stock vs patched), cmdline, governor, **AVIC posture**
  (`kvm_amd avic=` parameter), **`LS_CFG` state** (the SpecLockMap workaround, §2), SMT posture,
  core topology, and any services touched. The baseline captured on day one **is** the restore
  target; whenever the lock is yielded (and at spike end), return the box to a recorded state
  and verify the match. If the box is to become a standing AMD determinism host, its post-spike
  posture is recorded as a new baseline, explicitly, never implicitly.
- **Image content discipline.** Reference every bootable artifact **by content hash**: pin
  sha256 (+md5 cross-ref) in the harness and verify **immediately before every boot**, host
  kernels and the patched `kvm_amd` module included. Never trust a mutable path. The pattern to
  reuse is `vmm-core/tests/live_dirty_remap.rs` (`guest_images()` / `verify_pin`); the known-good
  x86 guest images (the pr44 postgres pair) carry over unchanged — same ISA, same Subject.
- **pkill/pgrep landmine.** `pgrep -f`/`pkill -f` self-match wrapper argv — harness suicide and
  waiter deadlocks have occurred on the Intel box. Use separate write and launch ssh calls,
  redirect stdin (`</dev/null`), launch long-running processes detached (`setsid`/`nohup`), and
  use **state-based waits** (poll for a file/socket/pidfile), never `pkill -f`-based
  interrogation of your own command lines.
- **Core pinning + the SMT caveat.** Unlike the ARM N1 (single-threaded cores), **Epyc cores
  are SMT-2** — the sibling-hyperthread confound class the Intel box also has is **present
  here**. Pin every measurement to a dedicated physical core **and idle or offline its SMT
  sibling**; record the pinned core, its sibling's state, governor, and frequency posture in
  every run's evidence (V-time counts are frequency-independent — frequency hygiene matters only
  for wall-clock numbers; the SMT sibling matters for count invariance). AE-0 establishes and
  records the standing core-assignment table (housekeeping / measurement / guest cores, with
  sibling map) for the new box.
- **Pinning + `LS_CFG` are load-bearing here** (§2). The vCPU thread and its perf context stay
  hard-pinned for every sample of every stage, and the SpecLockMap workaround (`LS_CFG` write)
  is applied and attested per run. The one sanctioned deviation is AE-1's bounded
  workaround-**off** probe, which deliberately reproduces the overcount to evidence the
  mitigation, then re-applies it permanently.

## Evidence integrity (binding — the PR-98 lesson)

The nested-x86 spike's review (2026-07-12, PR #98) found harnesses that could report green on
failed gates, dispositions whose acceptance floors were not met by the retained evidence, and an
existential-stage harness that silently exercised the stock fallback instead of the patched
mechanism. These countermeasures are therefore **mandatory acceptance criteria of every stage
below** — a stage without them cannot be GO regardless of its numbers:

1. **Gate-RC propagation.** A harness's success condition is the machine-propagated conjunction
   of every constituent gate's exit status. A done-marker, completion print, or "reached the
   end" condition is **never** a success condition.
2. **Machine-checked floors.** Every numeric acceptance floor (sample counts, rep counts,
   zero-mismatch claims) is checked by a script **against the retained evidence records** —
   recomputed from the raw per-sample data, not read from a summary line the harness itself
   asserted. The disposition may not be written until the checker passes; the checker's output is
   itself retained evidence.
3. **Content-hash-verified boots.** Every boot artifact (host kernel, patched `kvm_amd`, guest
   kernel, payload images, initramfs) is sha256-verified **immediately before execution** —
   verification is a gate, not a log line. Recording a hash without verifying it is the
   anti-pattern this rule exists to kill.
4. **Mechanism attestation.** Every stage proves, in-band and per-run, that the *claimed*
   mechanism was exercised: patched-vs-stock `kvm_amd` identity, the deterministic exit reason,
   the AVIC-off posture, the single-step primitive actually armed (BTF vs TF vs PMC-step)
   asserted in the evidence as part of the stage's own acceptance. A silent fallback path
   (signal-kick instead of the 0004-analogue exit, TF-step masquerading as the ruled BTF
   primitive) must be structurally unable to masquerade as the mechanism under test.
5. **Independent oracle.** Count-exactness claims are judged against **analytically constructed
   payload oracles** (payloads whose taken-branch and instruction counts are known by
   construction), never PMU-vs-PMU comparison, which is circular.
6. **Multiplicity + totality accounting.** Overflow/PMI delivery claims are established from
   per-record multiplicity (exactly-once shown from the records, not inferred from totals), and
   **every attempted sample appears in the evidence** — a missing sample is a failure to
   account, not a pass. Unsupported is a result.

Evidence manifests are machine-readable (stable JSON, sorted keys), written by the harness,
never handwritten from terminal output. Raw volume too large for git is content-addressed with a
checked-in manifest, summary, and reproduction command. Golden evidence is immutable; reruns
create a new run-set.

## Spike architecture

All under `spikes/amd-epyc/`:

1. **Host prep** (`host/`) — box baseline/restore scripts (incl. `LS_CFG` and `avic=` posture
   capture/apply), patched-host-kernel + patched-`kvm_amd` build recipe (AE-3 onward), pinned
   environment capture.
2. **Payloads + oracles** (`payloads/`) — the x86 oracle payloads with analytically known
   taken-branch and instruction counts, per class (straight-line, branch-dense, syscall,
   exception, `iret`, interrupt-shadow (`STI`/`MOV SS`), HLT/idle, locked-instruction — the
   SpecLockMap probe class). Reuses the existing det-corpus/contract payloads (same ISA) where
   they already carry analytical oracles; new-by-purpose only for the AMD-specific probe classes.
3. **Harness** (`harness/`) — the minimal KVM harness (single vCPU, pinned, ioctl-level, SVM);
   the single-step characterization driver (BTF/TF/`#DB`); run orchestration.
4. **Evidence** (`schemas/`, `results/<stage>/<run-set>/`) — canonical machine-readable results
   plus the floor-checker scripts of §Evidence integrity.

Every run records at least: CPU family/model/stepping + Zen generation + microcode, host kernel
+ kvm_amd module identity (stock vs patched, with hashes), guest/payload image hashes (verified
pre-boot), perf event configuration (the pinned per-generation `ex_ret_brn_tkn` encoding, pinned
core, exclusion flags), `LS_CFG`/SpecLockMap posture, AVIC posture, single-step primitive armed,
core pinning map + SMT-sibling state, governor, experimental condition, all counter values,
targets, overflow records with multiplicity, skid, landed state, and result digests.

## Risk-ordered stages

Each stage: question / method / acceptance / stop. The §Evidence-integrity criteria are part of
every stage's acceptance implicitly and are not restated per stage.

### AE-0 — day-one bring-up + Zen-generation capability truth table

**Question:** What exact AMD part arrived, and does it expose what this program assumes?

Method: capture the baseline manifest (§Box discipline). Record, from real silicon, a
machine-readable truth table:

- identity: CPU family/model/stepping, **Zen generation**, SoC part, core/thread count,
  microcode revision, firmware/kernel versions;
- **the pinned `ex_ret_brn_tkn` event encoding for this generation** (§2) — verified openable as
  a pinned, non-multiplexed `perf_event_open`, with a trivial host-side overflow test delivering
  a sample/signal; the SpecLockMap/`LS_CFG` MSR present and writable;
- PMU model: legacy per-counter `PERF_CTL`/`PERF_CTR` vs **PerfMonV2** (Zen 4+) global
  control/status — recorded as the per-generation contract fact of §4;
- SVM: `/dev/kvm` + `kvm_amd` present; SVM enabled in firmware; VMCB feature surface
  (NRIP save, DecodeAssist, LBR virtualization, MSR-permission-bitmap), **AVIC present and its
  `avic=` posture**, `#DB`/DR intercept support, RDTSC/RDTSCP/RDRAND/RDSEED intercept controls;
- single-step facilities: `DebugCtl.BTF` present, `RFLAGS.TF` behavior, DR7 breakpoint count —
  the AE-2 candidate surface;
- topology: the standing core-assignment table for this box chosen and recorded, **with the
  SMT-sibling map** (feeds a `docs/BOX-PINNING.md` Epyc section on the port branch, later).

**Acceptance:** truth table complete and machine-readable; byte-identical across two reboots;
every "expect" row confirmed or recorded as a deviation with an explicit disposition (a
*favorable* deviation — e.g. AVIC absent, or PerfMonV2 present — still requires a recorded ruling
before any stage relies on it); the per-generation event encoding pinned.

**Stop:** no KVM/SVM, no usable PMU, or `ex_ret_brn_tkn` absent/unopenable → NO-GO for this box
with the capability diff recorded; the program moves to the rented-AMD fallback (§6).

### AE-1 — the work clock: count exactness, PMI reliability, skid, SpecLockMap (the existential trio)

**Question:** Is `ex_ret_brn_tkn` counting bit-deterministic on a pinned Zen core (with the
SpecLockMap workaround), do overflow PMIs arrive reliably out of `KVM_RUN`, and what is the skid
bound?

This is the highest-value measurement of the program; nothing may displace it. All counting is
judged against the analytical oracle (§Evidence integrity #5). Sub-experiments:

- **(a) Host-side exactness:** pinned EL0/CPL3 counting of oracle payloads across classes
  (straight-line, branch-dense, syscall, signal, page-fault), differentially across 1e6/1e7/1e8
  scales. Expected shape is oracle + a small constant offset (the Intel analogue measured n+2);
  the offset is *measured and pinned per class*, and a variable offset is a mismatch, not a
  calibration.
- **(b) Guest-mode exactness:** the minimal SVM KVM harness runs the oracle payloads on a pinned
  vCPU; count guest-only (host-excluded attribution); equal streams → equal counts, vs oracle;
  across payload classes including HLT/idle and injected-interrupt classes; repeated after a host
  reboot.
- **(c) The SpecLockMap probe (deliberate, the AMD signature):** the locked-instruction payload
  class run with `LS_CFG` workaround **off** — reproduce and quantify the run-to-run overcount
  — then **on** — confirm determinism restored. This turns the standing workaround condition into
  evidence (the AMD twin of ARM AA-1's migration probe). Then the workaround stays on
  permanently.
- **(d) Overflow + skid:** sampling-mode overflow with a kick out of `KVM_RUN` (the pre-patch
  mechanism — a host-side signal to the vCPU thread; AE-3 moves this in-kernel); every armed
  overflow delivered exactly once, shown per-record (§Evidence integrity #6); the early/late
  skid distribution measured → the candidate **Zen `skid_margin`** and the event-density table
  (§2's re-measured constants).
- **Contamination probes:** co-tenant load on other cores, then on the same core and its **SMT
  sibling**, memory pressure; count invariance required (wall clock may move; counts may not).

**Acceptance (PROVISIONAL GO threshold):** zero count mismatches and zero missed/duplicate
overflows over **≥10⁶ armed overflows cumulative** across the condition matrix (workaround on),
stable per-class count offsets, a stable skid bound, and the SpecLockMap overcount reproduced
off / eliminated on; the measured `skid_margin` and density table recorded as the constants pack
(§Definition of done #2). Report confidence and coverage; do not call it a proof.

**Stop:** one unexplained mismatch with the workaround applied, or PMI loss that pinning does not
eliminate → NO-GO for the Zen hardware work clock; record which fallback the evidence selects
(§6): second-Zen-generation re-measurement before an AMD-wide conclusion, then software work
counter or emulation tier.

### AE-2 — single-step exactness without MTF (the hard one)

**Question:** Can a `#DB`-based single-step primitive (BTF or TF) deliver deterministic,
work-exact stepping under SVM, and which one does the patch-0005 analogue use?

This is §1's stage — the highest-risk mechanism. Method: stock `kvm_amd`, pinned vCPU, oracle
payloads. Characterize, against the analytical oracle, for **both** `DebugCtl.BTF` (branch
granularity) and `RFLAGS.TF` (instruction granularity):

- exactly-one-event-per-`#DB` across straight-line, branch-dense, syscall, exception
  entry/return, `iret`, **interrupt-shadow** (`STI`/`MOV SS`/`POP SS`), and injected-interrupt
  boundaries — no skipped or doubled steps, `#DB` reaching the VMCB intercept deterministically
  across the VMRUN boundary;
- **guest visibility of `TF`** (§1 (B)): whether SVM lets us hide it (intercept `PUSHF`/`POPF`,
  or a virtualized-`TF` posture) or whether it is a recorded contract limitation — tested, not
  asserted;
- the **BTF-plus-TF-residual landing** (§1 (A)): does branch-granularity BTF stepping plus a TF
  step for the sub-branch residual land `work == target` with no overshoot;
- and — deliberately — the interaction with **locked/atomic sequences** and the
  interrupt-shadow window, where the hazard is a deferred or doubled `#DB`.

**Acceptance:** exact step counts vs oracle across all classes for the chosen primitive;
replay-identical stepped states; the guest-`TF`-visibility disposition recorded; and **the
ranked single-step ruling written** (§Definition of done #3) — which primitive AE-3 implements,
with the rejected candidates' failure modes recorded. "TF and hope" is not a ruling.

**Stop:** neither BTF nor TF steps deterministically under SVM, or guest-visible `TF` cannot be
hidden *and* the frozen guest depends on it → the patch-0005 analogue is not achievable from the
debug facilities; fall to the PMC-step bracket strategy (§1 (D)) with its skid cost measured, or
REDESIGN; if no primitive lands exactly, this is the §Bet's single-step kill condition.

### AE-3 — deterministic force-exit at PMI (the 0004-analogue on `svm.c`) + exact landing

**Question:** Can a patched `kvm_amd` (`svm.c`) convert a work-counter overflow into a
deterministic in-kernel vCPU exit, and does overflow-early + the AE-2 single-step primitive land
`work == target` exactly, with AVIC in its recorded posture?

Method: the real patch work. Build the `svm.c` analogue of patch 0004 — guest-mode work-counter
overflow → in-kernel vCPU kick with a dedicated deterministic exit reason — on the recorded host
kernel, **AVIC disabled** (§3); content-pin the patched modules. Then drive the full landing
contract (`run_until_overflow` + `single_step` via the AE-2 primitive, the `CpuBackend`
inversion) against seeded-random targets: deltas 1..100k; overflow-early / skid-bracket /
pure-overflow classes interleaved; across payload classes including targets adjacent to counted
and uncounted instructions and on both sides of exceptions and interrupt shadows. **Mechanism
attestation is load-bearing here** (the PR-98 failure was exactly this stage's x86 twin silently
testing stock fallback): every landing's evidence must assert the patched exit reason, the
patched-`kvm_amd` identity, the AVIC-off posture, and the single-step primitive actually armed,
and the harness must be structurally unable to fall back to the AE-1 signal-kick or a different
step primitive and still pass.

**Acceptance:** **≥10⁶ armed deadlines cumulative** with `work == target` on every landing,
never overshoot, replay-identical landed-state digests; skid never exceeding the AE-1 margin (a
violation triggers an explicit rerun/ruling — the margin is never silently enlarged); the
**trait-freeze memo** written (does late-only-stop hold on SVM PMI delivery; what, if anything,
the `Arch`/`CpuBackend` design must absorb — `docs/ARCH-BOUNDARY.md`'s deferred decision).

**Stop:** PMI-to-exit cannot be made deterministic in `svm.c`, or landing overshoots irreducibly
→ NO-GO for the hardware work-clock thesis on Zen; fallback ladder as in AE-1.

### AE-4 — contract vendor column + enforcement truth table

**Question:** Can the guest-visible CPU surface be frozen and enforced on this AMD part as a
vendor column on the one contract?

Method (§4, on real artifacts):

- **(a) AuthenticAMD freeze:** install a synthetic `det-zenN-v1` frozen CPUID/MSR model
  (AuthenticAMD vendor string, the `0x8000_00xx` extended leaves, the AMD MSR set) via the VMCB
  CPUID intercept and the **MSR-permission bitmap**; verify the guest sees frozen values
  (including feature bits *below* host capability), and that unlisted leaves/MSRs default-deny.
- **(b) PMU-model enforcement:** the per-generation counter model (legacy `PERF_CTL`/`PERF_CTR`
  vs PerfMonV2 global control) mapped to trap dispositions — guest PMU reads/writes fault (the
  `RDPMC`→#GP analogue); RDTSC/RDTSCP/RDRAND/RDSEED intercepts confirmed reaching the vmm.
- **(c) The enforcement-mechanism truth table:** every planned contract row mapped to a
  demonstrated VMCB trap/freeze, or recorded as undeniable on this silicon with a disposition.
  Deliverable: the vendor-column skeleton (§Definition of done #5) — a column on
  `docs/CPU-MSR-CONTRACT.md`, **not a forked document** (§4, `docs/GLOSSARY.md`).

**Acceptance:** freeze demonstrated including below-host feature bits; PMU/RDTSC/RDRAND
enforcement demonstrated; truth table complete; vendor-column skeleton recorded.

**Stop:** a guest-visible CPUID leaf or MSR that reaches state cannot be frozen or trapped on
this part → REDESIGN (respecify the row) or NO-GO with the gap named.

### AE-5 — the bare-metal mini determinism gate (the AMD×metal GO)

**Question:** Does the whole mechanism stack hold its determinism claim on bare-metal AMD?

Method: same seed twice → bit-identical `state_hash`, console, and event evidence, on the SVM
harness, over the payload matrix **plus** the existing x86 Subject (the hash-verified postgres
pair — same ISA, unchanged), with events injected at seeded-random `Moment`s — the whole
mechanism stack (work clock + `LS_CFG`, exact landing via the ruled single-step primitive, the
`svm.c` force-exit, AVIC-off, the AuthenticAMD frozen contract) exercised together. Then the
**one-command demo** (§Definition of done #6): from a fresh checkout on the box, build the
content-pinned AMD-patched stack, boot the Subject, pass the same-seed gate, emit the evidence
bundle. This is the AMD×metal reach-matrix cell demonstration.

**Acceptance:** **≥1,000 same-seed repetitions bit-identical**, every attempted sample accounted
for, floors machine-checked against the retained records; the one-command demo runs green from a
fresh checkout with a complete hash manifest.

**Stop:** any silent divergence → P0 (never serialize-to-hide, per the task-69 M2 principle);
diagnose within the thesis or record the NO-GO with the mechanism named.

### AE-6 — nested SVM (the AMD×virtualized cell; gated on AE-5 GO)

**Question:** Does the same AMD-patched stack hold determinism as an L1 guest of stock-KVM
nested SVM?

**Gated on the AE-5 bare-metal GO.** Method: transpose `docs/NESTED-X86.md`'s N-0..N-3 program
to nested SVM (mature in KVM, §5) — flip L0 to stock `kvm_amd nested=1`, boot the AMD-patched
consonance stack as L1, and run the existential trio (count exactness / overflow delivery /
exact landing) and the full same-seed gates nested, including the **portability gate**: the
nested `state_hash` must equal the AE-5 bare-metal `state_hash` for the same seed/images/vmm.
The vPMU-through-one-layer bet (§5) is the empirical unknown; the three by-construction survivors
(trap closure, single-step, nested paging) transfer from the Intel nested analysis with SVM
substituted.

**Acceptance:** the existential trio clean nested; ≥1,000 same-seed full-gate reps bit-identical;
nested==metal hash equality demonstrated; every sample accounted for.

**Stop:** L0 contaminates the L2 work count without a filterable/correctable bound, or the vPMU
is multiplexed undetectably → NO-GO for the nested AMD cell (the bare-metal cell stands); record
the fallback tier (`docs/NESTED-X86.md`'s ladder: software counter / ring-3 substrate /
emulation replay).

## Decision ladder

Each stage ends with exactly one recorded disposition:

- **GO** — acceptance met; next stage may begin.
- **PROVISIONAL GO** — evidence clean but bounded; the limitation is named and re-stressed at a
  later stage (AE-5's mini gate is the default re-stress point). "Requires `LS_CFG`" and "AVIC
  off" are recorded standing conditions under this label, not NO-GOs.
- **REDESIGN** — achievable with a named change inside the same bare-metal AMD/SVM thesis
  (different single-step primitive, different arming strategy, injection-boundary discipline,
  enforcement-level change); repeat the stage.
- **NO-GO** — a required hard mechanism is absent on this silicon. Record which fallback the
  evidence selects: (a) a second Zen generation (rented AMD metal, §6) before any AMD-wide
  conclusion, (b) software work counter inside the owned guest kernel, or (c) the
  deterministic-emulation replay tier. Fallbacks are recorded, not built, in this spike.

Out of scope for this spike: the production vendor restructure (`docs/ARCH-BOUNDARY.md`'s
substrate wave — the event-pin constant, the two patches, the contract column as *production*
code), the full AMD CPU-contract document (AE-4 delivers its enforcement truth table and column
skeleton only), the paravirt work-derived clock (an AMD performance lever, not validated here;
`docs/PARAVIRT-CLOCK.md`), any rented-AMD execution beyond the recorded fallback/confirmation
runs, and ARM/Intel cross-vendor work — those are their own documents.

## Repository layout

```text
spikes/amd-epyc/
├── README.md            # commands, environment, current dispositions
├── host/                # baseline/restore scripts (LS_CFG/AVIC posture), patched kvm_amd build (AE-3+)
├── payloads/            # x86 oracle payloads (reused) + AMD-specific probe classes (SpecLockMap, interrupt-shadow)
├── harness/             # minimal SVM KVM harness, single-step characterization driver, run orchestration
├── schemas/             # canonical evidence formats + floor-checker scripts
└── results/
    ├── box-baseline-manifest.json
    └── <stage>/<run-set>/
```

Raw result volume too large for git is content-addressed with a checked-in manifest, summary,
and reproduction command. Golden evidence is immutable; reruns create a new run-set.

## Execution packet (hand this to the executing model on hardware arrival)

```text
Objective: Execute the AMD vendor feasibility spike defined in docs/AMD-EPYC.md: determine
whether the consonance deterministic-hypervisor mechanisms are real on bare-metal AMD Epyc
under Linux/KVM (SVM) — the ex_ret_brn_tkn work clock (with the SpecLockMap/LS_CFG workaround),
an exact single-step landing primitive WITHOUT MTF (SVM has none — BTF/TF #DB ruled at AE-2),
the svm.c force-exit-at-PMI analogue (AVIC disabled), and an AuthenticAMD frozen contract as a
vendor column on the one contract — then, gated on the bare-metal GO, nested SVM.

Read first: docs/AMD-EPYC.md (this program — binding, including its Evidence-integrity
section), docs/NESTED-X86.md (the x86 sibling whose evidence standards apply and whose nested
program AE-6 transposes), docs/ARM-ALTRA.md (the sibling this inherits structure and evidence
from), docs/ARCH-BOUNDARY.md (the seam; Intel↔AMD share the Arch — the vendor split is a
substrate concern; the trait-freeze memo AE-3 owes it), docs/CPU-MSR-CONTRACT.md (the frozen
Intel contract AE-4 adds a vendor column to), docs/BOX-PINNING.md, and
consonance/vtime/src/planner.rs (the CpuBackend contract AE-3 validates).

Work through stages AE-0 to AE-6 in order (AE-6 gated on the AE-5 bare-metal GO). Treat every
stage's acceptance criteria, the evidence-integrity countermeasures, and the disposition as
mandatory internal gates; record GO / PROVISIONAL GO / REDESIGN / NO-GO in docs/AMD-EPYC.md with
evidence locations before starting the next stage. Continue past intermediate reports while safe
in-scope progress remains; the terminal deliverable is the definition-of-done in the document,
not feasibility prose.

Workspace: git worktree add ../harmony-spike-amd-epyc -b spike/amd-epyc; all artifacts under
spikes/amd-epyc/. Commit locally as checkpoints. Never push, never merge, never commit on main.
Production-crate edits only when strictly required, minimal, marked SPIKE(amd-epyc):, and listed
in the final report.

Do not use Beads or the bd CLI for planning, tracking, memory, status, or handoff during this
spike; durable state lives in the evidence directories and this document's recorded
dispositions.

The Epyc box is exclusively yours and arrives un-provisioned — provisioning is part of AE-0.
Follow the Box discipline section exactly: test reachability first and stop-and-report if
unreachable (never simulate results); capture the baseline manifest (Zen generation, microcode,
LS_CFG and AVIC posture, SMT map) before the first change; content-hash-verify every bootable
artifact (patched kvm_amd included) immediately before boot; hard pinning AND the LS_CFG
SpecLockMap workaround are correctness conditions — the sanctioned deviation is AE-1's bounded
workaround-off probe; idle/offline the SMT sibling of every measurement core; separate
write/launch ssh calls, detached long-running processes, state-based waits, never pgrep/pkill -f
your own command lines; restore to the recorded baseline whenever yielding the lock, and verify
it.

You are the sole hardware executor: serialize every box-backed run, record its environment
first, account for every attempted sample, and validate raw evidence personally before accepting
it. Subagents may do bounded offline work with non-overlapping file ownership; they must not
touch the box, declare dispositions, or write authoritative manifests.

Evidence integrity is binding acceptance, not style: propagate every gate RC (a done-marker is
never success); machine-check every acceptance floor against retained records before writing a
disposition; attest the exercised mechanism per run (patched vs stock kvm_amd, the deterministic
exit reason, AVIC-off, the single-step primitive armed); judge counts only against analytical
oracles; account per-record overflow multiplicity. Never relax bit-identical, accept a
wall-clock dependency, silently substitute an event/counter/kernel/single-step/enforcement path,
simulate results for unreachable hardware, or count missing samples as successes. "AVIC off" and
"requires LS_CFG" are recorded limitations, not NO-GOs. Unsupported is a result. Smoke-fire each
large run-set's exact configuration once before spending it.

If a result fails, diagnose and attempt reasonable redesigns within the bare-metal AMD/SVM
thesis. Stop only when the definition of done is met or a named hard mechanism is conclusively
unavailable, and record which fallback the evidence selects (second Zen generation / software
work counter / emulation tier).

Report at the end: dispositions per stage with evidence paths, the measured-constants pack
(count offsets, Zen skid_margin, density table, pinned event encoding, single-step semantics),
the single-step ruling, the trait-freeze memo, the contract vendor-column skeleton + enforcement
truth table, the AE-5 one-command demo (and AE-6 nested twin if reached), all production-crate
diffs on the spike branch, box baseline status verified (LS_CFG/AVIC postures recorded), and
residual risks.
```

## Immediate focus

AE-0 exists solely to make **AE-1 runnable on day one** — the first scientifically interesting
result of the entire AMD program is whether `ex_ret_brn_tkn` counting is bit-deterministic on a
pinned Zen core with the SpecLockMap workaround applied. The single-step question (§1/AE-2) is
the *hardest* mechanism and the one most likely to force a redesign, but it is not the first
measurement: it depends on nothing AE-1 doesn't, and can be characterized in parallel offline
(the BTF/TF characterization driver is buildable before silicon). Nothing (contract work,
single-step characterization, nested thinking, rented-AMD excursions) may displace the AE-1
work-clock measurement. Before hardware arrives, the only work is offline apparatus: the oracle
payloads (mostly reused from the x86 det-corpus — same ISA), the single-step characterization
harness, the `svm.c` patch draft, and the floor-checker schemas — built so that arrival day is
spent measuring, not scaffolding.
</content>
</invoke>
