# ARM vendor spike program — Linux/KVM on Ampere Altra (Neoverse N1)

Status: **spike program, authored 2026-07-12; execution gated on hardware arrival.** This is
the ARM sibling of `docs/NESTED-X86.md`: a risk-ordered, GO/NO-GO-gated de-risking program for
the **bare-metal ARM cell of the reach matrix** (vendors × forms — the Consonance north star,
`docs/QUEUE.md`). Target hardware: an incoming **Ampere Altra** box (Neoverse N1, Armv8.2,
single-threaded cores, GICv3). The doc exists so that the day the box arrives is experiment
day: every stage below is specified to the level of "run this, retain that, decide on these
criteria."

This program is the gate `docs/ARM-PORT.md` demanded ("spike #1 on real silicon decides
whether ARM happens") and the gate `docs/ARCH-BOUNDARY.md` originally held its D-list behind:
**no production ARM backend code is built before this program returns GO** — a sequencing
clause superseded 2026-07-13 by the pre-build ruling (`docs/ARCH-BOUNDARY.md` §Pre-build
ruling): the D-list may now be built pre-GO on the port lane (`hm-cbt`), and this program's
GO/NO-GO decides whether that pre-built work is *kept and trusted*, plus every measured
constant and the trait freeze. Unchanged either way: this spike itself never writes
production backend code. It does not modify the
production architecture, does not fill the reach-matrix cell by itself (cell-fill — one
documented command builds the pinned stack, boots, and passes the same-seed determinism gate —
is the *port program's* exit, downstream of this spike), and does not decide procurement
strategy; it answers whether the mechanisms are real on this silicon.

Vocabulary note (binding): per the north-star ruling, **"vendor" replaces "personality"**
throughout — where `docs/ARCH-BOUNDARY.md` says "personality," read "vendor"; the
engine/vendor crate-split names stay reserved for exactly this window
(`docs/GLOSSARY.md` §Reserved). `docs/GLOSSARY.md` otherwise governs: `Subject`, `Moment` /
`Span`, `Reproducer`, `state_hash`, V-time as the name of the work-derived clock.

## Read first (binding context)

`docs/ARM-PORT.md` — the cross-ARM mechanism analysis; its hardware facts, three-mechanism
analysis, rr evidence base, and viability gate all stand and are assumed here, not re-derived.
`docs/ARCH-BOUNDARY.md` — the ISA seam ruling; this spike produces the data that freezes the
`Arch`/`CpuBackend` trait decision it deliberately deferred. `docs/NESTED-X86.md` — the
sibling program whose structure, execution constraints, and evidence standards this document
inherits (tightened — see §Evidence integrity). `docs/BOX-PINNING.md` — the pinning
discipline, which transfers with a new core map. Bead `hm-8h8` — the paravirt work-derived
clock design spec; this document cross-references it and validates it (stage AA-5), it does
not duplicate it.

## Topology and thesis

```text
Ampere Altra box (bare metal, Neoverse N1, Armv8.2)
└── host: Linux + KVM (VHE expected), patched with the arm64 determinism analogues
    └── guest: deterministic Subject — OUR arm64 guest kernel + new-by-purpose payloads
```

Same thesis as x86 bare metal: a KVM-based deterministic hypervisor — same seed ⇒
bit-identical execution, V-time derived from a hardware work counter, hypercall-only I/O,
default-deny guest CPU contract. What changes is every arch-specific mechanism underneath:
the work event, the time-virtualization story, the exit/patch surface, the interrupt fabric,
and the contract vocabulary. `docs/ARCH-BOUNDARY.md`'s audit says ~85% of the tree is already
arch-blind; this spike measures whether the remaining 15% *can exist* on N1 — it builds spike
apparatus only, never the production backend.

The six load-bearing questions, front and center, in risk order:

## 1. Time virtualization is the centerpiece: no FEAT_ECV

**Altra/N1 (Armv8.2) has no FEAT_ECV** (ECV is mandatory only from Armv8.6), so guest
`CNTVCT_EL0` reads **cannot be trapped** — a guest that executes a raw virtual-counter read
gets real hardware time, a nondeterminism source no hypervisor control can close on this
silicon. And this is not an Altra quirk: **no reachable ARM server part has ECV** — Graviton3
(Neoverse V1), Graviton4 and Grace (V2) all lack it; it arrives only with the AmpereOne /
V3–N3 wave. There is no trap-based time-virtualization tier on any hardware this program can
select. The x86 mechanism (RDTSC exiting → `f(V-time)`) simply does not exist here.

The design answer is the **paravirt work-derived clock** — spec bead `hm-8h8`
(cross-referenced, not duplicated): we own the guest kernel, so *all* of its time reads route
through a work-derived clock page (kvmclock-shaped, V-time/work as the only source), and raw
counter access is **closed at the contract level**, not the trap level:

- **EL0 (guest userspace):** denied architecturally — the guest kernel clears
  `CNTKCTL_EL1.{EL0VCTEN,EL0PCTEN,…}`, so any EL0 counter read undefs into the guest kernel.
  This closure is real hardware enforcement and is testable.
- **Physical counter/timer (`CNTPCT`, EL1 physical timer):** trappable without ECV via
  `CNTHCTL_EL2` — kept trapped/denied as a backstop.
- **EL1 virtual-counter reads (the untrappable residue):** closed by ownership — the guest
  kernel's clocksource, sched_clock, and delay paths are replaced with the paravirt page
  protocol, and the shipped kernel image is **opcode-scanned** for raw counter-read encodings
  as a machine-checked acceptance criterion (the same scan machinery as the LL/SC ladder, §4).

**Stage AA-5 validates this design.** Its kill condition, stated here because the task hangs
on it: *a load-bearing, reachable guest-kernel time dependency that cannot be routed through
the page (given that we own the kernel), or work-derived time proving insufficient for guest
kernel liveness (timekeeping / scheduler / RCU cannot reach steady state on it).* Because no
reachable ARM silicon offers a trap fallback, tripping this kill condition is a NO-GO for the
ARM vendor port as designed — not a stage-local setback.

(The same clock design is the nested-x86 N-4 memo's performance lever on x86 — removing RDTSC
exits from the hot path. One design, two axes; `hm-8h8` owns it.)

## 2. The work clock bet: BR_RETIRED on N1

V-time on ARM counts **`BR_RETIRED` (raw event `0x21`) = every architecturally executed
branch instruction, taken or not**. This is the integrator-ruled ARM binding from measured
finding **AA1-F1**. It is deliberately per-architecture: x86 remains retired conditional
branches, unchanged. Two facts, one favorable and one demanding:

- **Favorable:** N1 is the **best rr-characterized aarch64 lineage** — rr's production
  aarch64 support was developed and empirically trusted on Cortex-A76/Neoverse-N1-class cores
  (`docs/ARM-PORT.md` §evidence). Of every ARM part we could have received, this is the one
  with the strongest external evidence that precise branch-instruction counting is physically
  achievable. (Contrast Neoverse V2: in rr's allowlist speculatively, zero tested data.)
- **Demanding:** it is a **different event** than x86's retired *conditional* branches
  (`0x1c4`). Same trait contract (`CpuBackend`: monotonic, 0-or-1-per-instruction `u64`
  counter — `docs/ARCH-BOUNDARY.md` verified the trait ports unchanged), different physics.
  Therefore: **every `skid_margin`, event-density, and count-offset constant is re-measured
  on N1 and never inherited from x86.** The x86 `skid_margin = 256` is planning folklore
  here until AA-1/AA-3 produce the N1 numbers; `SimCpu` gets re-parameterized from the
  measured density table, not by copying.

**Standing condition — the migration PMI bug.** The N1/V1 lineage has a documented arm64
kernel bug: **PMU overflow interrupts can be missed on core migration** (rr issue #3607) —
exactly the failure that breaks precise injection (missed overflow → `run_until` never breaks
out of `KVM_RUN`). Mitigation: **hard pinning** of the vCPU thread and its perf context to one
core for every sample, as a standing condition of every stage (we pin anyway per
`docs/BOX-PINNING.md`; on this box it is a correctness requirement, not just hygiene). AA-1
probes the failure mode once, deliberately and bounded, so the mitigation is evidence-backed
rather than folklore — and then never runs unpinned again.

## 3. Kernel patch analogues: 0004 is real work, 0005 may be nearly free

The x86 determinism ABI rests on two host-kernel patches; their arm64 analogues have opposite
cost profiles and get **their own stages**:

- **The 0005-analogue (exact single-step) may be nearly free:** arm64 KVM already exposes
  hardware single-step via `KVM_GUESTDBG_SINGLESTEP` (`MDSCR_EL1.SS` + `PSTATE.SS`), stock.
  Stage AA-2 characterizes whether its semantics meet the landing loop's needs (exactly one
  instruction per step, deterministic across exceptions / WFI / injection boundaries /
  exclusive sequences) before any patch is written.
- **The 0004-analogue (deterministic in-kernel force-exit at PMI) is real arm64 KVM patch
  work:** converting a guest-mode work-counter overflow into a deterministic vCPU exit with a
  dedicated exit reason (the arm64 mirror of `KVM_ARM_PREEMPT_EXIT` → `KVM_EXIT_PREEMPT`).
  Stage AA-3 builds and validates it, then closes the loop with the full
  `run_until_overflow` + `single_step` exact-landing contract.

AA-3's data is what `docs/ARCH-BOUNDARY.md` deferred the trait freeze for ("ARM's
PMU-overflow-to-exit path may pressure `run_until_overflow`'s late-only-stop contract —
design the trait now; freeze it only with spike data in hand"). The stage's deliverables
include an explicit trait-freeze memo.

## 4. LL/SC vs LSE: the atomics ruling

LL/SC exclusives (`LDXR`/`STXR`) are a count-determinism minefield: an event landing between
the load-exclusive and store-exclusive clears the monitor → `STXR` fails → retry loop →
run-to-run taken-branch divergence (`docs/ARM-PORT.md`); the architecture additionally
permits *spurious* `STXR` failure, which is rr's related reason for refusing to record LL/SC
at all; and single-stepping an LL/SC loop can livelock outright (every step clears the
monitor). N1 has LSE (Armv8.1 atomics, mandatory ≥ v8.1), so the design answer is an
**LSE-only guest contract**, with three enforcement levels evaluated in stage AA-4:

1. **Build-level guarantee:** guest kernel (`CONFIG_ARM64_LSE_ATOMICS`, with the vanilla
   kernel's LL/SC fallback path removed — we own the kernel, so the static-key fallback body
   can be patched out, not merely not-taken) and guest userspace built LSE-only
   (`-march=armv8.1-a`, outline-atomics resolved to LSE or disabled).
2. **Verification:** opcode scan of every executable guest page for LDXR/STXR-family
   encodings, made durable by **W^X + rescan-on-exec** so runtime code generation cannot
   smuggle exclusives past a boot-time scan.
3. **Runtime execute guard (backstop only; requires the Harmony KVM patch):** every GFN starts stage-2
   execute-denied. First execute must exit to userspace for a scan before the page becomes
   executable/read-only; a write to an approved page must exit first, revoke execute, and require
   another scan. Stock Linux 6.18.35 arm64 KVM exposes neither per-GFN XN control nor an execute-
   fault exit: it grants `KVM_PGTABLE_PROT_X` internally and resumes. Therefore level 3 is
   unavailable on stock KVM. The draft `host/patches/0002-*` implements the required
   `kvm-cap-arm-stage2-exec-guard` state machine against pinned Linux 6.18.35, but apply+compile
   evidence is not runtime proof; `KVM_EXIT_MEMORY_FAULT`, dirty logging, `userfaultfd`, and
   `guest_memfd` do not substitute for it.

**The final ruling — a mandatory AA-4 deliverable — must state which of two worlds we are
in:** LL/SC is **mechanically unreachable** (levels 1+2 airtight; level 3 never engaged), or
LL/SC is a **cooperative residual risk** (named residuals — e.g. kernel alternatives/ftrace
runtime patching, JIT surfaces — with the enforcement level that bounds each). "We built with
LSE and hope" is not a ruling.

## 5. New contract, new payloads

The x86 CPU/MSR contract (`docs/CPU-MSR-CONTRACT.md`, ~1640 lines of CPUID leaves and IA32_*
MSRs) is the **rigor template, not the content**. The ARM analogue is a new document built on
the same philosophy — freeze a synthetic CPU, default-deny everything unlisted:

- **`ID_AA64*` freeze:** a synthetic ID-register model (the det-N1 analogue of `det-cfl-v1`)
  installed via KVM's writable-ID-register surface; the guest sees frozen feature bits, never
  the host's.
- **Trapped-sysreg tables:** the enforcement backend is `HCR_EL2`/`MDCR_EL2` trap groups
  instead of MSR filters — including the PMU row (guest may not observe or program any PMU
  system register; the `RDPMC→#GP` analogue) and the counter rows of §1. The spike delivers
  the **enforcement-mechanism truth table** (which contract rows map to which real traps or
  freezes, and what is undeniable on N1); the full contract document is port work.
- **Device row:** **GICv3 + the generic-timer model replace LAPIC/PIT.** The x86 seam shape
  carries (pure `now_vns`-in / deadlines + deliverable-interrupts-out; `docs/ARCH-BOUNDARY.md`
  §B), but arm64 KVM couples the generic timer's PPI wiring to the *in-kernel* vGICv3,
  whereas the x86 design keeps the interrupt fabric in userspace for determinism. Whether the
  in-kernel vGIC can be state-saved/restored bit-identically (its `KVM_DEV_ARM_VGIC_GRP_*`
  state surface) — or whether the port needs a userspace GIC model — is a measured decision
  input, stage AA-6.
- **Payloads are new-by-purpose, not ports.** The x86 bare-metal payloads test the x86
  contract; ARM gets new payloads against the new contract, on a minimal arm64 payload
  runtime (boot shim, exception vectors, PL011 console, GIC init — spike-grade apparatus
  under `spikes/arm-altra/`, reusing the host-derived-golden harness *pattern*). The
  hypercall doorbell surfaces as `HVC`/MMIO (a reserved GPA), not port I/O — per-vendor
  dispatch knowledge, as `docs/ARCH-BOUNDARY.md` already rules.

## 6. Fallbacks and siblings

- **Graviton `.metal` is the zero-procurement fallback and the second microarch data point:**
  c7g.metal (Graviton3, Neoverse V1) and c8g.metal (Graviton4, V2) are real bare-metal EL2
  with rentable-by-the-hour access. If the Altra slips or dies, AA-0/AA-1 run there
  unchanged (V1 shares the rr-characterized lineage — and the #3607 bug; V2 has *zero*
  rr-tested data and is a genuinely new measurement, not a covered one). On Altra GO, a
  bounded AA-1 re-run on a Graviton window is the cheap confirmation that the constants are
  lineage-stable versus N1-specific — decision input for every future ARM host class. Note
  V1/V2 carry SVE (the rr-flagged non-faulting-load worry); N1 does not — a hazard the Altra
  program gets to skip and a Graviton re-run must not.
- **Nested-on-ARM is explicitly deferred.** It requires FEAT_NV2 silicon (N1, v8.2, has no
  nested-virt hardware at all) plus very fresh, still-maturing KVM nested-arm64 support. It
  is its own future gate with its own program doc — never an assumption in this one, and no
  stage below may cite it.
- If the **work clock itself** fails on N1 (AA-1/AA-3 NO-GO), the fallback ladder mirrors
  the nested-x86 program: (a) re-run on the second microarch (Graviton) before concluding
  the failure is ARM-wide; (b) software work counter inside the owned guest kernel; (c) the
  deterministic-emulation replay tier. Fallbacks are recorded, not built, in this spike.
- If the **paravirt clock** fails (AA-5 kill), there is no fallback tier on reachable ARM
  silicon (§1) — that verdict escalates to the reach-matrix owner as a strategy fact.

## The bet and its kill conditions

The ARM vendor thesis is **NO-GO on this silicon** if any of these survives the bounded
experiments and reasonable redesigns:

- equal guest instruction streams produce different `BR_RETIRED` counts on a pinned core;
- work-counter overflow PMIs can be lost, duplicated, or delayed without a defensible
  empirical bound (with pinning enforced; #3607 makes unpinned operation untrusted, not the
  thesis);
- the 0004-analogue cannot convert overflow into a deterministic exit, or single-step
  landing can overshoot the work target irreducibly;
- an LSE-only guest still shows count divergence under injection, or LL/SC cannot be either
  mechanically excluded or bounded as a named cooperative residual;
- the owned guest kernel cannot live on work-derived time, or raw-counter closure cannot be
  demonstrated (§1's kill — fatal to the port, not just the stage);
- a guest-visible ID/sysreg that reaches state cannot be frozen or trapped on N1.

One unexplained count mismatch is blocking. Never convert NO-GO into GO by relaxing
"bit-identical," accepting a wall-clock dependency, counting unverified or missing samples as
successes, or quietly substituting a different event, counter, kernel, or enforcement level.
Unsupported is a result.

## Definition of done

Not a feasibility essay. The terminal deliverable is:

1. dispositions (GO / PROVISIONAL GO / REDESIGN / NO-GO) with retained machine-readable
   evidence for stages AA-0 through AA-6;
2. the **measured-constants pack**: BR_RETIRED count offsets per payload class, the N1
   `skid_margin`, the event-density table (the `SimCpu`/`PlannerConfig` re-parameterization
   inputs), and the single-step semantics notes;
3. the **LL/SC ruling** (mechanically unreachable vs cooperative residual, §4) recorded;
4. the **trait-freeze memo** to `docs/ARCH-BOUNDARY.md` (does `run_until_overflow`'s
   late-only-stop contract hold on arm64 PMI delivery, and what — if anything — the `Arch`
   trait must change before freezing);
5. the **paravirt-clock validation verdict** feeding `hm-8h8` ratification;
6. the Altra box's standing core assignments and baseline manifest recorded (the
   `docs/BOX-PINNING.md` table gains an Altra section on arrival), and the box left in its
   recorded baseline state whenever the lock is yielded.

On ALL-GO, what unblocks is **trust in the D-list work** (`docs/ARCH-BOUNDARY.md` §D — the
additive ARM backend/vendor wave, pre-buildable since the 2026-07-13 ruling): the measured
constants get applied, the trait freezes per the AA-3 memo, and the port proceeds to the
reach-matrix cell-fill demo as its exit. On NO-GO, the pre-built ARM-specific slice is the
sunk cost that ruling accepted. This spike never writes production backend code.

## Execution constraints (binding)

- **Hardware-arrival gate.** Nothing below runs until the Altra box is racked and reachable.
  Until then the only permitted work is offline: payload/oracle construction, harness and
  schema scaffolding, kernel-config and patch drafting — all under `spikes/arm-altra/`, all
  clearly untested-on-silicon. (A Graviton `.metal` window may substitute for arrival per §6,
  as an explicit recorded decision, never silently.)
- **Worktree.** Work in a dedicated git worktree on a new branch:
  `git worktree add ../harmony-spike-arm-altra -b spike/arm-altra` from `main`. All spike
  artifacts live under `spikes/arm-altra/` (layout below). Commit locally on the spike branch
  as checkpoints; **never push, never merge to main, never commit on main.** Production
  crates may be modified *on this branch only* when strictly required (expected: never — no
  ARM code exists in production); any such diff is minimal, marked `SPIKE(arm-altra):`, and
  listed in the final report. No production-architecture refactoring, no edits behind the
  `Backend`/`Arch` seam design.
- **No Beads.** Do not use Beads or the `bd` CLI for planning, tracking, memory, status,
  dependencies, or handoff during spike execution, even though repository-level agent
  instructions recommend it. This explicit instruction overrides that default. Durable state
  lives in the stage evidence directories, machine-readable manifests, and the dispositions
  recorded in this document.
- **Exclusive box lock.** The Altra box is exclusively the executor's for the spike's
  duration. Reboots and host-kernel swaps are permitted under the lock, subject to
  record-then-modify and baseline-restore below.
- **Serialization.** One hardware executor: every box-backed run is serialized by the
  primary agent, its environment recorded before execution, every attempted sample accounted
  for. Subagents may do bounded offline work (research, script construction, trace analysis,
  review) with non-overlapping file ownership; they must not touch the box, declare a stage
  disposition, or write an authoritative evidence manifest.
- **Smoke once before spend.** Before any large run-set (≥10⁴ samples or ≥30 min box time),
  fire the identical configuration once end-to-end and validate the evidence pipeline on
  that single sample.
- **Unsupported is a result.** Never silently substitute a different event, counter, kernel,
  toolchain, or enforcement path. If a capability is missing, record it and stop the
  affected stage.

## Box discipline (Altra edition)

Adapted from the nested-x86 program; copied here because the executor cannot read bd memories.

- **Reachability fluctuates on every box we run.** Test `ssh <altra-box> true` before every
  session (alias recorded in AA-0's environment manifest and `~/.ssh/config`; the repo
  hard-codes no host — `docs/BOX-PINNING.md`'s `DET_BOX_SSH` convention extends with an
  `ARM_BOX_SSH` variable). If unreachable, stop and report — never simulate results or
  fabricate a pass.
- **Record-then-modify.** Before the first change, capture a baseline manifest to
  `spikes/arm-altra/results/box-baseline-manifest.json`: SoC part/MIDR, firmware versions,
  running kernel, kvm module identity (stock vs patched), cmdline, governor, core topology,
  and any services touched. This box is *new* — the baseline captured on day one **is** the
  restore target; whenever the lock is yielded (and at spike end), return the box to a
  recorded state and verify the match. If the box is to become the standing ARM determinism
  host, its post-spike posture is recorded as a new baseline, explicitly, never implicitly.
- **Image content discipline.** Reference every bootable artifact **by content hash**: pin
  sha256 (+md5 cross-ref) in the harness and verify **immediately before every boot**, host
  kernels included. Never trust a mutable path. The pattern to reuse is
  `vmm-core/tests/live_dirty_remap.rs` (`guest_images()` / `verify_pin`).
- **pkill/pgrep landmine.** `pgrep -f`/`pkill -f` self-match wrapper argv — harness suicide
  and waiter deadlocks have occurred on the x86 box. Use separate write and launch ssh
  calls, redirect stdin (`</dev/null`), launch long-running processes detached
  (`setsid`/`nohup`), and use **state-based waits** (poll for a file/socket/pidfile), never
  `pkill -f`-based interrogation of your own command lines.
- **Core pinning.** N1 cores are single-threaded — there is no SMT sibling to idle (one
  whole x86 confound class absent). Pin every measurement to a dedicated physical core;
  record the pinned core, governor, and frequency posture in every run's evidence
  (`docs/BOX-PINNING.md` discipline; V-time counts are frequency-independent — frequency
  hygiene matters only for wall-clock numbers). AA-0 establishes and records the standing
  core-assignment table (housekeeping / measurement / guest cores) for the new box.
- **Pinning is load-bearing here** (§2, rr #3607): the vCPU thread and its perf context stay
  hard-pinned for every sample of every stage. The one sanctioned unpinned run is AA-1's
  bounded migration probe.

## Evidence integrity (binding — the PR-98 lesson)

The nested-x86 spike's review (2026-07-12, PR #98) found harnesses that could report green on
failed gates, dispositions whose acceptance floors were not met by the retained evidence, and
an existential-stage harness that silently exercised the stock fallback instead of the patched
mechanism. These countermeasures are therefore **mandatory acceptance criteria of every stage
below** — a stage without them cannot be GO regardless of its numbers:

1. **Gate-RC propagation.** A harness's success condition is the machine-propagated
   conjunction of every constituent gate's exit status. A done-marker, completion print, or
   "reached the end" condition is **never** a success condition.
2. **Machine-checked floors.** Every numeric acceptance floor (sample counts, rep counts,
   zero-mismatch claims) is checked by a script **against the retained evidence records** —
   recomputed from the raw per-sample data, not read from a summary line the harness itself
   asserted. The disposition may not be written until the checker passes; the checker's
   output is itself retained evidence.
3. **Content-hash-verified boots.** Every boot artifact (host kernel, guest kernel, payload
   images, initramfs) is sha256-verified **immediately before execution** — verification is
   a gate, not a log line. Recording a hash without verifying it is the anti-pattern this
   rule exists to kill.
4. **Mechanism attestation.** Every stage proves, in-band and per-run, that the *claimed*
   mechanism was exercised: patched-vs-stock module identity, exit reasons, patch markers
   asserted in the evidence as part of the stage's own acceptance. A silent fallback path
   (signal-kick instead of the 0004-analogue exit, debug-step instead of the armed
   mechanism) must be structurally unable to masquerade as the mechanism under test.
5. **Independent oracle.** Count-exactness claims are judged against **analytically
   constructed payload oracles** (payloads whose taken-branch counts are known by
   construction), never PMU-vs-PMU comparison, which is circular.
6. **Multiplicity + totality accounting.** Overflow/PMI delivery claims are established from
   per-record multiplicity (exactly-once shown from the records, not inferred from totals),
   and **every attempted sample appears in the evidence** — a missing sample is a failure to
   account, not a pass. Unsupported is a result.

Evidence manifests are machine-readable (stable JSON, sorted keys), written by the harness,
never handwritten from terminal output. Raw volume too large for git is content-addressed
with a checked-in manifest, summary, and reproduction command. Golden evidence is immutable;
reruns create a new run-set.

## Spike architecture

All under `spikes/arm-altra/`:

1. **Host prep** (`host/`) — box baseline/restore scripts, patched-host-kernel build recipe
   (AA-3 onward), pinned environment capture.
2. **Payload runtime + oracles** (`payloads/`) — the minimal arm64 bare-metal runtime (boot
   shim, exception vectors, PL011 console, GIC init) and the oracle payloads with
   analytically known taken-branch counts, per class (straight-line, branch-dense, syscall,
   exception, WFI/idle, LL/SC and LSE atomics, clock-page reads).
3. **Harness** (`harness/`) — the minimal KVM harness (single vCPU, pinned, ioctl-level) and
   run orchestration; the scan tooling (LDXR/STXR and counter-read opcode scans) shared by
   AA-4/AA-5.
4. **Evidence** (`schemas/`, `results/<stage>/<run-set>/`) — canonical machine-readable
   results plus the floor-checker scripts of §Evidence integrity.

Every run records at least: SoC/MIDR + firmware, host kernel + kvm module identity (stock vs
patched, with hashes), KVM mode (VHE/nVHE), guest/payload image hashes (verified pre-boot),
perf event configuration (raw 0x21, pinned, exclusion flags), core pinning map, governor,
experimental condition, all counter values, targets, overflow records with multiplicity,
skid, landed state, and result digests.

## Risk-ordered stages

Each stage: question / method / acceptance / stop. The §Evidence-integrity criteria are part
of every stage's acceptance implicitly and are not restated per stage.

### AA-0 — day-one bring-up + capability truth table

**Question:** Does the delivered Altra expose exactly what this program assumes?

Method: capture the baseline manifest (§Box discipline). Record, from real silicon, a
machine-readable truth table:

- identity: MIDR (Neoverse N1 revision), SoC part, core count, firmware/kernel versions;
- ID-register facts, each an explicit expect-vs-found row: `ID_AA64MMFR0_EL1.ECV` (**expect
  absent** — confirms the §1 premise), `ID_AA64ISAR0_EL1.Atomic` (**expect LSE present**),
  `ID_AA64DFR0_EL1.PMUVer` (PMUv3 version), SVE (**expect absent**), nested-virt (**expect
  absent**);
- PMU: `BR_RETIRED` (0x21) present in `PMCEID1_EL0` (bit 1: `PMCEID1_EL0` enumerates events
  `0x20..0x3f`, so event `0x21` is its bit 1; `PMCEID0_EL0` covers only `0x00..0x1f`);
  `perf_event_open` of raw 0x21 as a pinned, non-multiplexed event succeeds; a trivial
  host-side overflow test delivers a sample/signal;
- KVM: `/dev/kvm` present; VHE vs nVHE mode recorded; `KVM_CAP_SET_GUEST_DEBUG`
  (single-step) present; vGICv3 device creatable; the writable-ID-register surface
  enumerated;
- topology: the standing core-assignment table for this box chosen and recorded (feeds a
  `docs/BOX-PINNING.md` Altra section on the port branch, later).

**Acceptance:** truth table complete and machine-readable; byte-identical across two
reboots; every "expect" row either confirmed or recorded as a deviation with an explicit
disposition (a *favorable* deviation — e.g. ECV unexpectedly present — still requires a
recorded ruling before any stage relies on it).

**Stop:** no KVM/EL2, no usable PMUv3, or `BR_RETIRED` absent/unopenable → NO-GO for this
box with the capability diff recorded; the program moves to the Graviton fallback (§6).

### AA-1 — the work clock: count exactness, PMI reliability, skid (the existential trio)

**Question:** Is `BR_RETIRED` counting bit-deterministic on a pinned N1 core, do overflow
PMIs arrive reliably out of `KVM_RUN`, and what is the skid bound?

This is `docs/ARM-PORT.md`'s spike #1, and the highest-value measurement of the program;
nothing may displace it. All counting is judged against the analytical oracle (§Evidence
integrity #5). Three sub-experiments:

- **(a) Host-side exactness:** pinned EL0 counting of oracle payloads across classes
  (straight-line loops, branch-dense, syscall, signal, page-fault), differentially across
  1e6/1e7/1e8 scales. The expected shape is oracle + a small constant offset (the x86
  analogue measured n+2); the offset is *measured and pinned per class*, and a
  variable offset is a mismatch, not a calibration.
- **(b) Guest-mode exactness:** the minimal KVM harness runs the bare-metal oracle payloads
  on a pinned vCPU; count guest-only (host-excluded attribution); equal streams → equal
  counts, vs oracle; across payload classes including WFI/idle and injected-interrupt
  classes; repeated after a host reboot.
- **(c) Overflow + skid:** sampling-mode overflow with a kick out of `KVM_RUN` (the
  pre-patch mechanism — a host-side signal to the vCPU thread; AA-3 moves this in-kernel);
  every armed overflow delivered exactly once, shown per-record (§Evidence integrity #6);
  the early/late skid distribution measured → the candidate **N1 `skid_margin`** and the
  event-density table (§2's re-measured constants).
- **The migration probe (bounded, once):** deliberately unpin and force cross-core
  migrations under armed overflow to observe the #3607 failure mode on this exact
  kernel/silicon; record its signature (lost PMI → hang vs delayed). Then re-pin
  permanently. This turns the standing pinning condition into evidence.
- **Contamination probes:** co-tenant load on other cores, then on the same core, memory
  pressure; count invariance required (wall clock may move; counts may not).

**Acceptance (PROVISIONAL GO threshold):** zero count mismatches and zero missed/duplicate
overflows over **≥10⁶ armed overflows cumulative** across the condition matrix, stable
per-class count offsets, and a stable skid bound; the measured `skid_margin` and density
table recorded as the constants pack (§Definition of done #2). Report confidence and
coverage; do not call it a proof.

**Stop:** one unexplained mismatch, or PMI loss that pinning does not eliminate → NO-GO for
the N1 hardware work clock; record which fallback the evidence selects (§6): Graviton
re-measurement before an ARM-wide conclusion, then software work counter or emulation tier.

### AA-2 — single-step exactness (the 0005-analogue; expected nearly free)

**Question:** Does stock `KVM_GUESTDBG_SINGLESTEP` (`MDSCR_EL1.SS`/`PSTATE.SS`) deliver
exactly-one-instruction steps with deterministic work accounting?

Method: stock KVM, pinned vCPU, oracle payloads. Verify: one instruction retired per step
(vs oracle); `BR_RETIRED` increments exactly on stepped branch instructions, taken or not,
and not on ordinary non-branch instructions;
step behavior across exception entry/return, WFI, and injected-interrupt boundaries (no
skipped or doubled instructions); and — deliberately — stepping **through LL/SC sequences**,
where the architectural hazard is that each step clears the exclusive monitor and can
livelock the retry loop. Characterize that behavior precisely; it is direct input to AA-4's
ruling.

**Acceptance:** exact step counts vs oracle across all classes; replay-identical stepped
states; the LL/SC-stepping behavior documented with retained evidence.

**Stop:** stepping skips/doubles instructions, or interacts nondeterministically with
injection → the 0005-analogue is *not* free; REDESIGN with the patch cost re-estimated
(an x86-0005-style patch on arm64 KVM) before proceeding — AA-3 depends on a trustworthy
step primitive.

### AA-3 — deterministic force-exit at PMI (the 0004-analogue) + exact landing

**Question:** Can a patched arm64 KVM convert a work-counter overflow into a deterministic
in-kernel vCPU exit, and does overflow-early + single-step land `work == target` exactly?

Method: the real patch work. Build the arm64 analogue of patch 0004 — guest-mode
work-counter overflow → in-kernel vCPU kick with a dedicated deterministic exit reason
(mirroring `KVM_ARM_PREEMPT_EXIT` → `KVM_EXIT_PREEMPT`) — on the recorded host kernel;
content-pin the patched modules. Then drive the full landing contract
(`run_until_overflow` + `single_step`, the `CpuBackend` inversion) against seeded-random
targets: deltas 1..100k; MTF-analogue-edge / skid-bracket / pure-overflow classes
interleaved; across payload classes including targets adjacent to counted and uncounted
instructions and on both sides of exceptions. **Mechanism attestation is load-bearing here**
(the PR-98 failure was exactly this stage's x86 twin silently testing stock fallback): every
landing's evidence must assert the patched exit reason and patched-module identity, and the
harness must be structurally unable to fall back to the AA-1 signal-kick and still pass.

**Acceptance:** **≥10⁶ armed deadlines cumulative** with `work == target` on every landing,
never overshoot, replay-identical landed-state digests; skid never exceeding the AA-1
margin (a violation triggers an explicit rerun/ruling — the margin is never silently
enlarged); the **trait-freeze memo** written (does late-only-stop hold on arm64 PMI
delivery; what, if anything, the `Arch`/`CpuBackend` design must absorb —
`docs/ARCH-BOUNDARY.md`'s deferred decision).

**Stop:** PMI-to-exit cannot be made deterministic, or landing overshoots irreducibly →
NO-GO for the hardware work-clock thesis on N1; fallback ladder as in AA-1.

### AA-4 — the LL/SC vs LSE ruling

**Question:** Can the guest contract make LL/SC mechanically unreachable — and does an
LSE-only guest hold count-determinism under injection?

Method (§4's ladder, evaluated on real artifacts, not in prose):

- **(a) Demonstrate the hazard:** an LL/SC oracle payload with events injected (via AA-3's
  machinery) inside exclusive sequences → observe monitor-clear retries and quantify the
  count divergence; plus a bounded probe for architecturally-permitted spurious `STXR`
  failure under cache pressure. This validates the threat model with evidence.
- **(b) Demonstrate the answer:** the same payload rebuilt LSE-only under the identical
  injection schedule → bit-identical counts and digests, repeated.
- **(c) Evaluate the enforcement ladder** on the real owned-guest artifacts: the LSE-only
  kernel + userspace build (level 1, including removal of the vanilla kernel's LL/SC
  fallback bodies); the executable-page opcode scan with W^X + rescan-on-exec (level 2) run
  against the actual guest kernel image and a running guest; the patched stage-2 execute guard
  (level 3) advertised by `kvm-cap-arm-stage2-exec-guard` and exercised at least once against a
  deliberately planted exclusive, to prove the pre-execute scan/reject path engages.

**Acceptance:** divergence demonstrated (a), invariance demonstrated (b), all three ladder
levels exercised with evidence (c), and **the ruling recorded** — mechanically unreachable
vs cooperative residual risk, with every residual named and bounded (§4). The ruling is a
deliverable of the program (§Definition of done #3), not commentary.

**Stop:** LSE-only builds unachievable for the owned guest, or LSE payloads still diverge
under injection → REDESIGN (injection-boundary discipline, emulate-through-exclusives) with
the determinism cost measured; if no level of the ladder closes the hazard, NO-GO with the
mechanism named.

### AA-5 — the paravirt work-derived clock (the centerpiece)

**Question:** Does the owned guest function deterministically when its *only* time source is
the work-derived clock page, with raw counter access closed at the contract level?

This stage validates the `hm-8h8` design (§1). Method, mechanism first:

- **(a) Payload-level mechanism:** the spike harness maintains the clock page per the
  `hm-8h8` layout (that spec governs; this stage implements the minimum needed to test it);
  oracle payloads read time via the page protocol only. Verify time values are derived from
  work alone: bit-identical across same-seed reps, and **invariant under deliberate
  wall-clock perturbation** (host stalls, delays between exits — values may not move).
- **(b) Closure:** verify each closure layer of §1 by test — an EL0 `CNTVCT_EL0` read undefs
  under the `CNTKCTL_EL1` setting; physical counter/timer access traps (`CNTHCTL_EL2`
  posture recorded); the counter-read opcode scan runs clean against the shipped guest
  kernel image (every hit triaged to an unreachable or patched-out site), under the same
  W^X/rescan machinery as AA-4.
- **(c) The Linux smoke:** our arm64 guest kernel — paravirt clocksource, sched_clock, and
  delay paths on the page; `CNTKCTL_EL1` closure applied — boots to userspace under the
  spike harness and reaches steady state (no RCU stalls, no timekeeping wedges, timers
  fire), then holds a same-seed determinism digest: two runs, bit-identical console and
  state digest.

**Acceptance:** (a) and (b) machine-checked; (c) boots, reaches steady state, and reproduces
its digest; the validation verdict written back against `hm-8h8` (§Definition of done #5).

**Stop (the stage's kill condition, verbatim from §1):** a load-bearing, reachable
guest-kernel time dependency that cannot be routed through the page, or work-derived time
insufficient for guest-kernel liveness. Because no reachable ARM server silicon can trap
`CNTVCT`, this kill is a NO-GO for the ARM vendor port as designed — escalate to the
reach-matrix owner; do not attempt a trap-based workaround that the hardware does not have.

### AA-6 — contract enforcement + device-model decision inputs + the mini determinism gate

**Question:** Can the guest-visible CPU surface be frozen and enforced on N1, and do the
remaining device-row mechanisms have a deterministic shape?

Method:

- **(a) `ID_AA64*` freeze:** install a shrunk synthetic ID-register model through KVM's
  writable-ID-register surface; verify the guest sees frozen values (including with feature
  bits *below* host capability); enumerate the `HCR_EL2`/`MDCR_EL2` trap groups against the
  contract-row skeleton of §5 — PMU sysregs denied (observe: guest PMU reads/writes fault),
  counter rows per AA-5(b). Deliverable: the **enforcement-mechanism truth table** — every
  planned contract row mapped to a demonstrated trap/freeze, or recorded as undeniable on
  N1 with a disposition.
- **(b) vGIC decision input:** in-kernel vGICv3 state save → restore → save round-trip
  (`KVM_DEV_ARM_VGIC_GRP_*`): bit-identical? Injection through the vGIC at a landed
  `Moment`: reproducible? Verdict recorded as the port's userspace-GIC-vs-in-kernel-vGIC
  decision input (§5) — measured, not argued.
- **(c) The mini determinism gate:** same seed twice → bit-identical state digest, on the
  spike harness, over the payload matrix **plus** the AA-5 Linux guest, with events injected
  at seeded-random `Moment`s — the whole mechanism stack (work clock, exact landing,
  LSE-only contract, paravirt time, frozen IDs) exercised together. This is the spike's
  proof-of-mechanism for the reach-matrix cell; the cell itself is filled by the port
  program's one-command demo later.

**Acceptance:** truth table complete; vGIC round-trip verdict recorded; **≥1,000 same-seed
mini-gate repetitions bit-identical**, every attempted sample accounted for, floors
machine-checked against the retained records.

**Stop:** an unfreezable guest-visible register that reaches state, or vGIC state that
cannot round-trip *and* no userspace-model shape exists → REDESIGN (respecify the device
row) or NO-GO with the gap named.

## Decision ladder

Each stage ends with exactly one recorded disposition:

- **GO** — acceptance met; next stage may begin.
- **PROVISIONAL GO** — evidence clean but bounded; the limitation is named and re-stressed
  at a later stage (AA-6's mini gate is the default re-stress point).
- **REDESIGN** — achievable with a named change inside the same bare-metal ARM/KVM thesis
  (different arming strategy, injection-boundary discipline, enforcement-level change);
  repeat the stage.
- **NO-GO** — a required hard mechanism is absent on this silicon. Record which fallback the
  evidence selects: (a) the second microarch (Graviton `.metal`, §6) before any ARM-wide
  conclusion, (b) software work counter inside the owned guest kernel, or (c) the
  deterministic-emulation replay tier. Fallbacks are recorded, not built, in this spike. An
  AA-5 NO-GO has no fallback tier (§1) and escalates as a strategy fact.

Out of scope for this spike: the production D-list build (`docs/ARCH-BOUNDARY.md` §D), the
ARM CPU-contract document itself (AA-6 delivers its enforcement truth table only), the
`hm-8h8` implementation (AA-5 validates the design), any Graviton execution beyond the
recorded fallback/confirmation runs, nested-ARM in any form, and cross-vendor (AMD) work —
the Epyc program is its own document.

## Repository layout

```text
spikes/arm-altra/
├── README.md            # commands, environment, current dispositions
├── host/                # baseline/restore scripts, patched-host-kernel build (AA-3+)
├── payloads/            # arm64 payload runtime + analytical oracle payloads
├── harness/             # minimal KVM harness, run orchestration, opcode-scan tooling
├── schemas/             # canonical evidence formats + floor-checker scripts
└── results/
    ├── box-baseline-manifest.json
    └── <stage>/<run-set>/
```

## Execution packet (hand this to the executing model on hardware arrival)

```text
Objective: Execute the ARM vendor feasibility spike defined in docs/ARM-ALTRA.md: determine
whether the consonance deterministic-hypervisor mechanisms are real on bare-metal Ampere
Altra (Neoverse N1) under Linux/KVM — BR_RETIRED work clock, deterministic force-exit +
single-step landing, LSE-only atomics contract, the paravirt work-derived clock (no FEAT_ECV
on this or any reachable ARM silicon), and a freezable guest CPU contract.

Read first: docs/ARM-ALTRA.md (this program — binding, including its Evidence-integrity
section), docs/ARM-PORT.md (hardware facts + rr evidence base), docs/ARCH-BOUNDARY.md (the
seam; the trait-freeze memo AA-3 owes it), docs/NESTED-X86.md (the sibling whose evidence
standards apply), docs/BOX-PINNING.md, the hm-8h8 clock spec, and
consonance/vtime/src/planner.rs (the CpuBackend contract AA-3 validates).

Work through stages AA-0 to AA-6 in order. Treat every stage's acceptance criteria, the
evidence-integrity countermeasures, and the disposition as mandatory internal gates; record
GO / PROVISIONAL GO / REDESIGN / NO-GO in docs/ARM-ALTRA.md with evidence locations before
starting the next stage. Continue past intermediate reports while safe in-scope progress
remains; the terminal deliverable is the definition-of-done in the document, not
feasibility prose.

Workspace: git worktree add ../harmony-spike-arm-altra -b spike/arm-altra; all artifacts
under spikes/arm-altra/. Commit locally as checkpoints. Never push, never merge, never
commit on main. Production-crate edits only when strictly required, minimal, marked
SPIKE(arm-altra):, and listed in the final report.

Do not use Beads or the bd CLI for planning, tracking, memory, status, or handoff during
this spike; durable state lives in the evidence directories and this document's recorded
dispositions.

The Altra box is exclusively yours. Follow the Box discipline section exactly: test
reachability first and stop-and-report if unreachable; capture the baseline manifest before
the first change; content-hash-verify every bootable artifact immediately before boot; hard
pinning is a correctness condition (the N1-lineage missed-PMI-on-migration bug, rr #3607) —
the one sanctioned unpinned run is AA-1's bounded migration probe; separate write/launch ssh
calls, detached long-running processes, state-based waits, never pgrep/pkill -f your own
command lines; restore to a recorded baseline whenever yielding the lock, and verify it.

You are the sole hardware executor: serialize every box-backed run, record its environment
first, account for every attempted sample, and validate raw evidence personally before
accepting it. Subagents may do bounded offline work with non-overlapping file ownership;
they must not touch the box, declare dispositions, or write authoritative manifests.

Evidence integrity is binding acceptance, not style: propagate every gate RC (a done-marker
is never success); machine-check every acceptance floor against retained records before
writing a disposition; attest the exercised mechanism (patched vs stock) in-band per run;
judge counts only against analytical oracles; account per-record overflow multiplicity.
Never relax bit-identical, accept a wall-clock dependency, silently substitute an
event/counter/kernel/enforcement path, simulate results for unreachable hardware, or count
missing samples as successes. Unsupported is a result. Smoke-fire each large run-set's exact
configuration once before spending it.

If a result fails, diagnose and attempt reasonable redesigns within the bare-metal ARM/KVM
thesis. Stop only when the definition of done is met or a named hard mechanism is
conclusively unavailable, and record which fallback the evidence selects (Graviton second
microarch / software work counter / emulation tier — noting an AA-5 kill has no fallback and
escalates).

Report at the end: dispositions per stage with evidence paths, the measured-constants pack
(count offsets, N1 skid_margin, density table, single-step semantics), the LL/SC ruling, the
trait-freeze memo, the hm-8h8 validation verdict, all production-crate diffs on the spike
branch, box baseline status verified, and residual risks.
```

## Immediate focus

AA-0 exists solely to make **AA-1 runnable on day one** — the first scientifically
interesting result of the entire ARM program is whether `BR_RETIRED` counting is
bit-deterministic on a pinned N1 core. Nothing (contract work, clock implementation detail,
Graviton excursions, port planning) may displace that measurement. Before hardware arrives,
the only work is offline apparatus: oracle payloads, the minimal harness, the floor-checker
schemas — built so that arrival day is spent measuring, not scaffolding.

## Dispositions (task 122 execution log — hardware arrived 2026-07-17)

Box: `harmony-arm` — Ampere Altra (Neoverse N1 **r3p1**, MIDR `0x413fd0c1`), HPE
ProLiant RL300 Gen11, BIOS 1.74, 80 cores, SMT not implemented, delivered kernel
Ubuntu 6.8.0-134-generic, KVM VHE. Baseline manifest (the restore target):
`spikes/arm-altra/results/box-baseline-manifest.json`. Core assignments: housekeeping
0–3, measurement 60–69, guest 70–79 (recorded in the truth table's topology block).

### AA-0 — **GO** (2026-07-17)

Acceptance met in full: the 14-row truth table is complete and machine-readable;
captures A, B, C (`results/aa-0/capture-{A,B,C}/truth-table.json`) are
**byte-identical across two reboots** (reboot returns 173s/198s, both on
6.8.0-134); every expect row is confirmed except `writable-id-registers`, which
carries its explicit recorded ruling (PFR1 frozen on stock 6.8; re-probe on the
patched host before AA-6(a) relies on it). Evidence and probe inputs under
`results/aa-0/`; the runtime posture is re-applied after every reboot by
`host/spike-posture.sh` (nothing persists).

Evidence: `spikes/arm-altra/results/aa-0/` — `capture-A/truth-table.json` (14 rows,
probe RC 0), `box-config.json` + `rulings.json` (the probe inputs),
`capture-boot0*/` (the day-one pre-fix captures, retained as the record of the two
apparatus findings below).

Every existential row confirmed on silicon: `/dev/kvm` + VHE; raw `0x21` opens
pinned/non-multiplexed and counts; **BR_RETIRED is PMCEID1-implemented**; a host
overflow **delivers**; `KVM_CAP_SET_GUEST_DEBUG` present; vGICv3 creatable; **ECV
absent** (the §1 premise, now measured); **LSE present**; SVE absent; FEAT_NV absent;
PMUVer **0x4** (PMUv3p1, per the N1 TRM); determinism cap absent (stock kernel, as
expected).

Day-one findings (apparatus fixes, committed with this log):

1. **No `/sys/module/kvm_arm/parameters/mode` on this kernel** (`kvm-arm.mode` is an
   early_param). `sys::kvm_mode` gained a strict kernel-log fallback (klogctl, parsing
   the three `kvm_arm_init` lines); requires `dmesg_restrict=0`, applied by
   `host/spike-posture.sh` (re-run after every reboot; nothing in the posture persists).
2. **A featureless vCPU reads `ID_AA64DFR0_EL1.PMUVer` as 0x0** — KVM masks it without
   `KVM_ARM_VCPU_PMU_V3`. The disposable ID-reading vCPU now inits with the vPMU
   feature (the measurement vCPU keeps it off; the guest contract denies the guest a
   PMU); the row's raw records which read happened.
3. **`writable-id-registers` found ABSENT — RULED** (`results/aa-0/rulings.json`): the
   per-register probe shows the whole enumerated `ID_AA64*` surface writable except
   `ID_AA64PFR1_EL1` (frozen on 6.8; its writable mask is later mainline). Not a stop
   condition. Consequence: AA-6(a) cannot fully install its freeze on the stock 6.8
   kernel; the row is re-probed on the AA-3 patched host (6.18.35) before AA-6 relies
   on it, else PFR1 becomes a named enforcement-truth-table row (HCR_EL2.TID3
   trap-emulation as the fallback level).

Environment decisions recorded:

- **Host-kernel plan.** Ubuntu publishes no ddeb vmlinux for 6.8.0-134, so the
  delivered kernel cannot pass the harness's build-id host attestation. Decision: build
  the pinned **stock linux-6.18.35** natively (`host/build-stock-6.18.35.sh`) — the
  same tree the AA-3 patch targets — and run AA-1 on it; stock-vs-patched then differs
  by exactly the patch. The 6.8.0-134 baseline remains the recorded restore target.
- **Reboot authorization.** The AA-0 acceptance's two reboot-identity captures (B, C)
  and the 6.18.35 boot are pending operator authorization of box reboots (the
  execution session's permission layer blocks autonomous `reboot`; reported rather
  than worked around).

Disposition: **not yet declared** — waits on byte-identical captures A/B/C.

### AA-1 — preliminary (day-one EL0 probes; MAJOR event-semantics finding)

Evidence: `spikes/arm-altra/results/aa-1a/` — `aa1a-smoke-001` (2 classes × 2 seeds
× 3 reps, smoke scale) and `aa1a-scale-probe-001` (2 classes × 2 seeds × 2 reps ×
1e6/1e7/1e8), produced by the new `arm-el0-count` tool (AA-1(a): the SAME window
`.s` bodies the guest boots, linked into a pinned EL0 process, raw 0x21 counting
this thread's EL0 execution) and graded by `el0-check`.

**Finding AA1-F1 (doc-vs-hardware, ruled 2026-07-18): N1's `BR_RETIRED` (0x21)
counts architecturally-executed branch INSTRUCTIONS — taken and not-taken — not
"retired taken branches" as §2 and `docs/ARM-PORT.md` state.** The evidence
signature is unambiguous: branch-dense counts are IDENTICAL across seeds
(`8×trips + 14` exactly — the 7 data-dependent predicates plus the back-edge each
retire once per trip regardless of direction), while the taken-branch model would
differ per seed by the PRNG's taken-sum (and did, as a seed-varying "offset" in
`aa1a-smoke-001`'s failed oracle-exactness check — retained). Straight-line is
`trips + 12` exactly (its `b.ne` executes `trips` times: `trips−1` taken + 1
final not-taken).

**Determinism itself is STRONGER than the program assumed**: bit-exact counts
across five orders of magnitude (up to 800,000,014 events per window, zero
deviation), across seeds, reps, and two cores — measured incidentally under an
80-core kernel-build co-tenant, which the counts did not notice. 0-or-1 per
instruction, monotonic, data-independent for fixed control flow.

**Integrator ruling:** keep 0x21 as the ARM work clock with the corrected semantics — the
model's expected counts are branch-instructions-executed, knowable by construction from the
same windows; the accumulator machinery stays as the predicate witness. No event substitution
occurred: the hardware event is unchanged, our description of it is corrected. This ruling
does not alter the x86 retired-conditional-branch clock.

Caveats recorded: (a) both probe run-sets are labeled `pinned-solo` but ran
beside the kernel build — they are pipeline probes, not disposition evidence; the
graded AA-1(a) sets rerun each condition deliberately on a quiet box. (b)
`el0-check`'s oracle-exactness check grades against the pre-correction model and
correctly FAILED the probes; it is updated with the model, and the failed verdict
is retained as the discovery record.

Post-correction status: the model correction (`certain_branches`) is implemented
across oracle-model / el0-check / the guest checker's `total()` base, fixtures
and the expected-counts manifest regenerated, all offline gates green; the probe
evidence re-grades **PASS 11/11** with per-class constant offsets (+12
straight-line, +14 branch-dense over 36 records).

**AA-1(a) EL0 condition matrix: ALL GREEN — self-certified on the tool-attested
5-class union.** Certified evidence:
`results/aa-1a/aa1a-{pinned-solo,co-tenant-other-core,co-tenant-same-core,memory-pressure}-002`
(the full five-class sets — `straight-line`, `branch-dense`, `el0-syscall`,
`el0-signal`, `el0-pagefault` — 450 records each = **1800 records**, all four
conditions carrying one shared `tool_sha256` `fa3327…`, quiet box, core 61).
`el0-check` over the union: **RESULT PASS (12 checks)**, retained verbatim at
`results/aa-1a/el0-verdict.txt` (§Evidence-integrity #2 — the checker's output is
itself retained evidence, as for every other stage). It recomputes: the 5×4
class×condition matrix complete; the 1e6/1e7/1e8 differential covered per class;
180 repeated cases bit-identical; 1800/1800 accumulators match; and the per-class
constants — window classes `straight-line +14`, `branch-dense +13` (one constant
each across every condition and scale), kernel-mediated classes fit exactly as
`el0-syscall = 1·trips + 13`, `el0-signal = 2·trips + 14`,
`el0-pagefault = 2·trips + 14`. Wall clock moved under load; counts did not.

The disposition rests on the `-002` union and not the earlier `-001` sets by
design: the `-001` sets carry only two classes and a null `tool_sha256`, and
`el0-check` now **rejects** that union (aggregation: not one attested tool;
coverage-matrix: three classes missing under every condition) — the checker
self-certifies which evidence is admissible, and `host/el0-conditions.sh` writes
its success marker only if `el0-check` passes. (b)/(c) are guest-mode; (b) is
covered by AA-1(b) below, (c) by AA-1(c).

Measurement-host staging: stock 6.18.35 deb built and installed
(`linux-image-6.18.35_6.18.35-2_arm64.deb`; vmlinux build-id
`1e975db8ae7fa463a78c6190c4079a88409ab888` retained for run attestation).
**Reboot sequence authorized by Paul 2026-07-17** and staged
(`host/stage-6.18-boot.sh` run; `saved_entry` pinned to 6.8.0-134; `panic=30`
live; one-shot deliberately cleared until captures B/C land on -134, re-armed
before the 6.18.35 boot). Two staging findings recorded: (a) **the grubenv sits
on LVM, so GRUB cannot self-clear `next_entry` at boot** — the "one-shot" is
manual-clear-after-success, and a panicking 6.18.35 would loop rather than fall
back (escalation, per the authorized stop conditions; the earlier
"self-recovering" description was wrong on this box). (b) unattended-upgrades
installed a 6.8.0-136 image mid-spike (inert under `saved_entry` pinning;
baseline drift noted).

### AA-1(b) — guest-mode count exactness: FIRST REAL KVM_RUN, ALL GREEN

Evidence: `results/aa-1b/` — `aa1b-smoke-001` (8/8, the pipeline shake-out) and
`aa1b-pinned-solo-001` (**720 records**, 8 payloads × 3 scales {1e6,1e7,1e8} × cases ×
reps, counting mode — `overflow.armed: false`), core 60, stock 6.18.35 (build-id
`1e975db8…`), floor-checked.

The bare-metal oracle payloads boot on a pinned single vCPU and count guest-only
(`exclude_host`) exactly against the analytical oracle: **720/720 count-exact across
1e6–1e8**, zero window offset — the guest-mode counts land on the same
`certain_branches` model the EL0 half (AA-1(a)) confirmed, on first contact. The
**weights pack is CONFIRMED** (`results/aa-1b/weights-provisional.json`:
`exception_entry=1`, everything else 0, `window_offset=0`) — the per-class offsets are
the differential the density table rests on.

**Finding AA1-F2 (the §4 hazard, observed): `llsc-atomics` shows spontaneous
run-to-run count divergence** even with no injection — the architecturally-permitted
spurious `STXR` failure of `docs/ARM-ALTRA.md` §4, surfacing as data-dependent retry
counts. It does NOT break AA-1(b): `llsc-atomics`/`clock-page` carry a guest-*reported*
retry term (`has_reported_term`), so count-exactness grades `oracle_base + reported`
and holds; and AA-1 replay-identity has the documented llsc carve-out (multiple llsc
digests across reps are the expected §4 signature, not a failure). Recorded here as the
threat AA-4 must rule on (mechanically-unreachable vs cooperative-residual); the LSE
payload is deterministic by construction, the intended answer.

### AA-1(c) — overflow + skid (the existential-trio third): IN PROGRESS, campaign launched 2026-07-17

The armed-overflow experiment: sampling-mode overflow with a host-side signal kick out
of `KVM_RUN` (the pre-patch mechanism; AA-3 moves it in-kernel), every armed overflow
required delivered exactly once (per-record multiplicity), the early/late skid
distribution measured → the candidate N1 `skid_margin` + density table.

**Smoke (validated, clean):** `results/aa-1c/aa1c-armed-smoke-001` (16 armed records,
core 60) floor-checks PASS on every per-set check — multiplicity exactly-once,
count-exactness, skid self-consistent, mechanism = `SignalKick` attested, perf raw
`0x21` guest-only + pinned, image pins verified. (Only `condition-matrix` fails, as any
single-condition set must.)

**Finding AA1-F3 (apparatus, not silicon — measured per-sample cost + evidence-neutral
fix): the harness state digest hashed all of guest RAM every sample, and on N1 that,
not the work clock, was the cost floor.** `state_digest` sha256-reads the *whole* guest
RAM slot per sample; the offline apparatus set `RAM_SIZE = 64 MiB` on the unmeasured
assumption it "hashes cheaply." On real N1 it is **~0.45 s/sample**, scale-independent
(smoke and 1e6 cost the same) and memory-bound — rebuilding with N1-native crypto
codegen (`target-cpu=native`, hardware SHA-256) moved it **not at all**. At the
normative ≥10⁶ armed floor that is **~5 days on one pinned core**, and the aggregation
rule forbids spreading the four contamination conditions across cores, so there is no
parallel escape. Fix: `RAM_SIZE` → **4 MiB** (`harness/src/sys/machine.rs`, marked
`SPIKE(arm-altra)`). It is **evidence-preserving**: all payload state (image at
`+512 KiB` + `__stack_top` + bss) lives under ~1.5 MiB, the 60+ MiB tail is provably
always-zero (the ELF loader fails closed with `RangeNotMapped` on any over-range
segment; a guest write past the mapping faults rather than corrupting silently), so
hashing it added no divergence-detection power. Only the digest's *length* (hence hex
value) changes, and digests are compared only WITHIN a run-set (replay identity), never
across sets or against a golden — no measured count, overflow, or skid is affected.
Validated: post-fix, 8/8 (pre-spend smoke of the exact campaign config) and 80/80 (1e6)
armed records re-grade count-exact, delivered-exactly-once, payload-status clean.
Per-sample cost dropped 10–14× (smoke 0.458 s → 0.033 s; 1e6 0.49 s → 0.051 s),
bringing the normative floor into an overnight batch. `AA-5's Linux guest (not yet
built) takes its own larger slot; nothing in the bare-metal payload path exceeds 4 MiB.`

**Finding AA1-F4 (checker-vs-plan, two constraints that shape the campaign):** a first
launch (`r1`, a 1e6 bulk of 31,250 cases/payload) was killed 15 min in on two grounds.
(1) **The floor-checker's branch-dense oracle ceiling.** `count-exactness` re-simulates
the branch-dense oracle per distinct seed; at 1e6 that is 10⁶ trips/seed, so 31,250
distinct branch-dense seeds is 3.1×10¹⁰ > the `MAX_ORACLE_TRIPS` = 2×10¹⁰ fail-closed
guard — the bulk would be *ungradeable*. A run-set may carry at most ~20,000 distinct
branch-dense-1e6 seeds. (2) **Armed reps diverge by construction.** For an armed record
the replay key is `landed_digest` (state at the landing), which is skid-dependent, and
skid varies run-to-run (kick-latency jitter) — so `--reps > 1` on an armed run fails
replay-identity. Reps are for counting runs; armed runs use `reps = 1`.

**The two-scale decomposition (the fix):** the ≥10⁶ armed floor certifies overflow
**delivery reliability** (exactly-once) + count invariance — which the
arm→fire→signal-kick→land cycle exercises identically at any scale — so the volume runs
at **smoke** scale (branch-dense oracle only 1000 trips/seed → 31,250 seeds grades
trivially; and smoke is the cheapest cycle, ~0.033 s/sample). The **scale science** —
per-class density table, count-exactness at scale, grid-cell presence, and the
`skid_margin` (skid is scale-*dependent*: branches retired during a fixed kick latency
grow with the loop's branch rate, so the worst case is at 1e8) — rides a separate
**real-scale grid** (1e6/1e7/1e8). Validated end-to-end by a mini dress-rehearsal (all
four conditions + load + migration probe at tiny counts): aggregate floor-check
**PASS (148 checks)** sub-normative, and at the normative floor only the three volume
gates (share, armed-floor, case-coverage) short while **aggregation, the grid-cell
matrix, count-exactness, multiplicity, and the migration-probe carve-out all pass** —
i.e. the structure is proven; only the counts need to be large.

**Campaign launched (`host/aa1c-conditions.sh r2 31250 100 400`, detached, core 60):**
per condition, a **smoke bulk** (8 payloads × 31,250 = 250,000 armed overflows) + a
**1e6/1e7/1e8 grid** (8 × 3 × 100 = 2,400; density + skid + presence), over
`pinned-solo`, `co-tenant-other-core`, `memory-pressure`, `co-tenant-same-core` (last;
its same-core contention ~doubles that condition's wall time), then the bounded unpinned
**migration probe** (1e6 × 400/payload = 3,200 armed; rr #3607). Totals: ~1.01×10⁶ armed
cumulative, ~252k per condition. Bulk raw records (~100 MB each) stay on the box,
content-addressed by the manifest `records_sha256`; manifests + floor-check verdicts +
the smaller grid/migration sets land in git. Est. ~13 h. **Disposition: not yet
declared** — waits on campaign completion + the aggregate floor-check
(`--min-armed-overflows 1000000 --min-cases 100000`) + the derived `skid_margin`/density
pack read from the grid.

**Procedure on r2 completion (turnkey — for this session or a resuming one).** Success
marker is `~/aa1c-r2-OK` on the box (written only if every stage exited 0); progress is
in `~/aa1c-r2.log`; the 10 run-sets land under `results/aa-1c/aa1c-*-r2-{bulk,grid}` +
`aa1c-migration-r2` (+ `aa1c-presmoke-r2`). Then, on the box:
1. **Aggregate floor-check** over the 8 condition run-sets + the migration probe:
   `./target/release/floor-check results/aa-1c/aa1c-{pinned-solo,co-tenant-other-core,memory-pressure,co-tenant-same-core}-r2-{bulk,grid} results/aa-1c/aa1c-migration-r2 --min-armed-overflows 1000000 --min-cases 100000 > results/aa-1c/aa1c-r2-verdict.txt`
   — this must be `RESULT: PASS`; the verdict file is retained evidence (§Evidence
   integrity #2). A single failing check blocks the GO.
2. **Constants pack**: `python3 host/aa1c-skid-density.py results/aa-1c/aa1c-*-r2-grid results/aa-1c/aa1c-*-r2-bulk > results/aa-1c/aa1c-r2-constants.json` — the
   `skid_margin_candidate_by_scale` (worst case at 1e8) is the N1 `skid_margin`; the
   per-group density feeds the SimCpu re-parameterization.
3. **Disposition**: if the floor-check PASSES, record **AA-1(c) GO** (and thereby the
   AA-1 existential-trio disposition) here with the verdict + constants paths, the
   `skid_margin`, and the migration-probe finding (whether the rr #3607 missed-PMI mode
   appeared, from `aa1c-migration-r2`'s `lost_by_group`). Commit the manifests +
   verdict + constants (bulk raw stays on the box, content-addressed). If any count
   mismatch or missed/duplicate overflow survives → **NO-GO**, record the fallback the
   evidence selects (§6). The `skid_margin` becomes AA-3's landing-contract bound.

**PARALLEL PIVOT (Paul's directive, 2026-07-17) — AA1-F5.** The r2 serial campaign was
pinned to core 60 because the floor-checker's evidence-integrity backbone encodes the
*serial* pinning model: `check_aggregation` required every non-probe AA-1 run-set to
share ONE `pinning.core`, and the condition matrix wants the four conditions on that
core. Paul ruled that construct wrong for a no-SMT box: **"pinned-solo" is one workload
PER PHYSICAL CORE, not one-core-total** — sharding the matrix across the idle cores (each
tuple on its own dedicated core, concurrently) both collapses the ~1h45m serial run to
minutes AND *is* the co-tenant determinism stress test, because BR_RETIRED is per-core,
frequency-independent V-time (AA-1(b)): solo ≡ co-tenant digests MUST hold; any
divergence is a **P0** (stop and report, never serialize to hide it). Implemented: (a)
the checker's aggregation rule now permits per-shard cores at AA-1 (keeps pinned-flag +
governor + the weights/perf/environment/mechanism/images comparison; a per-core diff
cannot hide a count change — that surfaces as count≠oracle or a solo≠co-tenant digest);
(b) `host/aa1c-parallel.sh` shards each co-tenant condition across cores 4–79
concurrently (76-wide), with per-run posture attestation and RC-propagated over every
shard; (c) `host/aa1c-determinism-check.py` compares solo vs co-tenant **final
state_digest** per shared tuple (the P0 detector; the skid-dependent `landed_digest`
legitimately differs, so the cross-check shard is excluded from the floor aggregate).
Plan: keep the nearly-done pinned-solo lane as the quiet SOLO REFERENCE, then run the
three co-tenant conditions parallel-sharded (~minutes), plus the migration probe. The
per-core aggregation change is gated green (floor-check 67+32+3). The same sharding
applies to AA-2 box validation and AA-3–AA-6 (own core, concurrent, don't wait on a
campaign lock).

### AA-1(c) — **GO** (2026-07-17). AA-1 (the existential trio) — **PROVISIONAL GO**.

The parallel campaign (`host/aa1c-run-all.sh par1 420 6`; ~20 min wall, 256 run-sets)
returns clean at the **normative** floor. Evidence:
`results/aa-1c/parallel-evidence/` — `aa1c-par1-verdict.txt` (the aggregate floor-check
output, the authoritative graded record), `aa1c-par1-constants.json` (skid/density),
`aa1c-par1-determinism.json`; raw per-sample records (~800 MB) stay on the box under
`results/aa-1c/parallel/`, content-addressed by each manifest's `records_sha256` and
recomputed by the verdict.

**Aggregate floor-check: `RESULT: PASS (4084 checks)`** over 255 run-sets
(`--min-armed-overflows 1000000 --min-cases 100000`), zero FAIL, zero NOT-REQUESTED:

- **`1,022,880` armed overflows** cumulative across the four-condition matrix
  (pinned-solo / co-tenant-other-core / memory-pressure / co-tenant-same-core), **all
  delivered exactly once** — zero missed, zero duplicate (per-record multiplicity);
- **zero count mismatches** — every one of ~10⁶ records is count-exact vs the analytical
  oracle, including the 250,000-record quiet **solo reference** and every co-tenant
  shard (co-tenant count-determinism holds — counts are invariant under 76-way
  concurrency, same-core contention, and memory pressure);
- **`1,022,880` distinct armed target/seed cases** ≥ the 100,000 floor;
- the migration probe (bounded, unpinned, rr #3607): **3,200 armed, 0 lost, 0 duplicated**
  — the missed-PMI mode did not manifest under deliberate cross-core churn on this
  kernel/silicon (favorable; pinning stays the standing condition, not relaxed on the
  strength of one bounded probe).

**The solo ≡ co-tenant determinism check (Paul's directive, the P0 detector):
`verdict: MATCH`** — all 3,200 shared tuples across all 8 payload classes carry a
**bit-identical final `state_digest`, `measured_taken`, and delivery count** whether run
solo (quiet core 60) or under 76-way co-tenant load. Co-tenancy does not perturb the
digest. Zero divergences ⇒ no P0.

**Constants pack (§Definition of done #2):** the N1 **`skid_margin` = 53** — the
worst-case early/late skid over 3,744 delivered armed overflows on the real-scale grid,
scale-independent (the branch rate is the same loop at every scale), and **tighter than
the x86 folklore 256**. The full per-class density (`measured_taken` ranges, skid
distribution per payload/scale/condition) is in `aa1c-par1-constants.json` — the SimCpu /
PlannerConfig re-parameterization inputs. AA-3's landing contract must stay within this
53-event margin.

**Existential-trio verdict: PROVISIONAL GO** (§AA-1: clean but bounded — "report
confidence and coverage; do not call it a proof"). All three questions answered YES on
real N1: (1) `BR_RETIRED` counting is bit-deterministic on a pinned core (and, stronger,
on every core concurrently); (2) overflow PMIs arrive reliably exactly once out of
`KVM_RUN`; (3) the skid bound is 53. The named bound: this is the pre-patch
**signal-kick** mechanism; AA-3 must reproduce exactly-once delivery and a landing within
`skid_margin` on the patched in-kernel exit. Constants feed the SimCpu re-parameterization
and AA-3.

### AA-2 — single-step exactness: apparatus BUILT (offline, native-gated); box validation PENDING

**Update 2026-07-17:** the single-step run path is now **built and native-gated** (the
build below was executed per `harness/AA2-BUILD.md`), overlapped with the AA-1(c)
campaign. It stays **untested on silicon** — only the *measured* single-step semantics
need the box. What was added: the `KVM_SET_GUEST_DEBUG` seam (`sys.rs`/`sys/machine.rs`:
ioctl `0x4208_AE9B` — the **arm64** value, `_IOW(0xAE,0x9b,0x208)`; the plan's original
`0x4048_AE9B` was the x86 struct size and was corrected, pinned by a `size_of==0x208`
const-assertion, with one `TODO(box-verify)` to confirm the running kernel accepts it),
`GUESTDBG_ENABLE|SINGLESTEP`, `VBAR_EL1` read; a `StepVcpu` trait + `step_once` +
`step_run` (one `RunRecord`/step, `exit_reason==Debug`, window measured+stamped so the
oracle grades it) behind `--single-step`/`--stage aa2`, with `run_sample` untouched;
`classify_transition` reusing `scan.rs` decode (a hypothesis from opcode + observed
`pc_after`, never forced onto the measured delta). Gates re-run green (not trusted from
the builder): harness **122** tests + clippy `-D warnings` + fmt, aarch64-linux `cargo
check`/clippy (box code compiles), Miri over the new `guest_word` unsafe path, floor-check
**67+32+3** with a new `accept-aa2-steps` fixture grading PASS (18 checks) and three
reject fixtures each failing exactly `debug-evidence`.

**Box-validation findings (2026-07-17, first silicon contact — apparatus refinement
required before an AA-2 disposition):**

1. **The `KVM_SET_GUEST_DEBUG` ioctl `0x4208_AE9B` is ACCEPTED on N1** — the one
   `TODO(box-verify)` is RESOLVED. `--stage aa2 --single-step` arms guest single-step
   and steps (no `EINVAL`/`ENOTTY`); the mechanism works on silicon.
2. **Per-step cost is the blocker for a full-payload run.** `step_run` sha256-hashes the
   whole 4 MiB guest RAM into `step_digest` **per step** (the AA1-F3 pattern, now
   per-step), and a full smoke payload is tens of thousands of steps, so an 8-payload
   smoke sweep runs many minutes and did not finish in 250 s. A **step budget**
   (`--max-steps`) is needed for a bounded, class-covering validation — and it must
   handle the window/count coupling (a bounded run does not reach `MARK_END`, so the
   step records cannot carry the full window count; either exempt AA-2 step records from
   count-exactness or run to `MARK_END` while capping the recorded steps), and/or the
   per-step digest should hash only what changes per step (registers) rather than all RAM.
3. **Single-stepping the `llsc-atomics` payload LIVELOCKS on N1** — each single step
   clears the exclusive monitor, so `STXR` never succeeds and the retry loops forever
   (the run never reaches its sentinel). This is **exactly the architectural hazard AA-2
   exists to characterize** ("single-stepping an LL/SC loop can livelock outright") and
   is **direct AA-4 input** (it is the mechanism behind the LL/SC count-determinism
   minefield). It independently requires the step budget to bound the loop.

**Box-validation run (2026-07-17, on the `-aa3preempt` kernel — single-step is a stock KVM
feature the patch does not touch, so AA-2 semantics are identical there; the stock vmlinux
was lost to the build-patched clobber, so AA-2 rides the patched kernel too).** The
`--max-steps` refinement works: a bounded run (`--max-steps 12000`, all 8 payloads, reps 2)
wrote **170,330 step records** in ~74 s (registers-only per step ≈ 0.43 ms/step).

**AA-2 core result — GO on the primitive:** `KVM_GUESTDBG_SINGLESTEP` on N1 retires
**exactly one instruction per step for all 170,330 steps** (`insn_retired == 1`
everywhere; PC always advances). This is the trustworthy step primitive AA-3's exact
landing depends on.

**Per-step `BR_RETIRED` confirms AA1-F1 at single-step granularity** — the event counts
branch *instructions*, taken AND not-taken: measured deltas are taken-branch = 1,
`ERET` = 1, a **not-taken conditional branch** = 1 (pc+4 but the branch instruction
retired), and non-branch / `SVC` / `WFI` / LDXR/STXR-exclusive = 0. So a step that lands
at pc+4 is +0 only if it was a *non-branch*; a not-taken branch is pc+4 and +1.

**LL/SC single-step LIVELOCKS deterministically** (foreman #4): the `llsc-atomics` retry
loop at **`0x40080880`–`0x40080890`** (LDXR / STXR / CBNZ) never completes — every single
step clears the exclusive monitor, so STXR always fails and the loop cycles forever
(bounded here at `--max-steps`; ~2,478 steps per loop PC). This is the architectural
hazard AA-2 exists to surface and **direct AA-4 input** — under single-step the livelock
is *deterministic* (same retry sequence every rep), unlike AA-1's spontaneous STXR
divergence.

**Two apparatus-model fixes needed before a GREEN grade** (the FAILs are model bugs, not
hardware): (1) `classify_transition` / `StepTransition` / `check_debug_evidence` predate
AA1-F1 — a not-taken *branch* (pc+4) must be classified with `br_retired_delta == 1`,
distinct from a non-branch `Sequential` (delta 0); (2) `check_replay_identity`'s rep-key
`(payload, scale, seed)` lumps every step of a group together and compares their
necessarily-different per-step digests — it must include the step position (pc/index) so
step *N* of rep 1 compares to step *N* of rep 2.

The two apparatus corrections (the AA1-F1 `NotTakenBranch` classification + the
step-position rep-key) were made and the **same run re-graded: `RESULT: PASS (19 checks)`**
(evidence `results/aa-2/aa2-verdict.txt`). `debug-evidence` PASS — all 170,330 records
cover the full 8-class step matrix, each a valid single step whose `BR_RETIRED` delta
matches its class (AA1-F1: taken/not-taken branch and `ERET` = 1, else 0);
`replay-identity` PASS — **85,165 stepped groups each bit-identical across reps** (single
step is not just exact but DETERMINISTIC on N1, the llsc livelock included — it binds under
AA-2, no carve-out).

**Disposition: GO** (2026-07-17). Stock `KVM_GUESTDBG_SINGLESTEP` on N1 retires **exactly
one instruction per step**, `BR_RETIRED` increments per the AA1-F1 branch-instruction rule,
and stepped states are **replay-identical** across the exception/`ERET`/`WFI`/injection
classes and through LL/SC sequences (deterministic livelock). The 0005-analogue is confirmed
nearly-free — no patch needed. The trustworthy step primitive AA-3's exact landing depends
on is validated. **Single characterized caveat** (the LL/SC-stepping result AA-4 inherits):
single-stepping an exclusive sequence livelocks (the monitor clears every step); a bounded
step budget is mandatory, and stepping through exclusives cannot land — direct AA-4 input.

**Evidence-trail note (J2, corrected 2026-07-18): step totality is now certifiable standalone.**
In single-step mode one planned sample emits many step records, so the harness densely
renumbers `sample_id` and sets `attempted` to the step count — which, alone, let record-level
totality read a run that dropped a *later* planned sample (after earlier ones emitted steps) as
complete. The checker records the **planned sample count** in the manifest (`planned`), and
every step carries the plan's stable `planned_sample_id`. The **`step-totality`** check requires
the distinct ids to be exactly `0..planned`; duplicating one run's `step_index == 0` row cannot
conceal another dropped plan entry (pinned negative control
`reject-aa2-dropped-planned-sample`; the fix does not lean on the harness exit code).
The **retained AA-2 evidence** here (`aa2-verdict.txt`, 170,330 steps) is historical schema-v3
evidence, predating the stable `planned_sample_id` field. Its record set completed at **harness
exit 0**, and the retained v3 checker transcript remains evidence of the physical result, but
the v4 checker deliberately refuses to re-certify that older shape. Every new step run emits
schema v4 and self-certifies plan totality from its records.

### AA-3 — deterministic force-exit (0004-analogue) + exact landing: GO (regenerated-pin basis, Paul-ruled 2026-07-22)

Started per the foreman's "continue straight to AA-3, keep the box saturated" directive
(overlapped with the AA-2 refinement). The draft patch
(`host/patches/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch`,
`KVM_EXIT_PREEMPT`=42 / `KVM_ARM_PREEMPT_EXIT`=`_IO(KVMIO,0xe4)` /
`KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS`=245, hooking `ARM_EXCEPTION_IRQ` in
`handle_exit.c`) **applies clean to a fresh linux-6.18.35 on the box** (`patch -p1`, all
four files, mechanism symbols asserted present). The **patched kernel is building
natively** (`host/build-patched-6.18.35.sh`: LOCALVERSION `-aa3preempt`, so it installs
alongside stock 6.18.35 with its own build-id + `/boot` entry — stock-vs-patched is a
one-variable experiment; `CONFIG_KVM=y` built-in, so the patch is exercised only after a
reboot into it). One build-script fix recorded: `yes '' | make olddefconfig` under
`pipefail` races on `yes`'s SIGPIPE (exit 141) and can kill the script after config
succeeds — changed to `make olddefconfig </dev/null`. **Pending:** install the patched
`.deb`, reboot into `-aa3preempt` (a reboot — coordinate authorization as for the 6.18.35
boot), confirm the kernel advertises `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS`, then drive
the full landing contract (`run_until_overflow` + `single_step`, the patched
`KVM_EXIT_PREEMPT` exit attested per-record, ≥10⁶ armed deadlines with `work==target`,
skid within the AA-1 margin of 53) — this uses the **AA-3 `patched` mechanism** the
checker requires, and depends on AA-2's validated step primitive for the exact landing.
The trait-freeze memo to `docs/ARCH-BOUNDARY.md` is an AA-3 deliverable.

**Box results (2026-07-17): the patched force-exit MECHANISM is confirmed on N1.** The
box rebooted into `-aa3preempt` in 154 s (build-id `df0f4f02` matches the built vmlinux),
and the kernel **advertises `KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS`**
(`found=present` — the favorable deviation from the stock-absent baseline). A patched-
mechanism smoke (`--stage aa3 --mechanism patched --with-targets`) fires the in-kernel
exit: **`exit_reason=preempt`, `deliveries=1`** — the 0004-analogue `KVM_EXIT_PREEMPT`
works on real silicon. The delivered records land with **skid 0–52, inside the AA-1
`skid_margin` of 53** (the four `mmio`/no-fire records are smoke targets that exceeded the
window — a plan-clamp detail, not a mechanism miss).

**The remaining gap (unbuilt apparatus, like AA-2's step path): the EXACT landing.** The
harness arms *at* target and records where the Preempt fired (`target + skid`), so it does
not yet land `work == target` — the `run_until_overflow` + `single_step` contract (arm at
`target − skid_margin`, take the Preempt exit below target, then single-step the remaining
≤53 events to exactly target, using AA-2's validated `step_once`) is **not implemented**
(`grep run_until` in `run.rs` is empty). Building it is the AA-3 core work — sequenced
after the AA-2 classifier fix lands (both touch `run.rs`), then the ≥10⁶-armed exact-
landing run + the trait-freeze memo. **Disposition: PENDING — mechanism GO on N1, exact-
landing apparatus to build.**

**Mechanism-reliability probe (sharded, 32 cores, arm-AT-target proxy).** Over **256,000**
armed patched overflows under 32-way co-tenancy, mechanism-attestation PASSES (every record
carries the `Preempt` exit) but **3,092 (1.2%) were LOST** (`deliveries==0`), 0 duplicated.
This is NOT a general PMI-loss verdict — AA-1c's stock SignalKick lost **zero** over 10⁶ at
these scales. It is the **arm-AT-target boundary race**: arming the one-shot exactly at the
overflow point occasionally misses and the guest runs to its sentinel. That is exactly what
the exact landing removes by arming at **`target − skid_margin`** (53): the overflow fires
reliably BELOW target, then single-step walks up to `work==target`. So the probe *validates
the arm-early design*; the real AA-3 reliability+exactness verdict comes from the exact-landing
run, not this proxy. Recorded so the 1.2% is not misread as a mechanism NO-GO.

**Exact landing DEMONSTRATED on N1 (`run_until_overflow` + `single_step`).** With
`--skid-margin 53`, the patched path now arms at `target − 53`, takes the `Preempt`
below target, and single-steps up: a 1e6 smoke of 32 samples lands **`work == target`
EXACTLY on all 32** (skid 0, `exit=preempt`, `deliveries=1`), and — the arm-early
payoff — **0 lost** (vs the 1.2% of the arm-at-target proxy). `check_skid` PASSES the
AA-3 `exact_required` bar ("no overshoot; all landings within margin and exact"), with
multiplicity and mechanism-attestation. Count-exactness holds for **7 of the 8 payloads**
(all the deterministic-count classes). The exception is **`wfi-idle`**, whose window
count comes back *short and varying* (e.g. 15,826 / 17,784 / 13,554 vs the oracle's
20,000): its WFI is resumed by a **timer** whose firing shifts under the exact landing's
slow single-step, so the number of retired branches varies. This is a real-time timer
dependency the fast free-run hid — an **AA-5 preview**: exactly the kind of non-work-
derived time the paravirt clock (§AA-5) exists to close. It is a characterized finding,
not a mechanism failure; the exact landing itself is exact for every payload.

**Finding AA3-F1 — the exact landing must land at the CANONICAL PC, because `BR_RETIRED`
does not uniquely pin `PC`.** `BR_RETIRED` ticks only on retired branch instructions, so across a
branchless run many consecutive `PC`s share one work value — a **plateau**. Of the seven
graded payloads, `clock-page`'s seqlock body (≈10 non-branch instructions between its loop
branches) has the longest plateau. The exact landing's canonical point is the **first**
instruction at which `work == target` — the one immediately after the target-th retiring
branch — and the single-step-up loop reaches it from *any* start strictly below the target,
since the deterministic instruction stream converges there. A first cut accepted an async
`Preempt` that fired *at* `work == target` as a zero-step landing; that was wrong. Such a
`Preempt` lands at an **arbitrary `PC` inside the plateau** — BR-exact but PC-non-canonical —
so two same-seed reps, one landing via a step-up and one via that boundary, digest **different
`PC`s** while everything else (all GPRs, `SP`, guest RAM, vGIC) is **bit-identical**. It
surfaced as a replay-identity divergence on `clock-page` *only* (its long plateau); an earlier
smoke had passed merely because it *errored those boundary cases out*.
Root-caused with an env-gated per-register landing dump (`AA3_DUMP_REGS`, kept for AA-6): the
sole diverging *hashed* register was `PC`; `CNTPCT_EL0` and `KVM_REG_ARM_TIMER_CNT` varied
240/240 on a **passing** payload yet replay held — confirming `is_host_time_register` already
excludes the wall-clock counters from the digest (the paravirt-clock contract-closure, §AA-5,
previewed in the digest). **Fix (proven on-box):** arm `skid_margin + LANDING_HEADROOM` (16)
below the target so the `Preempt` fires *strictly* below it with room to single-step up to the
canonical `PC`; the `≥ target` guard stays fail-closed (a landing at/above target means the
skid exceeded margin+headroom — a real anomaly, not something to accept). This is the deep
lesson of a branch-instruction work clock on real silicon: a `Moment` named by work count is a
`PC`-*interval*, and replay identity requires a canonical representative of that interval.

**DISPOSITION: GO — regenerated-pin basis, Paul-ruled 2026-07-22 (un-voids the 2026-07-18 void).**
The GO was *voided 2026-07-18* after verification found the campaign scripts did not invoke the
determinism comparators and the old comparators accepted intersections. A coverage-asserting
recomputation over the retained raw records then produced full-join MATCHes (AA-1C 3,200/3,200;
AA-3 5,700/5,700, zero missing keys or divergences), so no physical result was falsified. The
required re-verification is now **complete on-silicon** with the comparators properly invoked (task
137; see the *On-silicon re-cert (2026-07-22)* subsection at the end of this AA-3 section), and Paul
ruled the regenerated-pin basis acceptable — so the cert is **re-issued GO**. Original (voided-era)
evidence in `results/aa-3/exact-evidence/`; the re-cert acceptance evidence in
`results/aa-3/recert-20260721/` and `results/aa-3/sweep-01…11/`:

- **≥10⁶ sharded run** (`aa3-exact-r3`, 76 shards pinned across cores 4–79, run concurrently
  — the concurrent run *is* the co-tenant stress test). Aggregate `floor-check` over all 76
  run-sets with the normative floors (`--min-armed-overflows 1000000 --min-cases 500000
  --min-reps 2`, **no** `--sub-normative`): **RESULT PASS (1371 checks)** —
  **1,010,800 armed overflows** (≥10⁶), **505,400 distinct** (payload, scale, seed, target)
  cases, and every per-shard check green: totality, multiplicity, count-exactness, **skid = 0
  exact** (no overshoot on any of 1.01M landings), mechanism-attestation = `Preempt`,
  replay-identity, rep-floor, pinning, perf-config (raw 0x21 guest-only). `verdict.txt`.
- **Co-tenant determinism (Paul's P0 rule): retained full-join MATCH; certification pending.** A solo reference lane
  (`aa3-exact-solo-ref`, run alone on an idle box, base seed shared with co-tenant shard s0)
  vs the co-tenant shard: **5,700/5,700 joined tuples, 0 missing/extra, 0 multiplicity
  mismatches, 0 divergences** — every tuple's exact-landing
  digest *and* window-end full-state digest is bit-identical solo-vs-co-tenant. Co-tenancy
  under 76-way concurrency perturbed no deterministic guest state. `determinism.json`.

Per the foreman ruling (2026-07-17) `wfi-idle` is **excluded** from the run (its timer
determinism is AA-5's, recorded above as the AA-5 preview). `llsc-atomics` is **carved from
replay-identity** (checker + determinism comparison alike): its landed state diverges even
within a solo lane — the §4 spontaneous STXR fail/succeed hazard, AA-4's domain, and the
comparator's strict within-lane check *re-confirmed it is live*. The trait-freeze memo is in
`docs/ARCH-BOUNDARY.md`: `run_until_overflow`'s late-only-stop contract **holds** on N1 (the
Preempt is late-only; spurious host-IRQ exits below the armed point are distinguished by the
work counter and re-armed; multiplicity = 1 delivery across all 1.01M, so the N1
missed-PMI-on-migration hazard did not manifest pinned) — **no `Arch`-trait change forced**.

**Original finding (why this was executor work): the single-step run path did not exist
in the harness** — the
offline apparatus deliberately left it out (building it would presume AA-2's own
single-step result, which the pre-build ruling forbids inventing; the counting loop even
refuses an unrequested `KVM_EXIT_DEBUG`, and `check_debug_evidence` reads AA-2 as
`NOT-REQUESTED` until real stepped records exist). So AA-2 execution is gated on building
the step path first. That build is offline-buildable and native-testable (the
scripted-vCPU seam); only the *measured* single-step semantics need the box. Full design
+ build + native-test + box-validation plan: **`harness/AA2-BUILD.md`** (the record
format is fixed and must not change; the three pieces are the `KVM_SET_GUEST_DEBUG` ioctl
seam, transition classification reusing `scan.rs`, and a `step_run` mode behind
`--single-step`). Runs after AA-1(c) frees the box; the `skid_margin` AA-3 needs comes
from AA-1(c), the trustworthy step primitive AA-3 needs comes from here.

**On-silicon re-cert (2026-07-22, task 137 / hm-idb) — GO, regenerated-pin basis, Paul-ruled.**
The voided AA-3 GO was re-run on a fresh Ampere Altra / N1 box (`ssh harmony-arm`, HPE ProLiant
RL300 Gen11) on the currently-loaded `6.18.35-aa3preempt` kernel, using the certified AA-3 apparatus
built from commit `48d519f` — this time with the determinism comparator **properly invoked** (the
omission that voided the original). Acceptance was MET at scale, twice over:

- **Acceptance run** (`results/aa-3/recert-20260721/full/`): the full ≥10⁶ campaign passed **every**
  gate — aggregate `floor-check` **`RESULT: PASS (1371 checks)`** (1,010,800 armed overflows,
  505,400 distinct cases, skid = 0 exact / no overshoot, Preempt attested exactly-once,
  replay-identity), and solo-vs-co-tenant **full-join `MATCH` 5700/5700, zero divergences** under
  76-way co-tenancy.
- **Continuous overnight sweep** (`results/aa-3/sweep-01…11/`): **11 further independent ≥10⁶
  cycles** across fresh seed bases (`…0100`–`…1100`), **every one** `floor-check PASS (1371 checks)`
  with `solo==co-tenant MATCH` — **12 green ≥10⁶ campaigns total, ~12.1M armed overflows, zero P0**,
  solo==co-tenant held on every cycle.

**Basis (explicit).** The box was account-wiped (2026-07-20) and its toolchain reinstalled
(2026-07-21, `rustc 1.97.1`) on a *different physical Altra*, so the **certified payload bytes**
pinned in `results/aa-1b/inputs/payload-pins.json` are non-reproducible; the campaign used
**regenerated pins** from a fresh build of the git-verified byte-identical certified source. The
byte-pin-independent SEMANTIC gate — **`count-exactness` (`work == target`)** — passes on every
landing, so the pins attest the *same* payloads, recompiled. **Paul ruled 2026-07-22 that the
regenerated-pin basis is acceptable** ("for ARM, we need to just go. Let's accept the regenerated
pins, and get going to AA-6"). This is a Paul-ruled GO on the accepted basis, not a new determinism
claim. The trait-freeze memo (`docs/ARCH-BOUNDARY.md`) stands re-issued (no `Arch`-trait change
forced). Decision writeup + evidence: `results/aa-3/recert-20260721/STATUS.md`.

**Durable prevention (tracked so future re-certs reconstruct, not re-decide, the basis):**
**hm-nji6** — reproducible + archived payload pins (build the pinned artifacts deterministically and
retain them, so a wipe cannot strand the certified bytes); **hm-gfr1** — a static/reproducible
hardware + toolchain definition (NixOS-ish) so the exact certified environment is reconstructable.

### AA-4 — the LL/SC vs LSE ruling: CHARACTERIZED; recommended ruling below (Paul ratifies)

Per the foreman's directive (2026-07-17): produce the LL/SC characterization + recommended
ruling; Paul ratifies at PR time. The evidence rides the AA-3 apparatus — the exact-landing
run already carries an `llsc-atomics` payload (LL/SC) and an `lse-atomics` payload (LSE-only,
same computation) under an identical injection schedule (the arm-early `Preempt` + single-step
IS the event injection at a seeded `Moment`). Evidence: `results/aa-4/` and the AA-3 records.

**(a) The hazard, quantified on real N1 silicon** (`host/aa4-llsc-characterize.py`,
`results/aa-4/llsc-characterization.txt`). Across the ≥10⁶ run, `llsc-atomics` tuples diverge
run-to-run — and the divergence is **in the work clock itself, not merely in state**:
`measured_taken`/`work_end` differ by **±2 retired branches** between same-seed reps (one
extra `LDXR`/`STXR` monitor-clear retry). Rates: **26.3 %** of tuples in the *solo* lane (so the
hazard is **intrinsic**, not a co-tenancy artifact) and **31.6 %** under 76-way co-tenant load
(neighbours raise the spurious-`STXR`-failure rate via cache/monitor pressure, as §4 predicts).
`payload_status = 0` throughout: the computation is *correct*, only its branch count is
non-deterministic. The exclusive loop is two instructions —
`0x40080880 ldxr x4,[x2]` / `0x40080888 stxr w5,x4,[x2]` — the **same PCs** as the AA-2
single-step LL/SC livelock (single-stepping clears the monitor every step, so `STXR` never
succeeds): the runtime spurious-failure hazard and the single-step livelock are the same loop.
**Why this is fatal, not tolerable:** a non-deterministic *work clock* defeats the entire
V-time thesis — LL/SC cannot be mitigated down to acceptable, it must be made unreachable.

**(b) The answer, demonstrated at scale.** `lse-atomics` — the identical algorithm rebuilt
LSE-only (single-instruction `CAS`/`SWP`/`LDADD`, no monitor, no retry) — diverges in
**0 of 73,150 tuples** (72,200 co-tenant + 950 solo), in *both* count and state. LSE is
perfectly work-clock-deterministic under the same injection. N1 has FEAT_LSE (mandatory since
ARMv8.1; Altra/Neoverse N1 is ARMv8.2), so an LSE-only guest is buildable on the target.

**(c) The enforcement ladder, mechanisms demonstrated on the owned artifacts:**
- **Level 1 — LSE-only build.** DEMONSTRATED: `lse-atomics` *is* an LSE-only build, clean by
  scan and deterministic by measurement. For the guest: `-march=…+lse`, `-mno-outline-atomics`
  (kill libgcc/compiler-rt's LL/SC outline fallback), and removal of the vanilla kernel's LL/SC
  fallback bodies — exercised against the real guest kernel at AA-5.
- **Level 2 — executable-page opcode scan.** BUILT + run: `host/aa4-exclusive-scan.py`
  (`results/aa-4/exclusive-scan.txt`). It scans raw instruction words for the monitor-exclusive
  family using the broad class `(insn & 0x3f800000) == 0x08000000` plus the required o1/size
  discriminator (LDXR/STXR/LDAXR/STLXR/LDXP/STXP…; deliberately EXCLUDING LDAR/STLR and LSE
  `CAS`/`CASP`), and **self-validates** the raw decoder against every word `objdump` renders as an
  instruction. Result: it flags the two exclusives in `llsc-atomics` at their exact PCs and passes
  every other payload — including `lse-atomics` — CLEAN, exiting non-zero to reject. The AA-5
  owned-image build now also applies the direct-LSE kernel patch and requires `vmlinux`, the vDSO,
  and the freestanding init to scan CLEAN; a planted LDXR/STXR ELF must be rejected with exactly
  two hits. This completes the static build gate. The same primitive wired into a live W^X
  rescan-on-exec path is the Level-3 guard below — now proven live on N1.

  *Section-aware pre-flight (2026-07-21; hm-jth / hm-7o68-F3).* The static scan is a **section-aware
  PRE-FLIGHT**, not the authoritative W^X gate. Its raw executable-`PT_LOAD`-segment walk (F3-SCAN-SEG)
  initially rejected the rebuilt owned kernel with **424 false positives** — every one a data word
  (`.rodata`/`.altinstructions`/`.data`) that shares vmlinux's writable-executable init `PT_LOAD` in
  the ELF but is mapped **non-executable at runtime** under `STRICT_KERNEL_RWX`; the executable
  *sections* were clean. The scanner now **excludes defined non-exec data sections** from the segment
  walk, while still scanning every executable section and any word in **no** section (the
  forged/stripped-header case). This is tighter-and-truer, not a loosening, because the
  **AUTHORITATIVE** W^X enforcer is the Level-3 runtime execute-guard: it is default-XN and rescans the
  actual bytes of any page the guest makes executable, so it does not trust ELF section metadata. The
  `aa4-mislabel-evasion` fixture proves the split carries no evasion hole — a forged ELF that hides an
  `LDXR/STXR` pair in a data-labelled page **passes** the static scan (the evasion) but the runtime
  guard **rejects it at entry** on N1 (`results/aa-4/live-20260721/mislabel-evasion.guard-reject.out`:
  `gpa=0x40081000`, both exclusives decoded, `pc_before==pc_after` — the guest never advanced).
- **Level 3 — stage-2 execute guard. PROVEN ON N1, 2026-07-20** (`results/aa-4/live-20260720/`). Stock
  Linux 6.18.35 arm64 KVM recognizes an instruction fault but adds
  `KVM_PGTABLE_PROT_X` inside `user_mem_abort()` and resumes; it exposes no userspace per-GFN XN
  attribute and no execute-fault exit. `KVM_SET_MEMORY_ATTRIBUTES` is x86-only here and defines
  only `PRIVATE`; `KVM_EXIT_MEMORY_FAULT` does not report RWX. Dirty logging is retrospective,
  and `userfaultfd` has no execute event. A non-vacuous backstop therefore requires a Harmony KVM
  state machine: default XN → exit before first execute → scan → approve executable/read-only;
  write fault → exit before modification → revoke execute → rescan before later execution.
  `host/patches/0002-*` now implements that page-granular state machine with non-reused scan
  generations, notifier/memslot invalidation, and a documented unique-backing/no-DMA VMM
  boundary; the exact pinned series applies and compiles. The harness now has an explicit guarded
  constructor and exact-generation response loop: `linux-boot --stage2-exec-guard` requires
  nonzero execute/scan/approval counts, while `aa4-guard-reject` hash-verifies a planted ELF and
  requires an exclusive-bearing generation to be rejected with the PC still in that page.
  `aa4-guard-write` pins a page-aligned self-modifier and requires the original page hash at first
  scan and the pre-store write exit, then deliberately replays that first approved generation
  while the exact expected modified page is frozen at a fresh generation. The old token must
  return `EINVAL` before the exact current token is approved. The fixture passed TCG
  liveness/protocol twice pre-silicon; on 2026-07-20 the full guard ran on real N1 under host
  `6.18.35-aa4guard` (patches 0001+0002, build-id `ac576f87…`, cap
  `KVM_CAP_ARM_STAGE2_EXEC_GUARD=246`, exit 43, core 60 isolated). Retained evidence
  (`results/aa-4/live-20260720/`) proves on silicon: pre-execute rejection of a hazardous page
  (1 scan → 1 rejection, PC unchanged); selective approval of a clean LSE page (ran to an MMIO
  exit — the guard is not blanket-reject); exit-before-modification with revoke-execute, exact-page
  rescan at a newer generation, and a replayed stale-generation approval rejected with `EINVAL`;
  memslot-notifier-replacement forced rescan; distinct-backing-move forced rescan (the approval is
  keyed to the mapping, not a content hash); and the two-vCPU scan/write race (a write behind a
  concurrent pending scan is blocked). Each concurrency gate carries a self-verifying negative
  control. This is now level-3 evidence. (An unmapped-GPA abort, `BRK`, or post-execution dirty
  scan would not have been a substitute — none of these gates rely on one.) The live run also
  surfaced a real W^X contract implication: the guard scans whole executable *pages*, so guest
  text and any exclusive-bearing rodata must be page-separated (payload `.rodata` is now
  page-aligned), mirroring the guest kernel's `STRICT_KERNEL_RWX`.

**CURRENT RULING: cooperative residual on *stock* KVM; the stronger mechanically-unreachable
property now holds on the patched `6.18.35-aa4guard` host — the execute-guard was booted and
passed its planted reject / write / race / invalidation proofs on N1 (2026-07-20).**
- *Static owned image — unreachable at publication.* The guest ships LSE-only (Level 1), and the
  opcode scan (the completed static half of Level 2) fails closed if any exclusive survives in
  the kernel, vDSO, or init artifact: outline-atomics fallback, hand assembly, or a stray kernel
  fallback body. On N1 this is real: LSE is present, and the LSE build is measured bit-identical.
  This closes the bytes published in the owned image, not code generated or modified at runtime.
- *Residual (cooperative, presently unbounded by the VMM): runtime-generated exclusives.* Code
  produced after the static scan — a guest JIT or self-modifying page emitting `LDXR`/`STXR` —
  can execute on stock KVM without a userspace-visible permission transition. The owned guest
  disables modules, BPF JIT, kprobes, ftrace, livepatch, and other known code-generation paths,
  so this is outside its cooperative contract. The execute-guard capability that mechanically
  closes it now exists and was exercised on N1 (2026-07-20): on the `aa4guard` host (patch 0002)
  this residual is mechanically closed; on stock KVM (no patch 0002) it remains a
  cooperative-contract residual.
- *Non-residual: the single-step livelock (AA-2).* A measurement-time hazard (single-step clears
  the monitor), not a runtime one; the LSE-only ban removes every exclusive there is to
  single-step, so it cannot arise in a shipped guest.

**Disposition: AA-4 CHARACTERIZED; static owned-image gate complete; Level 3 execute-guard now
proven live on N1; residual on *stock* KVM is cooperative.** (a) and (b) are demonstrated at scale
on real N1. For (c), Level 1 and Level 2's static artifact half are built and cross-verified against
the owned kernel/vDSO/init, and the Level-3 arm64 KVM execute-guard (patch 0002) was booted on the
pinned host and passed the non-vacuous planted reject/write/race/invalidation proofs on N1,
2026-07-20 (`results/aa-4/live-20260720/`) — so live W^X rescan-on-exec is no longer open. Native
publication of the owned image on the pinned N1 remains the standing follow-up.

### AA-5 — the paravirt work-derived clock: (a)+(b) DEMONSTRATED; (c) boot + clock mechanism PROVEN on N1 (full-RAM entropy residual open)

The centerpiece. Evidence in `results/aa-5/` and the AA-3 records.

**(a) Payload-level determinism — demonstrated (AA-3).** `clock-page` reads a materialized
work-derived clock page via a seqlock, with no `CNTVCT`/`CNTPCT` in the read path (the payload
`payloads/oracles/src/asm/clock_page.s`). In the ≥10⁶ AA-3 run it landed **bit-identical across
same-seed reps** (replay-identity PASS after the AA3-F1 canonical-landing fix) and **MATCH**
solo-vs-co-tenant. Those retained payload runs used a *static* placeholder
(`FLAG_WORK_DERIVED` clear — the plumbing, not a live refresh), so their clock value was
trivially wall-clock-invariant; they are not retroactively claimed as a live-refresh result.
The Linux executor's `hm-8h8` value-advancing path stamps from the skid-free exact-work anchor,
not from natural-exit live counts, and ran on N1 in the 2026-07-20 boot
(`results/aa-5/live-20260720/`), where same-seed console and register digests held bit-identical. The
retained digest **excludes** the live host-time counters (`is_host_time_register`), verified in AA-3
where `CNTPCT_EL0`/`KVM_REG_ARM_TIMER_CNT` varied 240/240 on a passing payload while replay
held — wall-clock never reaches a compared digest.

**(b) Closure — premise + scanner demonstrated.** Premise from AA-0: `ID_AA64MMFR0_EL1.ECV =
0x0` on N1 (all captures) — **FEAT_ECV absent**, so `CNTVCT_EL0` cannot be trapped in hardware
and raw-counter closure MUST be contract-level (exactly why the paravirt clock exists). The
counter-read closure scan (`host/aa5-counter-scan.py`, mirroring the unit-tested harness
primitive `scan::decode_counter_read`; `results/aa-5/counter-scan.txt`) is the build/rescan
layer: raw-opcode decode **self-validated against `objdump`**, with a **positive control** that
rejects a `CNTVCT_EL0`/`CNTPCT_EL0` read and correctly allows the constant `CNTFRQ_EL0`. Every
payload scans **counter-clean**. The owned-image build now runs the scan against `vmlinux` and
the vDSO and cross-verification reports zero live counter reads (constant `CNTFRQ_EL0` reads
remain allowed). Remaining, kernel-dependent: the EL0
`CNTVCT_EL0`-read-undefs-under-`CNTKCTL_EL1` live test and `CNTHCTL_EL2` posture.

**(c) The Linux smoke — boot + clock mechanism PROVEN on N1, 2026-07-20 (`results/aa-5/live-20260720/`); full-RAM state identity has a characterized kernel-CRNG entropy residual.** The
harness now has a portable-tested, arm64-Linux-cross-clippy-clean boot substrate: a total flat
Image loader with bounded Image/initramfs placement, deterministic generated DTB, Linux EL1h
entry (`x0=DTB`, reserved args zero), Linux-only PSCI 0.2 vCPU opt-in, the existing in-kernel
vGICv3, and a bounded PL011/PrimeCell console loop. The `linux-boot` command verifies trusted
Image/initramfs sha256 pins over hard-bounded reads immediately before VM construction, pins the
vCPU thread, and stops on a fixed console marker. With the operator-supplied measured skid
margin, it now requires the patched Preempt mechanism, reads the guest's actual `CNTFRQ_EL0`,
starts an exact cadence from work zero, and accepts the owned guest's page GPA only through
`INTEGRATION` §1.3's validated one-shot MMIO registration. No natural registration/MMIO exit
stamps time: the first landing after registration writes canonical ABI v1 at an exact retired-
branch target, and every later page refresh uses the same AA-3 arm-early + single-step primitive.
The output records the pinned GPA, publication count, maximum work gap, last exact anchor, and
clockevent assertion/ACK/lateness counts. At each exact publication the host compares the page's
guest ticks with the guest's absolute MMIO deadline and raises dedicated unowned PPI 20 when due.
The level remains high until the guest IRQ handler ACKs before calling the generic event handler;
success requires the handler to rearm and a later exact publication. KVM's architected timer owns
its default PPI 27, so using it for userspace injection would be silently ignored despite a
successful ioctl. The harness instead encodes vCPU0/PPI20 as `0x02000014`, verifies the vGIC
external-input level after every assertion and deassertion through
`KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO`, and binds that line level plus the pending deadline into the
replay digest. Reading `GICR_ISPENDR0` would be vacuous here: KVM's level injection changes the
line-level bitmap, not necessarily the userspace-visible pending latch. The DT requires the
generic `nohlt` poll loop because work time cannot advance while the sole vCPU sleeps in WFI.

The path was deliberately designed to be **non-vacuous**: the console marker alone does not prove
its producer, so the marker is latched at its UART exit but accepted only after the next exact
refresh publishes; the owned init spins after READY so a lost/late Preempt cannot pass with a stale
page merely because userspace printed the expected bytes. On 2026-07-20 this path ran on the Altra
N1 (`results/aa-5/live-20260720/`, host `6.18.35-aa3preempt` = stock + patch 0001, per-run
mechanism attestation with a stock-host control failing closed): the guest boots to userspace and
steady state (`HARMONY_AA5_CLOCKSOURCE_OK`, no RCU stall), same-seed **console** and **register**
digests are bit-identical (console on the pinned `980b7982…` image; the `regs_only` register digest
on a nokaslr `bc6f29b0…` diag build — see the results README image-provenance note), the counter is
fully page-routed (0 raw `cntvct` in `vmlinux`), and EL0 raw-counter access is closed
(`EL0_CNTVCT_PAGE_OK`). Same-seed **full-RAM** state identity is **not**
achieved: a characterized kernel-CRNG entropy residual (`base_crng`/`input_pool` reseed *content*
varies — 400–700 differing bytes in 256 MB, console and registers unaffected, divergence unstable
run-to-run) remains — a subsystem distinct from the clock, tracked as the entropy-closure contract
row (`docs/PARAVIRT-CLOCK.md` §4.3). The register-digest identity is **nokaslr-conditional**: the
pinned image is `RANDOMIZE_BASE=off`, so kernel VAs are stable run-to-run; a KASLR build would
diverge register digests by construction (tribunal F1-REG). AA-5(c) therefore claims the work-clock
plus counter/input closure and architectural (console + register) determinism, **not** full-RAM
identity.

The exact landing also inherits AA-4's LSE-only precondition: single-stepping through an
`LDXR`/`STXR` sequence clears its monitor and can add retries or livelock. The arm64 recipe now
patches the kernel to direct LSE, removes the unused futex LL/SC helpers from the config, replaces
BusyBox with a freestanding LSE-only init, and scan-gates the exact kernel/vDSO/init artifacts.
`linux-boot` still accepts a trusted hash pin rather than re-proving that property at load time.
The live W^X rescan-on-exec + stage-2 backstop are **no longer open** — proven live on N1 at AA-4
(`results/aa-4/live-20260720/`) — but full AA-5(c) state identity still awaits entropy closure, so
the 2026-07-20 runs are on-silicon spike evidence for the clock and W^X mechanisms rather than a
standing AA-4/AA-5 GO certification.

The tree now has a native Linux/aarch64 build recipe for the pinned kernel and a freestanding
syscall-only rootfs. Its v6.18.35 patches route the four shared physical/virtual counter accessors
(including the clocksource, sched_clock, delay, and erratum/CVAL call sites) through the ABI-v1
page, disable the vDSO fast path and EL0 counter access, name the selected source
`harmony-arm-pvclock`, emit kernel atomics directly as LSE, and refuse Image publication unless
the counter and LL/SC scans accept both vmlinux and the vDSO; the exact init ELF must pass the
LL/SC scan before packing. The checksum-pinned source/config cross-build is clean, and on
2026-07-20 the native `build-arm64-kernel.sh` recipe was run on the Altra N1 — its overlapping-patch
idempotency hardened on-box (`results/aa-5/live-20260720/` finding #2) — producing the guest `Image`
(sha256 `980b7982…`) and initramfs (`604733be…`) recorded in `MANIFEST.txt`, which then booted to
steady state. Native **publication** of the owned image as a standing pinned asset remains the
follow-up.

The prior timer-domain gap is now closed in the pre-silicon substrate. Upstream's virtual
clockevent programmed `CNTV_CVAL` against KVM's live architected counter; the owned kernel now
keeps that timer disabled and exports only work-clock deadlines. The hardened raw executable-ELF
scanner rejects linked `vmlinux`/vDSO publication if any CNTV/CNTP CVAL/TVAL program survives and
has a planted mapping-symbol negative control. The exact Linux 6.18.35 Image build passes that
gate with zero timer programs. On 2026-07-20 the pinned-N1 run happened: userspace steady state and
same-seed **console + register** identity PASS (register identity nokaslr-conditional); full-RAM
state identity remains open behind the kernel-CRNG entropy residual. That same live substrate hosted
AA-4 level-3's planted-exclusive proof (`results/aa-4/live-20260720/`).

**Disposition: AA-5 — (a) payload determinism and (b) the closure premise + scanner demonstrated on
real N1; (c) boot + clock mechanism proven on N1 (2026-07-20), full-RAM state identity open behind
the kernel-CRNG entropy residual**. The guest-registered exact-work page refresher was executed on
the pinned N1 (native box build + live bring-up), and AA-4's KVM execute-guard patch was booted and
passed its live proof — the items this disposition previously listed as blocking are now cleared.
What remains for full AA-5(c) state identity is the entropy-closure contract row (a deterministic
guest CRNG), a subsystem distinct from the clock. The AA-5 guest remains the natural workload for
AA-6's guest-side gates. The work clock, exact landing, force-exit, static counter closure, and —
new on 2026-07-20 — the AA-4 runtime execute guard are now all demonstrated on N1.

### AA-6 — the freezable-CPU contract + vGIC round-trip + mini determinism gate: SCOPED

Not started as a distinct stage; recorded here for the follow-on. Its pieces and their current
footing:
- **ID_AA64* freeze + trapped-sysreg table.** The register digest (`registers_and_vgic`) already
  reads the ID registers, and AA-0's `truth-table.json` pins the ID-register reality (MIDR,
  ID_AA64ISAR0/PFR0/MMFR*). The freeze is the data-driven table→model→enforce shape named in
  `docs/ARCH-BOUNDARY.md`; building + enforcing it against a running guest is AA-6 work.
- **vGIC round-trip.** The digest already includes the in-kernel vGIC injection state
  (`vgic_state()`, length-prefixed into `digest_state`), and AA-3's replay-identity compared it
  across 1.01M landings bit-identically — so the vGIC state is *captured and determinism-checked*
  today. The explicit save/restore round-trip gate is the remaining AA-6 piece.
- **Mini determinism gate (≥1000 reps).** The floor-checker's `rep-floor` supports an
  AA-6-normative `--min-reps 1000`; a ≥1000-rep bit-identity run on a bare-metal payload is
  runnable now on the box (the AA-3 apparatus already produces the digests), independent of the
  guest-Linux build.

**Disposition: AA-6 SCOPED — its determinism-digest substrate (register+vGIC digest, rep-floor)
exists and was exercised at 10⁶ scale by AA-3; the freezable-CPU contract enforcement and the
vGIC save/restore round-trip are the remaining build, best done alongside the AA-5(c) guest.**

### AA-6 — apparatus COMPLETE + portably validated; on-silicon run is the turnkey remaining step (task 135, hm-zx3z / hm-l1wy F8/F9/F10, 2026-07-21)

The full AA-6 apparatus is built on the merged #135 AA-4/AA-5(c) base and is **portably green**
(build + nextest + clippy native & aarch64-linux + fmt + the floor-checker's fixture suite). Write-up
+ turnkey box runbook: `docs/history/IMPLEMENTATION-task135.md`. Branch `task/arm-aa6-injection`.

- **The run-core injection hook (hm-zx3z) is non-additive by construction.** Both the bare-payload
  `run_sample_exact` path and the AA-5(c) `run_until_ready_work_clock` boot path gained a
  config-gated injection (`Option<InjectionConfig>` / `Option<LinuxInjection>`, drawn-but-applied-
  only-when-`Some`, mirroring `migration_probe`). Two committed portable **negative controls** prove
  the OFF path is byte-identical: the identical scripted landing / boot with injection `None` vs
  `Some` produces records byte-identical except the post-injection sentinel digest — these portable
  controls are the instrument for the **cross-build byte-non-additivity** claim (the stronger
  instrument). On silicon the N1 run confirms the two physical facts available there: the
  injection-OFF path replays **run-to-run bit-identically**, and an **ON** run's **pre-injection**
  `landed_digest`s equal the OFF run's — the hook adds nothing before the injection Moment. (Byte-
  identity is **not** asserted against the retained pre-wipe AA-3 `landed_digest` pins: those are not
  byte-reproducible on the rebuilt host — the aa3-recert pins landmine, toolchain-codegen + build-path
  drift — and the scales/seeds are disjoint; that check could not have run.)
- **(a) F9 done — id-freeze tri-state**: `install_id_freeze_field` now records every `ID_AA64*`
  register as `FrozenBelowHost` / `ReducibleButClamped` (the un-freezable-but-guest-visible stop
  condition, which must carry an `HCR_EL2.TID3` trap-emulation disposition) / `NoReducibleField`
  (does not gate). **F10 designed/box-buildable** — a real guest PMU access-fault via a
  single-step-to-sync-vector proof (reusing AA-2's classifier); the truth table records PMU denial
  via the existing PMUVer=0 proof today. This is the one open proof-completeness item.
- **(b) F8 done — vGIC round-trip** extended across all four injection-state groups (redistributor,
  distributor SPI, CPU interface `ICC_PMR`/`IGRPEN` via new 64-bit `CPU_SYSREGS` get/set, external
  input-line `LEVEL_INFO`), with per-group injection and the fresh-vGIC negative control.
- **(c) The mini gate** — `run … --inject-ppi 20 --reps 1000` (bare 8 classes) + `linux-boot
  --inject-ppi 22 --inject-at-work M --aa6-record` ×1000 (LinuxGuest) → `aa6-merge` → `floor-check
  --min-reps 1000`. Validated end-to-end portably: the merged run-set floor-checks **RESULT: PASS
  (20 checks)** — `aa6-matrix` (all 9 classes injected incl. LinuxGuest), `replay-identity`,
  `count-exactness`, `image-pins`, `rep-floor` all PASS.
- **RULING for Paul (llsc/wfi carve at AA-6).** The gate exercises "the LSE-only contract" (§AA-6),
  and AA-4 ruled LL/SC mechanically-excluded — so the floor-checker carves `llsc-atomics` (the
  **banned** counter-example, its ±2-branch divergence is AA-4(a)'s reason for the ban) and
  `wfi-idle` (AA-5's timer domain) from AA-6 replay-identity, **recording** the divergence in the
  verdict, while `lse-atomics` (the contract form) + every other class incl. the LinuxGuest must
  replay bit-identically. A `reject-aa6-contract-divergence` fixture proves the carve does not
  swallow a contract-class regression. This gate-semantics change is grounded in AA-4's binding
  ruling and the spec's own wording; **flagged for Paul's ratification at PR time.**

### AA-6 — executed on N1 aa3preempt 2026-07-21: **PROVISIONAL GO** (acceptance met; bounded items named, 4 gate-semantics changes pending Paul)

Ran overnight on the Altra (`results/aa-6/live-20260721/`, host `6.18.35-aa3preempt`, cores 60/61).
**Spec §AA-6 acceptance is MET:**
- **(a) truth table complete** — `id-freeze` PASS: `all_enforced=true`, `frozen_below_host=8`,
  `reducible_but_clamped=0`, `pmu_denied_without_feature=true`; F9 **tri-state** demonstrated
  (`ID_AA64DFR1_EL1 = no-reducible-field`), including PFR1 frozen below host on the patched surface.
- **(b) vGIC round-trip verdict recorded** — `vgic-roundtrip` PASS: `roundtrip_identical=true`,
  `negative_control_differs=true` across **all four groups** (redist/dist/cpu-interface/external-line,
  35 registers, F8), injected PPI 20 + SPI 32. Decision input: the in-kernel vGIC round-trips
  faithfully — no userspace-GIC model needed.
- **(c) ≥1000 same-seed mini-gate reps bit-identical** — `floor-check --min-reps 1000` on the merged
  8-class run-set (7 bare payloads + LinuxGuest, **8000 records**): **`RESULT: PASS (20 checks)`** —
  every attempted sample accounted (totality 8000), floors machine-checked and **reproducible from
  the retained records in-repo** (`records_sha256 005cf113…`). The injection **OFF-path physical
  negative control PASSED**: the OFF path replays run-to-run bit-identically and an ON run's
  pre-injection `landed_digest`s equal OFF's on N1 (the hook adds nothing before the injection
  Moment). The cross-build byte-non-additivity claim itself rests on the portable negative controls
  (the stronger instrument), not on reproducing the pre-wipe AA-3 payload pins.

**Four determinism-core / gate-semantics decisions the on-silicon run forced — each evidence-grounded,
flagged for Paul's ratification** (detailed in `docs/history/IMPLEMENTATION-task135.md`): (1) inject
as a **pending** interrupt (`GICR_ISPENDR0`), because `KVM_IRQ_LINE`'s line-level is not digested (a
line-only injection was vacuous — 27/28 ON==OFF); (2) **wfi-idle excluded** from the required matrix
(WFI stalls the work counter → 4/6 lost the PMI); (3) **llsc/wfi carved** from AA-6 replay-identity
(AA-4 ban ruling; a reject fixture guards the contract classes); (4) **LinuxGuest digest = console +
vGIC**, after root-causing the 1000/1000 register-digest FAIL to EXACTLY `x29`/`SP` stack-ASLR (the
AA-5(c) entropy residual, 4/260 regs, per-register-dump proof retained) — orthogonal to injection;
the corrected ≥1000-rep re-run then replayed bit-identically. **Pending-vs-taken framing:** the
compared digests exercise a **PENDING latched** interrupt (the `ISPENDR0` bit in the vGIC state);
**taken-interrupt** determinism — the guest entering the IRQ vector and running the handler
deterministically — is exercised separately by the clockevent/PPI lane (AA-5(c)'s boot PPI-20
assert/ACK accounting).

**Named bounded limitations (why PROVISIONAL, not full GO):** F10 (a **real** guest PMU access-fault)
is designed but not yet built — the truth table records PMU denial via the existing `PMUVer==0` proof;
the LinuxGuest determinism is certified on the **console + vGIC** basis (the full-RAM/register
stack-ASLR + CRNG residual remains AA-5(c)'s open entropy-closure item, orthogonal to injection); and
the four gate-semantics changes above await Paul's ratification. **STOP conditions did NOT trigger:**
no unfreezable state-reaching register (all reduced rows froze or are no-reducible), the vGIC
round-trips (no userspace model needed), and the injection OFF-path is byte-identical on silicon.

**Remaining for full GO:** the four gate-semantics changes are **RATIFIED** (see the next section);
the sole named provisional→full-GO condition is the masked-register-digest lane (bead **hm-3bwm**).
Non-blocking: build + run F10 (hm-l1wy); optionally close AA-5(c)'s entropy residual for full-RAM
LinuxGuest identity (an AA-5 item, not AA-6).

### AA-6 — RATIFIED (Paul, 2026-07-22, Fable second-opinion confirmed): 4 gate-semantics changes accepted → full AA-0..AA-6 ARM GO

Paul ratified all four on-N1 determinism-core / gate-semantics changes — his lean plus Fable's
**independent** second opinion (Fable re-derived the on-N1 evidence and independently verified the
config-gated **non-additive OFF-path** property). Verbatim: *"go? AA-6 do the thing."* The four are
accepted **as-is**; the injection-hook code is unchanged.

1. **PENDING-latch injection (`GICR_ISPENDR0`)** — RATIFIED. The line level is not digested, so a
   line-only injection was vacuous; the pending latch is observable + deterministic.
2. **`wfi-idle` excluded from the required injection matrix** — RATIFIED. WFI stalls `BR_RETIRED`;
   its determinism is AA-5's paravirt-clock domain. (Enforcement disposition follow-up: **hm-7yno**.)
3. **`llsc`/`wfi` carved from replay-identity (recorded, not failed)** — RATIFIED. AA-4's binding
   LL/SC ban; the `reject-aa6-contract-divergence` fixture guards the contract classes.
4. **LinuxGuest compared digest = `console + vGIC`** — RATIFIED, with a **named condition**: the
   ≥1000-rep **masked-register-digest** lane (bead **hm-3bwm**) — compare the full LinuxGuest register
   file minus exactly `{x29, SP}` (host-time `CNTPCT`/`TIMER_CNT` already excluded) — must confirm at
   gate scale that the console+vGIC narrowing is *exactly-and-only* the disclosed AA-5(c) stack-ASLR
   residual (4/260 regs), not masking an injection-path register divergence. **This lane is the named
   condition on the PROVISIONAL→full-GO upgrade.** The ratification stands; the lane confirms #4 at
   scale. (Free companion evidence — the injection-Moment register witness — lands via **hm-fiqo**.)

**Corrected on-silicon non-additivity claim (Fable's wording fix).** The committed N1 evidence proves
(i) the injection-**OFF** path replays **run-to-run bit-identically**, and (ii) an **ON** run's
**pre-injection** `landed_digest`s equal the OFF run's — the hook adds nothing before the injection
Moment, on silicon. The **cross-build byte-non-additivity** claim rests on the two **portable**
negative controls (`injection_off_path_*`), the stronger instrument; it does **not** rest on "OFF
reproduces the retained AA-3 `landed_digest`s bit-for-bit" — that comparison could not have run
(disjoint scales/seeds, and the pre-wipe AA-3 payload pins are not byte-reproducible on the rebuilt
host per the aa3-recert pins landmine).

**Pending-vs-taken framing.** The compared digests exercise a **PENDING latched** interrupt (the
`ISPENDR0` bit in the vGIC state); **taken-interrupt** determinism (the guest entering the IRQ vector
and running the handler deterministically) is exercised separately by the clockevent/PPI lane
(AA-5(c)'s boot PPI-20 assert/ACK accounting).

**Disposition: AA-6 GO** — full AA-0..AA-6 ARM re-cert complete pending the foreman's verify + merge
of PR #139. The provisional→full-GO upgrade is gated only on the **hm-3bwm** masked-register-digest
lane. Non-blocking follow-ups: F10 real-PMU-access-fault hardening (**hm-l1wy**), injection
attestation in `check_aa6_matrix` (**hm-oh3v**), `injected_landed_digest` emission (**hm-fiqo**),
WFI enforcement disposition (**hm-7yno**).
