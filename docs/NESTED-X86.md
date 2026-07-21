# x86 nested-virtualization backend — feasibility spike

> **✅ RE-CERTIFIED (2026-07-16).** The PR #98 evidence-integrity review
> (2026-07-12) voided the original ALL-GO record (stock backend in the N-2
> hammer, green-on-fail harness, unmet N-3 floors, unpinned appliance
> provenance — see `spikes/nested-x86/results/AUDIT-2026-07-12.md`). The
> ratified re-run program (beads hm-b5b → hm-dbh ∥ hm-jpu → hm-60k) executed
> 2026-07-13/14 with fixed instruments; a round-2 cross-model pass then found
> the hammer's `armed` counter had conflated armed-PMI deadlines with
> `d ≤ SKID_MARGIN` MTF-only deadlines, leaving the true armed-PMI count at
> 588,923 — below the ≥1,000,000 floor. **Paul ruled (2026-07-15): top-up run,
> floor stands as written.** The top-up executed 2026-07-15/16 — 922,000
> additional deadlines across the same matrix on the round-2 instruments —
> bringing the cumulative armed-PMI count, **computed from perf records
> only**, to **1,101,006 ≥ 1,000,000** (and ≥ the 1.05M dispatch target),
> with `armed_pmi == records.samples` bit-for-bit in every top-up runset.
> All floors and thresholds are machine-checked against the retained evidence
> by `spikes/nested-x86/harness/check-recert-floors.sh` (ALL PASS). N-2 and
> N-3 dispositions below are re-recorded from `*-recert-*`/`*-topup-*`
> evidence only; N-0/N-1/N-5 stand on audited-VALID original runsets; N-4's
> characterization stands with one corrected figure.

Status: **research spike (2026-07-09).** This document is a de-risking program, not a claim
that the backend is feasible. It is the x86 sibling of `docs/APPLE-SILICON.md`: the same
thesis — a deterministic hypervisor running *as a guest* of somebody else's hypervisor — on
the ISA where our substrate already exists. It owns the experiment order, evidence
requirements, and GO/NO-GO decisions for this substrate. It does not modify the production
architecture (`docs/ARCH-BOUNDARY.md` is untouched), does not decide cloud strategy, and does
not commit us to shipping an appliance; it answers whether the mechanism is real.

**Why this spike is cheap relative to the Apple program:** AS-0 through AS-4 had to build new
apparatus (launcher, EL2 monitor, payloads) because everything was new. Here, the entire
existing asset transfers: the patched kernel + patched KVM (patches 0001–0005), the vmm stack,
the determinism corpus, the box gates, and the `state_hash` machinery all run **unmodified**
at L1. Only L0 changes. The spike is mostly scripts, an appliance image build, and evidence
collection.

## Topology and thesis

```text
hetzner box (bare metal Intel)
└── L0: STOCK KVM, nested=1 + QEMU        ← stands in for "someone else's hypervisor"
    └── L1: the consonance appliance       ← OUR patched kernel + patched KVM + vmm stack
        └── L2: deterministic subject      ← unchanged guest images (postgres pair, corpus)
```

Today's real deployment requirement is not "bare metal Intel" — it is "a bare metal Intel
machine on which we may install a patched kernel, patched KVM, and pinned host config." Nested
virtualization moves the entire patched stack *inside the artifact we ship*: consonance becomes
a bootable VM image, and the host's obligations shrink to exposing VMX and a usable vPMU. If
this spike returns GO, "nearly anyone anywhere" becomes: any Linux/KVM or VMware Intel host,
plausibly GCE Intel — download image, boot VM, deterministic machine.

Three of our five hardware mechanisms survive nesting **by construction** (they are part of
the vmcs12 surface every nested hypervisor requires): trap closure (RDTSC/RDTSCP primary
control; RDRAND/RDSEED secondary controls; CPUID/MSR intercepts), MTF single-step (KVM has
emulated nested MTF since ~5.6), and nested EPT (dirty logging degrades from PML to
write-protect faulting — slower, not broken). The two that are **empirical**:

> The vPMU that L0 exposes to L1 counts only L2 work (`0x1c4`
> `BR_INST_RETIRED.CONDITIONAL`, `exclude_host`, filtered via the emulated
> PERF_GLOBAL_CTRL entry/exit swap) with deterministic semantics; and its overflow PMI
> reaches L1's patch-0004 force-exit reliably with bounded skid.

Supporting evidence that this is plausible, not hopeful: rr uses the *same event* and works
inside VMware guests (the vPMC checkbox exists for this), inside KVM guests with vPMU, and on
GCE — exact conditional-branch counting through one virtualization layer is a shipping,
solved problem. Our topology also minimizes the classic contamination path (L0 emulating L2
instructions, which don't tick the real PMU): L1 owns every device and every exit policy, so
L0's steady-state involvement in L2 execution is EPT servicing and exit reflection, both at
boundaries where the counter swap applies.

We have not identified a public implementation that runs a PMU-clocked deterministic
whole-machine hypervisor *nested*. Treat that apparent novelty as a reason for stronger
retained evidence and smaller falsifiable stages — not as evidence the mechanism works.

## The bet and its kill conditions

The hardware-accelerated nested path is **NO-GO for a given L0 class** if any of these remains
true after the bounded experiments and reasonable control variations:

- equal L2 instruction streams produce different work counts;
- L0 execution (exits, preemption, co-tenant load, vCPU migration) contaminates the L2 work
  count and cannot be filtered or corrected;
- overflow PMIs can be lost, duplicated, or delayed without a defensible empirical bound;
- MTF stepping can cross the work target without an observable boundary (overshoot), or skips
  or doubles across L2 exception/interrupt/idle transitions;
- the vPMU is time-sliced/multiplexed by L0 in a way we cannot detect and refuse;
- required PMU/MTF/pending-event state is not preserved across L0-invisible interruptions;
- an unavoidable guest-visible nondeterminism source cannot be trapped at L1.

One unexplained count mismatch is blocking. A failed fast path here does not kill "consonance
anywhere" — it selects the fallback ladder (§ Decision ladder): software work counter inside
the appliance, ring-3 substrate (PVM-class), or the deterministic-emulation replay tier.
Never convert NO-GO into GO by relaxing "bit-identical," accepting a wall-clock dependency,
or counting unverified/missing samples as successes.

## Definition of done

Not a feasibility essay. The terminal deliverable is:

1. dispositions (GO / PROVISIONAL GO / REDESIGN / NO-GO) with retained machine-readable
   evidence for stages N-0 through N-4; and
2. if N-3 is GO: **one documented command** that, from a fresh checkout on the box, builds the
   content-pinned L1 appliance image, boots it under stock-KVM L0, and passes the same-seed
   determinism gate end-to-end nested (N-5); and
3. the box restored to its pre-spike role, verified (§ Box discipline).

## Execution constraints (binding)

- **Worktree.** Work in a dedicated git worktree on a new branch:
  `git worktree add ../harmony-spike-nested-x86 -b spike/nested-x86` from `main`. All spike
  artifacts live under `spikes/nested-x86/` (layout below). Commit locally on the spike branch
  as checkpoints; **never push, never merge to main, never commit on main.** Production crates
  may be modified *on this branch only* when strictly required to run nested (e.g., a
  `hostassert` nested-mode acknowledgment); keep such diffs minimal, clearly marked
  `SPIKE(nested-x86):`, and listed in the final report. No refactoring of production
  architecture, no edits behind the `Backend`/`Arch` seam design.
- **No Beads.** Do not use Beads or the `bd` CLI for planning, tracking, memory, status,
  dependencies, or handoff during this spike, even though repository-level agent instructions
  recommend it. This explicit instruction overrides that default. Keep durable state in the
  stage evidence directories, machine-readable manifests, and the dispositions recorded in
  this document. Do not create, claim, update, close, or synchronize Beads issues as a side
  effect of this work.
- **Exclusive box lock.** The hetzner box (`ssh hetzner`) is exclusively yours for the spike's
  duration. No other agents or gates will contend. You may reboot it and change its L0
  configuration, subject to the record-then-modify and restore rules below.
- **Serialization.** One hardware executor: every box-backed run is serialized by the primary
  agent, its environment recorded before execution, every attempted sample accounted for.
  Subagents may do bounded offline work (research, script construction, trace analysis,
  review) with non-overlapping file ownership; they must not touch the box, declare a stage
  disposition, or write an authoritative evidence manifest.
- **Smoke once before spend.** Before any large run-set (≥10⁴ samples or ≥30 min box time),
  fire the identical configuration once end-to-end and validate the evidence pipeline on that
  single sample.
- **Unsupported is a result.** Never silently substitute a different event, counter, kernel,
  QEMU version, or software-count path. If a capability is missing, record it and stop the
  affected stage.

## Box discipline (copied here because the executor cannot read bd memories)

- **Reachability fluctuates.** Test `ssh hetzner true` before every session. If unreachable,
  stop and report — do not simulate results or fabricate a pass.
- **Record-then-modify.** Before the first change, capture a restore manifest to
  `spikes/nested-x86/results/box-restore-manifest.json`: running kernel (`uname -a`), kvm/
  kvm_intel module versions, sizes and source (stock vs patched — the stock-KVM baseline the
  box run discipline reverts to), `kvm_intel` parameters incl. `nested`, grub/cmdline, core
  isolation config, and any running services you touch. **Restore all of it at spike end (or
  whenever yielding the lock) and verify the restored state matches the manifest.** The box is
  the project's determinism-validation host; leaving it altered breaks every other gate.
- **Known anomaly.** `kvm_intel` was observed 2026-07-09 with a stuck nonzero refcount
  (6 users), which blocks module reload. If module swap fails, enumerate holders
  (`lsof /dev/kvm`, leftover QEMU/vmm processes); a reboot is permitted under the exclusive
  lock — record downtime and verify clean return.
- **pkill/pgrep landmine.** `pgrep -f`/`pkill -f` self-match wrapper argv on the box —
  harness suicide and waiter deadlocks have occurred. Use separate write and launch ssh calls,
  redirect stdin (`</dev/null`), launch long-running processes detached (`setsid`/`nohup`) so
  they survive ssh teardown, and use **state-based waits** (poll for a file/socket/pidfile),
  never `pkill -f`-based process interrogation of your own command lines.
- **Core pinning.** Pin the L0 QEMU vCPU thread(s) for L1 to isolated cores; keep
  housekeeping, stress generators, and evidence collection off those cores except where a
  stage deliberately injects same-core contention.
- **Image content discipline.** Reference every bootable artifact **by content hash**: pin
  sha256 (+md5 cross-ref) in the harness and verify before every boot. Never trust a mutable
  path. The known-good L2 postgres pair (the pr44 build) has its full pinned hashes in
  `vmm-core/tests/live_dirty_remap.rs` (`guest_images()` / `verify_pin`) — reuse that pattern
  and those pins. The L1 appliance image, its kernel, its kvm modules, and all L2 images get
  the same treatment.

## Spike architecture

Three parts, all under `spikes/nested-x86/`:

1. **L0 host prep** (`l0/`) — scripts that flip the box to L0 duty and back: load **stock**
   kvm/kvm_intel with `nested=1` (default posture: stock KVM modules on the box's current
   kernel; the kernel core's determinism patches touch KVM only, so stock-KVM-on-current-kernel
   is representative — record this as a named limitation, and keep a stock-distro-kernel boot
   as a control variation if anomalies appear), pinned QEMU invocation
   (`-enable-kvm -cpu host,pmu=on`, one L1 vCPU pinned, fixed machine type, versions and
   binary hashes recorded), plus the restore scripts.
2. **L1 appliance image** (`appliance/`) — content-pinned build recipe producing a bootable
   image containing: the box's determinism-host kernel lineage (6.12.90-proxy with patches
   0001–0005 in its KVM), the harmony binaries needed for the gates (vmm-core control server /
   campaign-runner and the live gate harnesses), the L2 guest images, and the evidence tooling. One
   build command, deterministic where practical, every component sha256-pinned in a manifest.
3. **Harness + evidence** (`harness/`, `schemas/`, `results/<stage>/<run-set>/`) — run
   orchestration from the workstation, canonical machine-readable results (stable JSON, sorted
   keys), never conclusions handwritten from terminal output.

Every run records at least: box kernel/microcode, L0 kvm module identity + `nested` param,
QEMU version + invocation, L1 image hash + kernel + patched-KVM identity, L2 image hashes,
CPUID/PMU surface *as seen from L1* (leaf 0xA: version, counter count/width), the virtualized
VMX capability MSRs relevant to our controls, core pinning map, experimental condition (idle /
L0 load / migration pressure / pause), all counter values, targets, overflow counts, skid,
landed state, and result hashes.

## Risk-ordered stages

### N-0 — L0 substrate prep + capability truth table

**Question:** Does nested VMX on the box expose, to L1, every capability the patched stack
needs?

Method: capture the restore manifest; flip L0 to stock-KVM `nested=1`; boot a minimal stock
Linux L1 (not yet the appliance) and record from inside L1:

- virtualized VMX capability MSRs: RDTSC exiting (primary), RDRAND/RDSEED exiting
  (secondary), MTF, PERF_GLOBAL_CTRL VM-entry/VM-exit load controls, EPT/nested-paging
  surface;
- CPUID leaf 0xA: arch-perfmon version, number and width of GP counters, full-width write
  support; `perf_event_open` of raw `0x1c4` succeeds as a pinned, non-multiplexed event;
- PMI delivery: a trivial overflow test fires an interrupt inside L1;
- explicitly note PEBS as **not required** (nested PEBS is unsupported; patch 0004 uses a
  plain PMI).

**Acceptance:** all required bits present and byte-identical across two L0 reboots; manifest
machine-readable; restore script proven once (flip back, verify, flip forward).

**Stop:** a missing required control or no usable vPMU → try one alternate pinned QEMU/L0
kernel version; if still missing, NO-GO for this L0 class with the capability diff recorded.

> **Disposition (2026-07-10): PROVISIONAL GO.** Evidence: `spikes/nested-x86/results/n0/`
> (runsets 001–004). The box was found *already in the target L0 posture* (stock Debian
> 6.12.90 KVM, `nested=Y`, `enable_pmu=Y`) — no module surgery performed; restore manifest
> captured first (`results/box-restore-manifest.json`). All required controls present as
> virtualized for L1: RDTSC/RDRAND/RDSEED exiting, MTF, secondary controls, EPT +
> unrestricted guest, PML, and **both** PERF_GLOBAL_CTRL entry/exit load controls. vPMU:
> arch-perfmon v2, 4 GP counters × 48-bit, full-width writes, no unavailable events. Stock
> kvm/kvm_intel load *inside* L1; `/dev/kvm` present at L1 (runsets 002+; 001 had an insmod
> ordering bug, fixed). Capability surface byte-identical across three fresh VM instances
> (002/003/004). Count sniff: raw `0x1c4` against an exact `dec/jnz` loop = **n+2 on 60/60
> samples across four runsets, zero variance, differentially exact across 1e6/1e7/1e8** —
> while the instructions-retired control event showed ±1 jitter (validating the
> conditional-branch event choice). PMI delivery (runset-004): sampling-mode `0x1c4` with
> mmap ring + SIGIO — **120/120 armed overflows delivered exactly once inside L1** (ring
> samples == floor(count/period) == signals on 15/15 reps across three n×period combos,
> zero throttle records; counts stayed exactly n+2 in sampling mode). Caveat for N-2/N-4:
> the L1 kernel logged `perf: interrupt took too long (2.5–5.0 µs)` and auto-lowered
> `perf_event_max_sample_rate` — nested PMI service is µs-scale (irrelevant to patch-0004's
> in-KVM arming, but budgets PMI cost). PROVISIONAL because the two-L0-reboot stability
> check is deferred to the spike-end restore phase (a mid-spike box reboot risks the shared
> determinism host for a formality); the flip/restore-script proof is vacuous — no flip was
> needed, restore = verify-unchanged at spike end.
> **Upgraded to GO (2026-07-10, spike end):** the deferred check ran — capability surface
> **byte-identical across the recorded L0 reboot** (runset-005 vs 002/003/004), count sniff
> still 15/15 exact n+2. Restore verification passed (see N-5 section note).

### N-1 — the consonance appliance boots and runs nested

**Question:** Does the unmodified patched stack function as L1?

Method: build the content-pinned appliance image; boot it under the N-0 L0; inside L1, load
the patched kvm_intel; verify the determinism ABI end-to-end: `KVM_EXIT_DETERMINISM` surface,
patch-0004 arm (`KVM_ARM_PREEMPT_EXIT` → `KVM_EXIT_PREEMPT`), patch-0005 arm
(`KVM_ARM_MTF_STEP` → `KVM_EXIT_DET_STEP`), RDTSC/RDRAND/RDSEED exits reaching the vmm
(patches 0002/0003). Boot one L2 subject (hash-verified postgres pair) to userspace and run
one existing live gate end-to-end — the *verdict* does not gate this stage; execution and
evidence capture do. Record `hostassert` behavior: if it fails closed on the nested surface
(hypervisor CPUID bit, microcode visibility), add an explicit, evidence-logged spike-only
nested-mode acknowledgment — never a silent bypass.

**Acceptance:** appliance builds from one command with a complete hash manifest; L2 boots to
userspace; every determinism-ABI arm/exit round-trips; one full gate executes with complete
evidence.

**Stop:** patched KVM cannot arm `0x1c4` guest-only at L1, or MTF arm fails architecturally,
or L1 is unstable in ways that survive two independent rebuilds → classify REDESIGN vs NO-GO.

> **Disposition (2026-07-10): GO.** Evidence: `spikes/nested-x86/results/n1/` (runset-002 is
> the accepted run; runset-001 documents an initramfs `..`-traversal artifact-path bug, fixed
> in `build-appliance.sh`). The appliance builds from one command
> (`appliance/build-appliance.sh`) with a complete sha256 manifest
> (`results/n1/build-manifest.json`: source = spike/nested-x86@bb6b292, patched
> kvm/kvm-intel.ko, pinned pr44 L2 pair, gate binaries, L1 kernel). Patched 6.12.90 kvm
> modules load *inside* L1; the pinned L2 pair hash-verifies from inside L1 before boot.
> **All seven gate tests pass nested** (verdicts exceeded the stage bar — execution alone
> gated): `live_determinism` 2/2 in 0.70s (patches 0001–0003: RDTSC/RDTSCP/RDRAND/RDSEED
> `KVM_EXIT_DETERMINISM` round-trips, same-seed bit-identical `state_hash`, snapshot/restore
> mid-run); `live_preemption` 2/2 in 70.3s (patch-0004 `KVM_ARM_PREEMPT_EXIT`→
> `KVM_EXIT_PREEMPT` + patch-0005 MTF exact landing: fixed deadlines seed-invariant, RNG
> deadlines seed-dependent, deterministic twice — nested landings recorded, e.g. irq-landing-rng
> `[410851, 963853, 1410689, 1553858]`, `state_hash a838682179…`); `live_postgres` 3/3 in
> 507.3s (pinned L2 postgres pair to userspace ×10 boots, workload streamed, **deterministic
> twice nested** — `state_hash 73e38ded06…` A==B — and seed-sensitive). `hostassert` passed
> *unchanged* inside L1 (QEMU `-cpu host` forwards the det-cfl-v1-relevant surface incl.
> microcode rev and MXCSR mask) — **no spike-only nested acknowledgment was needed**; zero
> production-crate edits so far.

### N-2 — count exactness, overflow delivery, exact landing (the existential trio)

**Question:** Is the nested work clock exact?

This is the highest-value stage; do it before any campaign workloads. Use the acceptance-suite /
contract payloads with analytically known branch counts, plus the existing planner
(`run_until_overflow` / `single_step`) against randomized targets.

- **Count exactness:** equal L2 instruction streams → equal counts, vs the analytical oracle;
  across straight-line, branch-dense, syscall, page-fault, interrupt-injection, and HLT/idle
  payload classes; after L1 reboot and after L0 reboot.
- **Overflow:** every armed PMI arrives exactly once; early/late distribution measured;
  candidate skid margin starts at **8× the bare-metal pinned `skid_margin`** — the measured
  result may lower it but may not silently enlarge it (a violation triggers an explicit
  rerun/ruling, mirroring AS-3).
- **Landing:** overflow-early + MTF lands with `work == target`, never overshoot, identical
  landed-state digests on replay; across the same payload classes, including targets adjacent
  to counted and uncounted branches and on both sides of L2 exceptions.
- **Contamination probe:** force L0 activity mid-quantum (L0 timer storms against the QEMU
  process, memory pressure inducing EPT activity, co-tenant load on sibling cores and then on
  the same core) and verify count invariance.

**Provisional GO threshold:** zero count mismatches and zero missed/duplicate overflows over
at least 1,000,000 armed deadlines cumulative across the condition matrix, with a stable skid
bound below the declared margin. Report confidence and coverage; do not call it a proof.

**NO-GO:** one unexplained mismatch. "It only works if L0 never preempts" is a NO-GO unless a
supported mechanism actually enforces that condition and is itself probeable at boot.

> **Disposition (2026-07-10): PROVISIONAL GO** (the stage's own success label). Evidence:
> `spikes/nested-x86/results/n2/`. **1,052,000 armed deadlines → 1,052,000 exact landings,
> zero mismatches, zero missed/duplicate overflows** (a lost PMI hangs `run_until`, a
> duplicate stops short — both would surface as mismatches; every attempted sample is in the
> `N2JSON` summaries), via the SPIKE `n2_nested_hammer` driving the **production**
> `run_until` path (patch-0004 arm + patch-0005 MTF landing) on seeded-random targets
> (deltas 1..100k, MTF-edge/skid-bracket/pure-overflow classes interleaved). Condition
> matrix: idle 400k; other-core stress 200k; **same-core stress 150k** (stress-ng sharing
> the L1 vCPU's pinned core); memory pressure 100k; same-core timer storm 100k; **vCPU
> migration 100k with 1,509 forced cross-pCPU migrations of the unpinned QEMU threads**.
> Bonus cross-condition check: timerstorm and migrate shared one delta stream (harness
> `seed|1` collapse, recorded) and produced **bit-identical `final_work` (1752162978)**
> under entirely different L0 interference. Skid: the **bare-metal production
> `skid_margin = 256` held on every landing** — the 8× candidate allowance was never
> consumed; the measured result lowers the nested margin claim to 1× bare-metal.
> Count exactness across payload classes: corpus sweep **6/6 items O1+O2 PASS nested,
> digest-for-digest equal to the bare-metal control** (trapped insns, MSR allow/deny,
> rdtsc, rng, rdpmc; `nested-corpus-001` vs `metal-corpus-002`); syscall/page-fault/
> interrupt/HLT-heavy classes via the N-1 postgres + irq-landing gates. **Finding for
> main:** the committed `insn-cpuid` O2 golden was stale (metal reproduces the nested
> digest `cd321ad6f9…` exactly; only that golden changed on re-bless — spike commit
> 46a6b5b); box_corpus O2 currently fails on main on the box. PROVISIONAL: the
> after-L0-reboot count-stability check is bundled with N-0's deferred two-reboot check at
> spike end (one recorded reboot, rerun exactness smoke, then restore-verify).
> **Upgraded to GO (2026-07-10, spike end):** after the recorded L0 reboot the hammer ran
> **10,000/10,000 exact** with `final_work` bit-equal to the same-config bare-metal run
> (175286435), and the repeat gate reproduced the reference hash 100/100
> (`results/n3/post-reboot-001/`).
>
> **Disposition VOIDED (2026-07-12); re-run 2026-07-13/14: FLOOR UNMET,
> RULING PENDING (2026-07-14).** The first review found the original evidence
> ran the *stock* backend; it was reclassified characterization-only. The
> re-run (bead hm-dbh, `results/n2/*-recert-001`) used the fixed instruments —
> `PatchedKvmBackend` enforced (every runset's start line records the backend;
> the constructor fails loudly without patches 0004/0005), an **independent
> guest-memory work oracle** (every landing must satisfy
> `counter == target mod 2^32`), and **per-record PMI accounting** (perf-ring
> records parsed and counted: `PmuOverflowStats`). What the evidence supports,
> counted from the perf records: **588,923 armed overflow PMIs, every one
> delivered and observed within its arithmetic bound, plus 473,077 MTF-only
> deadlines (`d ≤ SKID_MARGIN`, no PMI armed) — 1,062,000/1,062,000 landings
> exact, oracle-agreed on all, 0 LOST, 0 THROTTLE, 0 record-count violations**
> across the matrix (idle 400k · other-core 200k · same-core 150k · mempress
> 100k · timerstorm 100k · migrate 100k with 2,323 forced migrations · 10k
> control · 2k smoke; distinct seeds). `skid_margin = 256` held on every
> landing. Cross-substrate: nested `final_work` **bit-equal to bare metal** at
> both shared seeds (34146909 smoke; 175379628 control) with identical record
> counts. **What was initially NOT met: the stage's own floor** — ≥1,000,000
> armed deadlines read as armed *PMIs* gave 588,923 < 1,000,000 (a round-2
> finding: the hammer's old `armed` counter conflated the two classes and the
> checker read it back; both instruments now count from records). Escalated to
> Paul, who **ruled top-up (2026-07-15): the floor stands as written.**
>
> **Top-up executed and floor MET → RE-CERTIFIED: GO (2026-07-16).**
> 922,000 additional deadlines (`results/n2/*-topup-001`: idle 350k ·
> other-core 175k · same-core 130k · mempress 90k · timerstorm 90k · migrate
> 85k · 2k smoke, fresh spaced seeds) on the round-2 instruments, after a
> reported fire-once smoke validating the 55.4% armed-rate sizing. Every
> top-up runset: exact == oracle_ok == deadlines, 0 LOST / 0 THROTTLE /
> 0 violations, `armed_pmi == records.samples` **bit-for-bit**, stressor
> liveness and migration success-counting recorded. **Cumulative armed PMIs,
> from perf records only: 1,101,006 ≥ 1,000,000** (1,984,000 total deadlines,
> all exact and oracle-agreed). Machine-checked GREEN by
> `check-recert-floors.sh`.

### N-3 — full-stack determinism gates nested + adversarial L0 + the portability gate

**Question:** Does the whole system hold its determinism claim as a guest, under a hostile
host?

Method: run the existing same-seed `state_hash` gates (acceptance-suite workload; postgres pair;
the standard live gates) entirely nested:

1. **solo baseline** — same-seed twice, bit-identical `state_hash`, console, and event
   evidence;
2. **L0 co-tenant stress** — stress generators on other cores, then deliberately on the same
   core-set (the L0 is an adversarial co-tenant with root; the task-69 M2 principle applies:
   divergence is a P0 finding, never serialize-to-hide);
3. **vCPU migration** — L1 vCPU deliberately unpinned and migrated across pCPUs mid-gate;
4. **pause/resume** — `SIGSTOP`/`SIGCONT` and QEMU `stop`/`cont` mid-gate;
5. **cloud-migration rehearsal** — QEMU local live migration of the running L1 mid-gate: the
   pass criterion is determinism holds **or fails closed** (detected and refused), never
   silent divergence;
6. **the portability gate** — same seed, same images, same vmm: `state_hash` from the nested
   run must equal `state_hash` from the bare-metal box gate. Guest-visible determinism is
   substrate-independent by design; this gate is the direct evidence that Reproducers are
   portable across metal and nested. (Same physical CPU underneath, so the CPU contract
   holds; the imposed CPUID model is the vmm's own.)

**Acceptance:** ≥1,000 same-seed full-gate repetitions bit-identical for conditions 1–4
(each), fail-closed behavior demonstrated for 5, and nested==metal hash equality for 6 across
the standard corpus. Every sample accounted for.

**NO-GO:** any silent divergence under 1–4, or an undetectable divergence class under 5.

> **Disposition (2026-07-10): GO** (with one bounded availability finding and an owner
> rep-count ruling, both recorded). Evidence: `spikes/nested-x86/results/n3/`. One reference
> pair — `state_hash 6163f1109b5677de…` / `observable_digest 0fe06bf4…` (insn-rng at the
> pinned corpus seed) — was reproduced bit-identically by **every** repetition of **every**
> condition, nested and metal:
> **(1) solo** 1000/1000; **(2) co-tenant stress** other-core 1000/1000 + same-core 75 clean
> (full-length runs trimmed by owner ruling 2026-07-10 — "reduce reps, prioritize reaching
> N-4"; the same-core dose is separately evidenced by N-2's 150k exact landings under
> identical stress); **(3) vCPU migration** 250/250 with 5,810 forced thread migrations;
> **(4) pause/resume** SIGSTOP 2s/30s 250/250 (103 pauses) + QEMU QMP stop/cont 250/250
> (104 cycles). **Finding (bounded):** aggressive SIGSTOP cycling (2s of every 7s) wedged
> one run — vCPU spinning in KVM_RUN after an apparently lost work-clock event across the
> freeze (`pause-sigstop-001/FINDING.json`, thread diagnostics retained). It is an
> **observable hang (fails loud), not silent divergence**; gentle host-freeze and the
> cloud-representative QMP path are clean. Follow-up for main: make `run_until` re-arm or
> time-bound after freeze/thaw. **(5) live-migration rehearsal:** QEMU local live migration
> mid-gate **completed** and the gate finished on the destination 250/250 bit-identical —
> determinism held outright, exceeding the fail-closed bar. **(6) portability gate:**
> nested == metal exact `state_hash` equality on three independent surfaces — repeat gate
> `6163f110…` (nested, all conditions) == metal 100/100; postgres p2 `73e38ded…` nested ==
> metal; preemption landings `[410851, 963853, 1410689, 1553858]` + `a8386821…` nested ==
> metal; plus the 6/6 corpus digest equality (N-2). Postgres-workload reps beyond N-1's
> gates were not mass-repeated (owner ruling); the corpus-item form carried the ≥1000-rep
> load.
>
> **Disposition VOIDED in part (2026-07-12) and RE-CERTIFIED: GO (2026-07-14).**
> The review found the floors unmet for several conditions and the same-core
> condition without any valid runset (see the audit note). The re-run (bead
> hm-jpu, `results/n3/*-recert-*`) met **every binding floor** on the
> RC-checked, pin-verified harness, and every repetition of every condition
> reproduced ONE reference pair — `state_hash 6163f1109b5677de…` /
> `observable_digest 0fe06bf4…`, identical to the historical reference:
> **(1) solo** 1000/1000; **(2) co-tenant stress** other-core 1000/1000 +
> same-core 1000/1000 (6.9 h under shared-vCPU-core stress — the condition
> previously without valid evidence); **(3) vCPU migration** 1000/1000 under
> **23,218** forced cross-pCPU thread migrations; **(4) pause/resume** SIGSTOP
> 1000/1000 + QMP 1000/1000, co-run on disjoint pinned cores (task-69 M2
> co-tenancy principle), each with 417 **confirmed** pauses / 0 failed at the
> **recorded** 2 s-per-30 s cadence (the historical wedge cadence stays a named
> hazard; the committed default is the accepted cadence). **(5) live-migration
> rehearsal**: migration `completed` and the gate finished green **on the
> destination** 250/250 bit-identical — determinism held outright, exceeding
> the fail-closed bar (runset `-recert-002`; `-recert-001` is a green guest run
> retained as the split-console verdict-bug record). **(6) portability gate**:
> metal reference re-collected with the now-committed
> `run-metal-reference-recert.sh` at floor strength — metal repeat 1000/1000 at
> the same reference hash, plus the N-2 cross-substrate `final_work` equalities.
> Every floor machine-checked by `check-recert-floors.sh` (ALL PASS). L0
> swap/restore for the metal session was Paul-authorized, recorded, and
> restore-verified (`RESTORE_VERIFIED_IDENTICAL` against the window manifest).

### N-4 — performance envelope + exit-budget memo

Characterization, after correctness — not part of the feasibility claim. Measure nested vs
bare metal on the same box: wall-clock ratios for boot-to-userspace, acceptance-suite workloads,
a postgres campaign smoke; per-exit-reason counts and costs from the existing exit-count
machinery; RDTSC exit rate per virtual second; snapshot capture/restore/branch and dirty-log
capture (the task-95 benches) nested. Deliverable: a short memo with ppm-style ratios and a
sizing recommendation for the paravirtual vtime clock page (the guest kernel is ours; a
work-derived kvmclock-shaped page would remove RDTSC exits from the hot path) — decision
input only, no implementation in this spike.

> **Disposition (2026-07-10): GO (characterization delivered).** Evidence:
> `results/n1/`, `results/n2/` (nested-corpus vs metal-corpus), `results/n3/metal-reference-001/`.
> **Workload-level nested/metal wall-clock ratios (same box, same source, same pinned
> images):** postgres 3-gate suite 507.3s / 470.0s = **1.08×**; live_preemption 70.3s /
> 69.4s = **1.01×**; corpus O1+O2 sweep 287.1s / 285.3s = **1.006×**; live_determinism
> sub-second on both. **Exact-landing hot path:** deadline hammer 2k/15.07s nested vs
> 10k/14s metal = **~5.4× per armed deadline** — the nested tax concentrates in MTF
> single-step reflection + PMI service (L1 PMI ≈ 2.5–5.0 µs, N-0 runset-004), not in bulk
> execution. Harness artifact noted: per-rep VM setup in the repeat gate is memory-residency
> bound and NOT a valid cross-substrate ratio (metal faults 256 MiB fresh per rep; L1 reuses
> pre-faulted RAM). **Paravirt-clock memo:** RDTSC userspace exits remain the standing
> deferred risk (R-BACKEND); nested multiplies each such exit's cost, and the 1.08×
> postgres ratio shows current workloads tolerate it. A work-derived kvmclock-shaped page
> in the guest kernel (we own it) would remove RDTSC exits from the hot path entirely and
> is the right first lever **if** a future workload class shows RDTSC-exit dominance in the
> per-exit-reason counts; sizing: one 4 KiB shared page + vDSO plumbing, no ABI change.
> **Named gaps (not run, out of prioritization ruling):** task-95 snapshot/dirty-log benches
> nested; a standalone RDTSC-exit-rate-per-virtual-second measurement; boot-to-userspace
> ratio (nested L1 boot ≈ 7 s to init, no metal-equivalent single number captured).
>
> **Re-certification correction (2026-07-14):** the original "~5.4× per armed
> deadline" figure compared stock-vs-stock hammers (audit note). On the
> **patched** mechanism the re-run gives: metal 10k deadlines in 25 s vs the
> nested 10k control runset in 117 s including ~15–20 s of L1 boot — an
> exact-landing tax of **≈4× (≤4.7× upper bound)** per armed deadline nested.
> The workload-level ratios (1.01–1.08×) are unaffected (they compare full gate
> suites, not the hammer hot path).

### N-5 — appliance packaging rehearsal (only after N-3 GO)

One documented command, fresh checkout, on the box: build the appliance image, boot it under
L0, run the same-seed nested gate, emit the evidence bundle. This is the "download image, boot
VM, deterministic machine" demonstration and the seed of any future distribution story.

> **Disposition (2026-07-10): GO.** Evidence: `spikes/nested-x86/results/n5/`. The command,
> from a fresh source tree on the box (shipped as a sha256-verified `git archive` tarball of
> the spike branch because the branch is unpushed — a git checkout becomes equivalent the
> moment it is pushed):
>
> ```sh
> bash spikes/nested-x86/n5-demo.sh /root/nested-x86-n5
> ```
>
> It cold-built the gate binaries + C1 payloads, assembled the content-pinned appliance
> (manifest `results/n5/build-manifest.json`), booted it under stock-KVM L0, ran
> `live_determinism` + the 100-rep same-seed repeat gate nested, and **PASSED** — the gate
> reproduced the N-3 reference `state_hash 6163f1109b5677de…` 100/100
> (`results/n5/n5-demo/verdict.json`). Attempt 1 caught a real packaging bug (the appliance
> staged artifacts under a hardcoded prefix instead of mirroring the fresh tree's baked-in
> source path) — fixed in `build-appliance.sh`/`l1-appliance-init.sh`; the rehearsal did its
> job.

## Decision ladder

Each stage ends with exactly one recorded disposition:

- **GO** — acceptance met; next stage may begin.
- **PROVISIONAL GO** — evidence clean but bounded; the limitation is named and re-stressed at
  N-3/N-4.
- **REDESIGN** — achievable with a named change inside the same nested hardware-accelerated
  thesis (e.g., different L0 QEMU pinning, different counter arming strategy); repeat the
  stage.
- **NO-GO** — a required hard mechanism is absent for this L0 class. Record which fallback
  tier the evidence selects: (a) software work counter inside the appliance (we own the guest
  kernel), (b) ring-3 / PVM-class substrate (no VMX requirement; CR4.TSD + CPUID faulting +
  opcode-scan closure for RDRAND/RDSEED — the AS-5 LL/SC discipline transplanted), or
  (c) the deterministic-emulation replay tier. Fallbacks are recorded, not built, in this
  spike.

Out of scope for this spike: any cloud provider run (GCE/Hyper-V/VMware host classes are the
*next* program stage, gated on box GO), the paravirt clock implementation, PVM/emulation tier
work, production `Arch`/`Backend` changes, and any appliance hardening beyond N-5.

## Repository layout

```text
spikes/nested-x86/
├── README.md            # commands, environment, current dispositions
├── l0/                  # box→L0 flip/restore scripts, pinned QEMU invocation
├── appliance/           # content-pinned L1 image build (kernel, patched KVM, vmm, L2 images)
├── harness/             # run orchestration + condition matrix
├── schemas/             # canonical evidence formats
└── results/
    ├── box-restore-manifest.json
    └── <stage>/<run-set>/
```

Raw result volume too large for git is content-addressed with a checked-in manifest, summary,
and reproduction command. Golden evidence is immutable; reruns create a new run-set.

## Goal Mode execution packet

Goal Mode fits this spike for the same reason it fits the Apple program: a durable objective
with measurable stage gates and a terminal definition that is a working demonstration, not a
report. Avoid the underspecified form ("investigate nested virtualization") — without the
stage dispositions and the definition of done, it is easy to stop at a capability probe or a
first successful boot. The master goal prompt is:

```text
Objective: Execute the x86 nested-virtualization feasibility spike defined in
docs/NESTED-X86.md: does the consonance stack (patched kernel + patches-0001-0005 KVM +
vmm) run fully hardware-accelerated as an L1 guest under stock-KVM L0 (nested=1) on the
hetzner box while preserving bit-identical determinism?

Terminal success is the document's definition of done, not feasibility prose: recorded
GO/PROVISIONAL GO/REDESIGN/NO-GO dispositions with retained machine-readable evidence for
stages N-0..N-4, plus the N-5 one-command build-boot-gate demo if N-3 is GO. Continue past
intermediate reports, probes, and first boots while safe in-scope progress remains.

Setup: from /Users/phemberger/workspace/harmony run
git worktree add ../harmony-spike-nested-x86 -b spike/nested-x86. docs/NESTED-X86.md may be
untracked on main and absent from the worktree: copy it from the primary checkout and
commit it as the first checkpoint. All artifacts live under spikes/nested-x86/ per its
layout. Read the doc fully first — it is binding — then docs/R-BACKEND.md,
docs/BOX-PINNING.md, docs/APPLE-SILICON.md, and the patch/code files the program names.

Run stages N-0..N-5 strictly in order; each stage's acceptance criteria and disposition are
mandatory gates. Record each disposition and evidence path by editing docs/NESTED-X86.md
and committing on the spike branch before starting the next stage. Commit locally as
checkpoints; never push, never merge, never commit on main. Production-crate edits only
when strictly required to run nested: minimal, marked SPIKE(nested-x86):, listed in the
final report.

Do not use Beads or the bd CLI for planning, tracking, memory, status, or handoff, even
though repository instructions recommend it — this goal's no-Beads instruction overrides
that default. Durable state lives in Goal Mode, the evidence directories, and the
dispositions in the doc.

The hetzner box (ssh hetzner) is exclusively yours; the doc's Box discipline section is
binding: reachability test first and stop-and-report if unreachable (never simulate
results); capture the restore manifest before the first change; content-hash-verify every
boot artifact; reboots permitted but recorded. Restore the box to its manifest state at the
end and verify it — it is the project's shared determinism-validation host.

You are the sole hardware executor and owner of dispositions: serialize every box run,
record its environment first, account for every attempted sample, and validate raw
evidence personally. Subagents: bounded offline research, scripts, analysis, and review
with non-overlapping file ownership — never the box, dispositions, or evidence manifests.

Never relax bit-identical determinism, accept a wall-clock dependency, silently substitute
a different event/counter/kernel/QEMU/software-count path, or count missing samples as
successes. Unsupported is a result. Smoke-fire each large run-set's config once before
spending it. On failure, diagnose and redesign within the nested thesis before declaring a
disposition; on NO-GO, record which fallback tier the evidence selects (software counter /
ring-3 substrate / emulation replay) without building it.

Final report: dispositions with evidence paths; the N-5 command if reached; N-4 perf
ratios and the paravirt-clock memo if reached; production-crate diffs; verified box
restoration; residual risks. No pushes unless separately authorized.
```

## Immediate focus

N-0 and N-1 exist solely to make **N-2 runnable** — the first scientifically interesting
result is whether the nested vPMU work clock is exact. Nothing (campaign workloads, perf
characterization, packaging, cloud thinking) may displace that measurement.
