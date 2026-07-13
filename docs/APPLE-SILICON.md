# Apple-silicon hardware backend — research program

Status: **ROUTE DEAD — retained as archaeology** (Paul, 2026-07-12). The Apple Silicon
route failed for Apple hardware reasons; the primary ARM direction is now **Linux/KVM on an
Ampere Altra** (Neoverse N1 — see `docs/ARM-ALTRA.md`, and `docs/ARM-PORT.md` for the mechanism analysis, which stands). This
document's experiment designs, PMU/debug-step analysis, and evidence discipline remain
reference material; its program is not scheduled and its Goal Mode packets must not be run.
Historical status line (2026-07-09): primary post-x86 hardware research direction. This
document was a de-risking program, not a claim that the backend is feasible. It narrows the ARM work in
`docs/ARM-PORT.md` to the novel path we care about: macOS on Apple silicon,
Hypervisor.framework as L0, a Harmony monitor at virtual EL2, and an ARM64 Linux payload at
EL1/EL0. `docs/ARCH-BOUNDARY.md` is a reference for possible later consolidation, not a design
constraint on the parallel implementation. This document owns the experiment order, evidence
requirements, and GO/NO-GO decisions for this substrate.

## Program objective and definition of done

The intended unattended outcome is not a feasibility report. It is a **working standalone
Apple-silicon deterministic hypervisor** that can be launched on a supported Mac without
depending on the existing consonance implementation.

“Working” means one documented command can build and run a content-pinned ARM64 Linux subject
under Hypervisor.framework → Harmony EL2 monitor, and the system:

- derives all guest-visible time from deterministic work, never macOS wall time;
- supplies seeded entropy and deterministic external inputs only;
- injects asynchronous events at exact reproducible work boundaries;
- boots Linux to userspace and runs a nontrivial syscall/timer/page-fault/atomic workload;
- produces bit-identical console, event ledger, CPU/device/monitor state, and memory hashes for
  repeated same-seed runs;
- saves, restores, and branches complete state without losing or duplicating an event;
- fails closed on an unclosed counter, RNG, identity, PMU, debug, LL/SC, or device surface;
- retains exact build commands, environment manifests, content hashes, and raw evidence.

Code reuse is explicitly secondary. Existing consonance code and contracts are references for
semantics, failure modes, test design, and rigor. The Apple implementation may use a separate
workspace, types, run loop, state format, monitor ABI, device models, and build system whenever
that shortens the path to evidence and a working machine. Consolidation happens only after the
standalone backend is feasible and useful.

The target is deliberately **not**:

- an Intel-Mac backend;
- a native macOS port of rr;
- `Virtualization.framework` (too high-level to own the trap boundary);
- an x86 guest under Rosetta (Rosetta translates user processes, not an x86 kernel);
- QEMU TCG as the main engineering program. TCG remains a credible slow oracle/fallback, but
  its feasibility is not the open research question.

## Thesis

The only credible fast design is a nested machine:

```text
macOS / Apple silicon
└── Hypervisor.framework (L0)
    └── Harmony monitor (virtual EL2, L1)
        └── deterministic ARM64 Linux subject (EL1/EL0, L2)
```

macOS 15 added Hypervisor.framework support for exposing EL2 to a guest on supported Apple
silicon. Apple describes the facility as enabling a hypervisor inside a VM, including virtual
GIC hypervisor-control state. The installed SDK also documents a consequential PMU behavior:
with virtual EL2 enabled and a valid `ID_AA64DFR0_EL1.PMUVer`, Hypervisor.framework emulates
PMU register access for the nested hypervisor. These facts make the design plausible. They do
**not** establish that the emulated PMU has the deterministic event semantics, reliable
overflow, bounded skid, privilege filtering, or complete save/restore surface Harmony needs.
Those are the central experiments below.

We have not identified a public implementation that uses Apple's nested virtual PMU as the
work clock for a deterministic whole-machine hypervisor, combines its overflow with exact L2
debug-step landing, and then snapshots/branches ARM64 Linux. Treat that apparent novelty as a
reason for stronger retained evidence and smaller falsifiable stages—not as evidence that the
mechanism works.

Primary Apple references (checked 2026-07-09):

- [Hypervisor updates — nested virtualization](https://developer.apple.com/documentation/updates/hypervisor)
- [`hv_vm_config_set_el2_enabled`](https://developer.apple.com/documentation/hypervisor/hv_vm_config_set_el2_enabled%28_%3A_%3A%29)
- [`hv_vcpu_set_trap_debug_exceptions`](https://developer.apple.com/documentation/hypervisor/hv_vcpu_set_trap_debug_exceptions%28_%3A_%3A%29)
- [`hv_vm_protect`](https://developer.apple.com/documentation/hypervisor/hv_vm_protect%28_%3A_%3A_%3A%29)
- local SDK headers: `Hypervisor.framework/Headers/{hv_vm_config.h,hv_vcpu.h,hv_vcpu_types.h}`

rr is evidence about the silicon, not an implementation dependency. Upstream rr requires
Linux, but supports Apple M-series cores under Linux and uses Apple PMU event `0x90` for taken
branches on the M1/M2 families. That refutes a blanket “Apple PMUs cannot support replay”
claim. rr's success still does not answer whether Hypervisor.framework's *nested virtual PMU*
preserves the required behavior.

## The bet and its kill conditions

Harmony needs two properties at the machine boundary:

1. every guest-visible nondeterministic result is trapped or made unreachable and replaced by
   a deterministic value; and
2. every asynchronous event is injected at a reproducible work boundary.

For this backend, property 2 reduces to the following hardware bet:

> A counter visible to the Harmony EL2 monitor counts only L2 work with deterministic
> semantics; its overflow reaches the monitor reliably with bounded skid; and ARM debug
> stepping can land at the exact target through exceptions and privilege transitions.

The hardware-accelerated path is **NO-GO** if any of these remains true after the bounded
experiments and reasonable control variations:

- no suitable deterministic branch/work event is exposed to virtual EL2;
- equal L2 instruction streams produce different work counts;
- monitor/L0 execution contaminates the L2 work count and cannot be filtered or corrected;
- overflow can be lost, duplicated, or delayed without a defensible empirical bound;
- exact stepping cannot cross L2 exception, syscall, page-fault, or idle transitions without
  overshoot or hidden state;
- an unavoidable guest-visible counter, entropy source, identity source, or asynchronous
  device remains tied to macOS state;
- required PMU/debug/nested-translation state cannot be completely captured and restored;
- LL/SC execution cannot be excluded or determinized under the chosen cooperative-guest
  contract.

A failed fast-path experiment does not prove ARM or macOS execution impossible. It selects a
single-step-every-instruction or TCG design. Those are fallback decisions, not reasons to
weaken the determinism bar.

## Why direct EL1 Hypervisor.framework is insufficient

A plain ARM64 Linux vCPU directly under Hypervisor.framework is useful for bring-up, but not
for a determinism claim:

- the public vCPU exit vocabulary is narrow (`CANCELED`, `EXCEPTION`,
  `VTIMER_ACTIVATED`, `UNKNOWN`);
- the SDK defines the guest virtual counter in terms of `mach_absolute_time` minus an offset;
  the offset changes the epoch, not the wall-clock slope;
- `hv_vcpus_exit` is an immediate host-requested exit, not a branch-count deadline;
- the public API has no `perf_event_open` analogue that arms a guest-only branch count and
  returns to userspace on overflow;
- debug exceptions can be trapped, but there is no documented “run N deterministic guest
  branches” operation;
- there is memory permission control but no public dirty-page-log API in the current SDK.

Virtual EL2 changes the ownership boundary: counter/time traps, nested stage-2 permissions,
debug control, PMU programming, and L2 interrupt scheduling can live in our monitor instead of
depending on a missing L0 userspace API.

## Architecture of the standalone research implementation

Keep the implementation isolated from consonance and narrower than a general-purpose VMM. It
may begin as spike-quality apparatus, but it is allowed to grow directly into the complete
standalone result instead of being rewritten behind the existing `Backend` seam. It has three
parts:

1. **macOS launcher** — creates one Hypervisor.framework VM and vCPU, enables EL2, maps a
   fixed pre-populated RAM image, loads the monitor and payload, runs the outer vCPU, and
   extracts a canonical result block. No `Virtualization.framework`, real devices, network,
   disk, wall-clock deadlines, or asynchronous dispatch queues.
2. **EL2 monitor** — establishes L2 stage-2 translation, exception vectors, virtual CPU
   identity, PMU/debug/timer trap controls, a deterministic event ledger, and a shared result
   page. It is the experimental analogue of patched KVM patches 0001–0005, not an
   implementation of consonance's current `Backend` trait.
3. **L2 payloads** — tiny assembly kernels first, then a minimal ARM64 Linux image. Synthetic
   payloads isolate one mechanism at a time; Linux is admitted only after the PMU and exact
   landing have evidence.

The spike must produce machine-readable evidence (canonical binary or stable JSON with sorted
keys), not conclusions handwritten from terminal output. Every run records at least:

- Mac model, chip family, macOS build, Hypervisor.framework availability, and EL2 support;
- exposed `ID_AA64*` values and PMU version;
- requested and observed PMU events/configuration;
- initial/final counter values, target, overflow count, skid, landed PC/PSTATE, and result hash;
- outer exits and L1/L2 exception classes;
- payload and monitor content hashes;
- experimental condition (idle host, load, migration pressure, sleep/wake if applicable).

The experiment must never silently substitute a different event, counter, core type, or
software-count path. Unsupported is a result.

## Risk-ordered plan

The order is intentional: settle the existential hardware questions before Linux, snapshot
performance, or architecture refactoring.

### AS-0 — Host capability and API truth table

**Question:** On which available Apple-silicon machines does Hypervisor.framework expose the
minimum nested substrate?

Build a signed, entitlement-correct command-line probe that does not boot Linux. It records:

- `hv_vm_config_get_el2_supported` and an actual EL2-enabled VM creation attempt;
- IPA width/granule and memory-map/protect behavior;
- vCPU feature registers and settable `ID_AA64*` fields;
- readable/writable EL2 sysregs needed for HCR/MDCR/timer/stage-2 control;
- virtual GIC availability and state APIs;
- debug-exception and debug-register trap controls;
- PMU behavior described by the SDK for both valid and zero `PMUVer`;
- the exact state surface exposed by get/set APIs.

**Acceptance:** at least one target Mac successfully creates an EL2-enabled vCPU, exposes the
required EL2 translation/timer/debug registers, and returns a stable capability manifest on
repeated runs. Any missing API is classified as existential, deferrable, or monitor-owned.

**Stop condition:** if virtual EL2 is unsupported on the target fleet or the monitor cannot
own nested translation and exception routing, stop the fast path.

### AS-1 — Minimal EL2 monitor runs an L2 payload

**Question:** Can we execute controlled code at EL1/EL0 beneath our own EL2 and observe every
transition needed for later experiments?

Implement the smallest monitor that:

- starts at virtual EL2 with deterministic register and memory initialization;
- installs EL2 vectors and L2 stage-2 translation over a fixed RAM map;
- enters a bare L2 payload at EL1, then EL0;
- handles an L2 `HVC`/deliberate doorbell, stage-2 fault, synchronous exception, IRQ, and WFI;
- reports the full transition ledger through a shared page and deliberate outer exit;
- runs without a virtual device model or host wall-clock timer.

**Acceptance:** 10,000 fresh-process runs yield byte-identical ledgers and final architectural
state for fixed payloads. EL2 and L2 execution are distinguishable in the ledger. A malformed
payload fails closed rather than hanging the launcher.

**Stop condition:** if L2 exceptions or stage-2 faults are irretrievably handled inside L0,
or if the monitor cannot force a bounded return to the launcher, stop.

### AS-2 — Nested PMU event discovery and count determinism

**Question:** Is there a deterministic L2 work counter?

This is the highest-value goal. Do it before Linux. Candidate events include Apple's M-series
taken-branch event `0x90` (used by rr on Linux) and architected `BR_RETIRED` `0x21`; accept no
event by name alone. Construct assembly payloads with analytically known control flow:

- straight-line code;
- fixed taken/not-taken branch ratios;
- direct, indirect, call/return, and exception transitions;
- EL0↔EL1 transitions;
- stage-2 faults handled by the monitor;
- WFI entry/resume;
- LSE atomics and, separately, LL/SC probes.

For each candidate, establish:

- event availability and width;
- whether L2 EL0 and EL1 can be included while EL2 monitor work is excluded;
- whether L0 exits, host preemption, and host load affect the count;
- whether counter state survives outer exits without unexplained deltas;
- whether migration pressure between efficiency and performance cores changes semantics,
  using any public scheduling metadata available without treating it as proof of placement;
- whether the same dynamic instruction stream produces the same delta after process restart,
  sleep/wake, and on another same-model Mac.

**Provisional GO threshold:** zero count mismatches over at least 1,000,000 independent
payload runs spanning idle and adversarial host-load conditions, with exact agreement against
the analytical oracle for the selected event. Report confidence and coverage; do not call
this a proof.

**NO-GO:** one unexplained mismatch is blocking. A deterministic event available only by
assuming a fixed physical core is not sufficient unless a supported hard affinity mechanism
actually enforces that assumption.

### AS-3 — Overflow delivery and bounded skid

**Question:** Can the counter preempt L2 reliably near a requested work deadline?

Program counter overflow at randomized targets across the AS-2 payloads. Measure:

- whether every armed overflow reaches the EL2 monitor;
- duplicate/spurious overflow;
- early/late delivery and maximum skid;
- contamination from monitor entry/exit;
- behavior across L2 exceptions, masked interrupts, WFI, and outer host preemption;
- read/write round-trip and outer-exit persistence of the armed counter and pending-overflow
  state (full snapshot closure belongs to AS-7).

Run no fewer than 1,000,000 armed deadlines under the same condition matrix as AS-2. Begin
with a conservative candidate margin of 4096 selected-event ticks; the measured result may
lower it but may not silently enlarge it. A result above the margin triggers an explicit
rerun/ruling, not automatic accommodation.

**Acceptance:** zero missed or duplicate overflows, a stable empirical skid bound below the
declared margin, and a complete explanation of every counter delta from arm to monitor entry.

**NO-GO:** any missed overflow, unbounded tail, or dependence on host wall time/core placement
that cannot be enforced.

### AS-4 — Exact landing by ARM debug step

**Question:** After an early overflow, can the monitor land on the exact deterministic work
target?

Implement overflow-early plus L2 single-step using the ARM debug architecture. The step
mechanism must be monitor-owned; guest debug-register access and debug exceptions must be
virtualized or denied so the subject cannot steal it. Exercise landing:

- in ordinary EL0 and EL1 code;
- on both sides of a syscall;
- through synchronous exceptions and exception return;
- through stage-1 and stage-2 faults;
- around WFI and interrupt acceptance;
- around branch instructions that do and do not increment the selected event;
- with a guest debug exception pending;
- at synthetic monitor-return/state-capture boundaries later used by snapshots.

**Acceptance:** at least 1,000,000 randomized deadlines land with `work == target`, never
overshoot, and produce the same PC/PSTATE/register/memory digest on replay. Every return to the
launcher leaves the monitor's one-shot step state disarmed, including when the stepped
instruction exits for another reason.

**NO-GO:** an architectural path can cross the target without an observable step boundary, or
the step state cannot be completely snapshotted/disarmed.

### AS-5 — ARM nondeterminism closure and CPU contract

**Question:** Can a cooperative ARM64 Linux subject be prevented from observing macOS state?

Write the ARM analogue of `CPU-MSR-CONTRACT.md` as an executable table, but initially only for
the exposed spike surface. Inventory and disposition at least:

- `CNTVCT_EL0`, `CNTPCT_EL0`, timer control/value registers, `CNTFRQ_EL0`, and ECV controls;
- `RNDR`/`RNDRRS` and feature advertisement;
- `ID_AA64*`, MIDR/MPIDR, cache topology, and frequency/capacity identity;
- PMU, debug, trace, statistical profiling, and branch-record facilities;
- WFI/WFE, event stream, and wait behavior;
- pointer authentication, MTE, SVE/SME, implementation-defined sysregs, and feature-dependent
  state that would expand snapshots;
- GIC and generic-timer state;
- LL/SC and LSE atomics.

For each row, name the hard mechanism: trap-and-emulate, fixed value, seeded stream,
deny/undefined, monitor ownership, or guest-build exclusion backed by a reachability gate.
“Hidden in `ID_AA64*`” is not sufficient if the raw instruction remains executable.

#### LL/SC ruling required here

The preferred contract is LSE-only Linux and userspace. De-risk three enforcement levels:

1. kernel configuration and alternatives guarantee LSE atomics for the pinned kernel;
2. all executable pages are scanned for exclusive load/store opcodes before execution, with
   W^X and rescan-on-exec for dynamically generated code;
3. if complete scanning cannot be enforced, trap or emulate the relevant instruction family
   through a slower translated path.

The final document must state whether LL/SC is mechanically unreachable or merely a
cooperative residual risk. Do not let “rr also requires LSE” substitute for our own closure.

**Acceptance:** the contract table, generated model, monitor controls, and adversarial payloads
agree. Same-seed runs of every allowed nondeterministic instruction return identical values;
every denied instruction gets the specified architectural fault; raw counter/RNG probes cannot
bypass the table.

### AS-6 — Minimal deterministic ARM64 Linux

**Question:** Does a real single-vCPU Linux subject boot and run using only the closed surface?

Only now add Linux. Build a content-pinned ARM64 kernel and initramfs with:

- one online vCPU; no real SMP execution;
- LSE atomics and the AS-5 LL/SC ruling enforced;
- a frozen CPU feature model;
- deterministic counter/time path supplied by the monitor;
- deterministic GICv3/generic-timer behavior;
- seeded entropy only;
- no passthrough devices, host filesystem, network, audio, GPU, or wall-clock RTC;
- a minimal console/hypercall doorbell suitable for evidence collection.

Milestones are deliberately narrow:

1. kernel decompressor and early console;
2. scheduler and deterministic idle/resume;
3. userspace init;
4. compute payload with syscalls, page faults, timers, signals, and LSE atomics;
5. same-seed twice with checkpoint state hashes.

**Acceptance:** at least 100 cold boots and 10,000 fresh-VM workload runs from the same
content-pinned initial image are bit-identical in canonical serial output, event ledger,
memory hash, and complete CPU/device/monitor state at named work boundaries. Seed-sensitive
inputs change only the intended seeded channels. Snapshot-derived repetition belongs to AS-7.

### AS-7 — Snapshot, restore, and branching closure

**Question:** Can every state component—including nested machinery—be captured and restored?

Enumerate and serialize:

- L2 GPR, PSTATE, FP/SIMD, system registers, pending exceptions, and idle state;
- EL2 monitor architectural state and monitor-owned ledgers;
- nested stage-2 tables and TLB invalidation epoch;
- PMU counter/config/overflow and debug single-step state;
- virtual GIC and generic timer state;
- seeded RNG state and all device state;
- L2 RAM and executable-page scan/W^X metadata.

Use host-mapped RAM plus nested stage-2 write protection for the first correct CoW design.
Hypervisor.framework has page permission APIs but no public dirty log; a full scan or
write-fault ledger is acceptable until measurements justify optimization. Never include
translation caches or host scheduling state in the canonical hash; invalidate/rederive them.

**Acceptance:** snapshot at every AS-4 transition class, perturb execution, restore, and
re-run to an identical state hash. Restoring an armed overflow, pending interrupt, WFI state,
or post-fault instruction must not duplicate or lose an event. Branch A→restore→A is identical;
A→restore→B differs only by the seeded modulation.

### AS-8 — Complete the standalone deterministic hypervisor

**Question:** Do the proven mechanisms compose into the working end-to-end system defined at
the top of this document?

Only after AS-2 through AS-7 are GO, connect the research components in their parallel
workspace:

- one-command content-pinned build and launch;
- launcher, EL2 monitor, ARM contract, Linux image, GIC/timer, deterministic services, and
  snapshot engine;
- a seed-driven run/replay/branch command surface sufficient to exercise the machine;
- canonical state hashing and deterministic-twice gates at named work boundaries;
- fail-closed capability and host-compatibility checks before guest entry;
- crash-safe evidence retention and a runbook another Mac can execute.

**Acceptance:** the “working” definition above passes on the pinned supported host tuple. At
least 10,000 same-seed end-to-end runs and a seed-sensitivity corpus pass with every sample
accounted for. This stage does not edit consonance, preserve x86 state hashes, or prove a
shared abstraction.

### AS-9 — Robustness and performance characterization

This is after correctness, not part of the feasibility claim. Measure:

- work throughput and exit cost by reason;
- RDTSC-analogue (`CNTVCT`) trap frequency and fast-path options;
- snapshot capture/restore/branch latency and memory amplification;
- PMU behavior across macOS updates, same-family Mac models, host load, thermal pressure,
  sleep/wake, and long campaigns;
- performance/efficiency-core migration and whether the selected event remains semantically
  identical;
- monitor attack surface and malformed L2 behavior.

Pin the supported tuple `(Mac model/chip family, macOS build range, monitor build, guest
image hashes)` until cross-version evidence justifies widening it. A framework update that
changes hidden behavior is a failed host assertion, never an automatic baseline refresh.

## Decision ladder

Each stage ends with one of four explicit dispositions:

- **GO** — acceptance criteria met; next stage may begin.
- **PROVISIONAL GO** — empirical evidence is clean but bounded; next stage may begin while the
  limitation remains named and is stress-tested again at AS-6/AS-9.
- **REDESIGN** — the property appears achievable with a named change inside the same
  hardware-accelerated thesis; repeat the stage before proceeding.
- **NO-GO** — the fast path lacks a required hard mechanism. Stop production work and record
  whether instruction-by-instruction Hypervisor.framework execution or TCG is the fallback.

Never convert NO-GO into GO by relaxing “bit-identical,” accepting a wall-clock dependency,
or counting unverified/missing samples as successes.

## Goal Mode execution packets

Codex Goal Mode is appropriate because each stage has a durable objective and measurable
success criteria (the
[official Codex use-case catalog](https://developers.openai.com/codex/use-cases) describes
“Follow a goal” as a durable objective for long-running work). Avoid an underspecified goal
such as only “build the Apple-silicon backend”: without the stage gates and terminal definition
below, it would be easy to stop at a report or spend weeks below an invalid assumption.

There are now two supported execution styles:

1. **Preferred unattended master goal:** one durable objective owns AS-0 through AS-9 and
   continues until the working standalone hypervisor passes its gates or a genuine hardware
   NO-GO is demonstrated. The stage dispositions are mandatory internal checkpoints; the goal
   must not skip them or stop merely because a report or bare payload works.
2. **Isolated stage goals:** use one goal per stage when human review or scarce hardware access
   should gate continuation. Start the next only after the prior disposition is recorded.

The master goal prompt is:

```text
Objective: Deliver the working standalone Apple-silicon deterministic hypervisor defined in
docs/APPLE-SILICON.md. Use Hypervisor.framework as L0, a Harmony monitor at virtual EL2, and
a content-pinned ARM64 Linux subject at EL1/EL0.

Work autonomously through AS-0 through AS-9 in order. Treat every stage's acceptance criteria
and disposition as a mandatory internal gate. Continue past intermediate reports, probes,
bare payloads, and Linux boot while safe in-scope progress remains. The terminal success is
the document's end-to-end “working” definition, not feasibility prose.

Do not use Beads or the `bd` CLI for planning, task tracking, memory, status, dependencies,
or handoff during this goal, even if repository-level agent instructions recommend it. This
goal's explicit no-Beads instruction overrides that default. Keep the durable objective and
stage state in Goal Mode; retain detailed progress through the stage evidence directories,
machine-readable experiment manifests, and the dispositions recorded in this document. Do
not create, claim, update, close, or synchronize Beads issues as a side effect of this work.

When running with GPT-5.6 Sol Ultra, the primary agent is the sole experiment coordinator and
owner of stage dispositions. Subagents may parallelize bounded research, source inspection,
test construction, offline trace analysis, independent calculations, and review of proposed
changes. They must not execute Hypervisor.framework or physical-machine experiments, mutate
shared VM/runtime state, consume scarce reset credits, declare a stage disposition, or
interpret the final determinism result. The primary agent must serialize every
hardware-backed VM run, record its environment before execution, account for every attempted
sample, and personally validate the raw evidence before accepting it.

Give each subagent a concrete, non-overlapping deliverable and an explicit file ownership
boundary. Do not allow concurrent edits to the same file or generated artifact. Subagents
return findings and candidate patches to the primary agent; the primary agent resolves
conflicts, integrates changes, runs the authoritative validation, and updates the durable
stage record. Keep only one hardware executor and one writer for each evidence manifest.

Implement in the parallel spikes/apple-silicon workspace. Use consonance and the existing x86
contracts only as references; do not refactor or integrate with consonance, preserve its APIs,
or build shared abstractions. Optimize for short experiment cycles and retained evidence.

Use consonance/unison as the reference design for the determinism oracle. Copy or lightly
vendor its domain-free comparison and divergence-bisection logic into
spikes/apple-silicon/oracle rather than coupling the new hypervisor to consonance. Adapt it so
VM creation and execution are fallible: an identical initialization failure, boot failure,
timeout, unsupported capability, or runtime error must fail the test loudly and must never be
hashed as two “identical” machines.

Do not use the unison-derived oracle to certify AS-0 through AS-3; those stages require direct
hardware-specific PMU/count/overflow evidence. Introduce the Machine adapter at AS-4 only
after exact run-to-work exists. Its contract is:

- spawn(seed): create a fresh VM or restore a content-pinned base state, with work == 0;
- run_to(target): PMU overflow-early plus monitor-owned debug step, returning only at the
  exact work target or an explicit halt/error;
- work(): the selected deterministic L2 work count;
- state_hash(): a pure canonical hash of L2 RAM and CPU state plus EL2 monitor, nested
  translation metadata, PMU/debug, GIC/timer, seeded RNG, pending-event, device, and output-log
  state—never host addresses, caches, scheduling state, or wall time;
- observable_digest(): console and explicit guest event output only, excluding latent RNG and
  device state.

Use same-seed comparison and divergence bisection for AS-4 onward. At AS-6, use it for Linux
deterministic-twice gates. At AS-7, make spawn restore a post-boot base snapshot so repeated
comparison and bisection are practical. Add separate conformance and seed-sensitivity oracles;
same-seed state-hash equality alone is necessary but insufficient.

Do not switch the primary effort to QEMU TCG, relax bit-identical determinism, accept macOS
wall time, infer missing samples as successes, or silently substitute a software counter for
the nested PMU under test.

At each stage, retain source, commands, machine-readable raw results, environment and content
hashes, and the GO/PROVISIONAL GO/REDESIGN/NO-GO disposition. If a result fails, diagnose and
try reasonable hardware-accelerated redesigns within the nested-EL2 thesis. Stop only when the
working system passes all gates or a named hard mechanism is conclusively unavailable.

Do not commit or push unless separately authorized. Report the exact terminal result,
validation evidence, residual risks, and changed files.
```

Each goal prompt should use this template:

```text
Objective: Complete APPLE-SILICON.md stage AS-N and produce its required evidence and
GO/PROVISIONAL GO/REDESIGN/NO-GO disposition.

Read first: AGENTS.md, docs/APPLE-SILICON.md, docs/ARM-PORT.md,
docs/ARCH-BOUNDARY.md, RESEARCH.md, and the files explicitly named by AS-N.

Scope: Only the AS-N question and deliverables. Do not begin AS-(N+1), refactor the
production architecture seam, build a TCG backend, or weaken an acceptance criterion.

Persistence: Continue through implementation, execution, diagnosis, and reruns until the
acceptance criteria are met or a named NO-GO mechanism is demonstrated. Unsupported or
unavailable hardware is evidence, not permission to simulate a pass.

Tracking: Do not use Beads or invoke `bd`, even if repository-level instructions recommend
it. Use the Goal Mode objective, the stage evidence directory, machine-readable experiment
manifests, and the disposition in APPLE-SILICON.md as the only progress sidechannel.

Subagents: The primary agent owns the stage disposition and is the only hardware executor.
Subagents may perform bounded research, offline analysis, test construction, and review with
non-overlapping file ownership. They must not run hardware-backed VMs, mutate shared runtime
state, consume reset credits, write the authoritative evidence manifest, or judge the final
determinism result. The primary agent integrates their work and runs every authoritative
validation serially.

Evidence: Retain source, exact commands, machine-readable raw results, content hashes,
environment manifest, and a concise analysis. Every attempted sample must be accounted for.

Completion: Update APPLE-SILICON.md with the disposition and evidence location. Report
changed files, validation, residual uncertainty, and the exact next permitted stage. Do not
commit or push unless separately authorized.
```

Stage-specific objective suffixes:

| Goal | Objective suffix |
|---|---|
| AS-0 | Establish the supported-host/API truth table on available Apple-silicon Macs. |
| AS-1 | Run deterministic bare EL1/EL0 payloads beneath a minimal Harmony EL2 monitor. |
| AS-2 | Discover and validate a deterministic, L2-only nested PMU work event. |
| AS-3 | Prove reliable PMU overflow delivery and establish a bounded skid margin. |
| AS-4 | Land exactly at randomized work deadlines using monitor-owned ARM debug step. |
| AS-5 | Close the ARM nondeterminism surface and ratify an enforceable LL/SC ruling. |
| AS-6 | Boot and repeatedly execute minimal deterministic ARM64 Linux. |
| AS-7 | Prove complete snapshot/restore/branch behavior, including hidden nested state. |
| AS-8 | Compose the proven components into the standalone deterministic Linux hypervisor. |
| AS-9 | Characterize and pin the supported host/performance envelope. |

AS-2, AS-3, and AS-4 are the existential sequence. If scheduling only one substantial Goal
Mode run now, choose **AS-2**, but only after AS-0 and AS-1 have supplied a functioning nested
test harness.

## Repository layout for research artifacts

Keep the parallel implementation reviewable without forcing it into production crates:

```text
spikes/apple-silicon/
├── README.md                 # commands, environment, current disposition
├── launcher/                 # macOS Hypervisor.framework host
├── monitor/                  # virtual-EL2 monitor
├── payloads/                 # bare L2 assembly/Rust payloads
├── linux/                    # admitted at AS-6; content pins + build recipe
├── schemas/                  # canonical evidence formats
└── results/
    └── <stage>/<host>/<run-set>/
```

Raw result volume that is too large for git must be content-addressed and accompanied by a
checked-in manifest, summary, and reproduction command. Golden evidence is immutable; reruns
create a new run-set rather than overwriting history.

## Deferred consolidation

`docs/ARCH-BOUNDARY.md` describes one plausible future integration, not a constraint on this
program. After AS-8 is working and AS-9 characterizes it, make a separate human architecture
ruling based on real code and measurements. Valid outcomes include extracting common traits,
keeping two implementations behind a process boundary, sharing only trace/state schemas, or
leaving the Apple backend standalone. No consolidation work belongs to the current goal.

## Immediate focus

The next engineering move is **AS-0 followed by AS-1**, solely to make AS-2 runnable. The first
scientifically interesting result is AS-2: whether Apple's nested virtual PMU exposes a
deterministic L2 work event. Linux boot, architecture refactoring, snapshot optimization, and
TCG work are downstream and must not displace that measurement.
