# x86 nested-virtualization backend — feasibility spike

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

### N-2 — count exactness, overflow delivery, exact landing (the existential trio)

**Question:** Is the nested work clock exact?

This is the highest-value stage; do it before any campaign workloads. Use the det-corpus /
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

### N-3 — full-stack determinism gates nested + adversarial L0 + the portability gate

**Question:** Does the whole system hold its determinism claim as a guest, under a hostile
host?

Method: run the existing same-seed `state_hash` gates (det-corpus workload; postgres pair;
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

### N-4 — performance envelope + exit-budget memo

Characterization, after correctness — not part of the feasibility claim. Measure nested vs
bare metal on the same box: wall-clock ratios for boot-to-userspace, det-corpus workloads,
a postgres campaign smoke; per-exit-reason counts and costs from the existing exit-count
machinery; RDTSC exit rate per virtual second; snapshot capture/restore/branch and dirty-log
capture (the task-95 benches) nested. Deliverable: a short memo with ppm-style ratios and a
sizing recommendation for the paravirtual vtime clock page (the guest kernel is ours; a
work-derived kvmclock-shaped page would remove RDTSC exits from the hot path) — decision
input only, no implementation in this spike.

### N-5 — appliance packaging rehearsal (only after N-3 GO)

One documented command, fresh checkout, on the box: build the appliance image, boot it under
L0, run the same-seed nested gate, emit the evidence bundle. This is the "download image, boot
VM, deterministic machine" demonstration and the seed of any future distribution story.

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
