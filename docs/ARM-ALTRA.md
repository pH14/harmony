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

V-time on ARM counts **`BR_RETIRED` (retired *taken* branches, raw event `0x21`)**. Two facts,
one favorable and one demanding:

- **Favorable:** N1 is the **best rr-characterized aarch64 lineage** — rr's production
  aarch64 support was developed and empirically trusted on Cortex-A76/Neoverse-N1-class cores
  (`docs/ARM-PORT.md` §evidence). Of every ARM part we could have received, this is the one
  with the strongest external evidence that precise taken-branch counting is physically
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
3. **Trap/emulate fallback (backstop only):** pages the scan flags get stage-2
   execute-denied; residual exclusive sequences fault to the host and are stepped/emulated
   atomically. Expensive by construction; its existence is what makes level 2's verdict
   enforceable rather than advisory.

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
(vs oracle); `BR_RETIRED` increments exactly on stepped taken branches and never otherwise;
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
  against the actual guest kernel image and a running guest; the stage-2 execute-deny +
  trap/emulate backstop (level 3) exercised at least once against a deliberately planted
  exclusive, to prove the backstop engages.

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

**Finding AA1-F1 (doc-vs-hardware, needs a ruling): N1's `BR_RETIRED` (0x21)
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

Recommended ruling (not yet ruled): keep 0x21 as the work clock with the
corrected semantics — the model's expected counts move from taken-branches to
branch-instructions-executed (knowable by construction from the same windows);
the accumulator machinery stays as the predicate witness. No event substitution
occurs: the hardware event is unchanged, our description of it corrects.

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

**AA-1(a) EL0 condition matrix: ALL GREEN** (evidence
`results/aa-1a/aa1a-{pinned-solo,co-tenant-other-core,co-tenant-same-core,memory-pressure}-001`,
smoke-fire-once per condition, quiet box, core 61): `el0-check` over the union —
**PASS 11/11 over 720 records**. 72 repeated cases bit-identical; ONE constant
offset per class across every condition and scale; 720/720 accumulators match.
Wall clock moved under load; counts did not. Still open for the (a)
sub-experiment: the kernel-mediated EL0 classes (syscall / signal / page-fault).
(b)/(c) are guest-mode and wait on the measurement host.

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
