# Prescriptive V-time — assigned virtual time, and consonance on ARM

Plan of record for bringing up consonance with **prescriptive V-time** — virtual time
the run loop *assigns* at VM exits — on ARM, with the M1 Max (macOS,
Hypervisor.framework) as the development and bring-up host. The `msr1` box (arm64
Linux/KVM, via ssh) joins as a second host in the follow-up work of §5 when it goes
idle; the plan's milestones run entirely on the M1. The term comes from the existing
clock documentation: `consonance/vtime/src/idle.rs` describes the idle jump as a
prescriptive advance layered on the descriptive base clock. This plan makes the
prescriptive path carry all of V-time.

## 1. Problem

Consonance is meant to be the go-everywhere deterministic hypervisor: any run
reproducible from its seed, on whatever machine is in front of you — a Linux server,
a laptop, a CI runner — with sessions portable across hosts of the same guest ISA.

Today V-time is a measured quantity: a hardware count of retired conditional
branches, read from a pinned `perf_event` counter at every VM exit
(`consonance/vmm-core/src/work.rs`, `consonance/vtime`). Binding time to a
microarchitectural measurement binds correctness to a long chain of host
properties, each of which is an architectural challenge in its own right:

- **An exact, guest-only vPMU.** The counter must attribute every guest branch and
  nothing else. This exists on bare-metal Linux/x86 with a patched KVM; it does not
  exist on macOS, on Apple Silicon, on most cloud instances, or under nested
  virtualization (`docs/NESTED-INTEGRATION.md` needs "one VMX-root layer with an
  exact vPMU" and hunts for hosts that supply it).
- **A pinned, single-tenant, homogeneous host** (`docs/CPU-MSR-CONTRACT.md`),
  largely so the counter stays trustworthy across scheduling
  (`WorkError::Untrustworthy`, the cross-VM accumulation hazard in `work.rs`).
- **Counter semantics that match across machines.** What "retired conditional
  branch" counts varies by vendor and family, so a recording replays only on
  counter-identical hardware.
- **Skid machinery.** Precise injection needs the arm-early/single-step planner
  (`InjectionPlanner`), a skid margin calibrated per part, and a
  `SkidExceeded` failure mode.

Every new host class re-opens all four questions. The macOS port stalled on the
first one.

**The change:** V-time becomes a value the run loop assigns at VM exits, computed
as a pure function of the guest's own exit stream. Interrupts are delivered at
exits. Determinism then rests on one property — the guest's instruction stream is
architecturally deterministic between exits — which every conforming CPU of the
pinned ISA baseline supplies. The host's job shrinks to: trap the exits, order
them, inject at them. That job is implementable on stock KVM, on
Hypervisor.framework, on any Apple Silicon generation, and on unpinned shared
machines, and it makes recordings and snapshots portable across hosts of the same
ISA baseline.

**The deviation, stated plainly.** Descriptive V-time can preempt the guest at
any retired-branch boundary: the counter lets a timer interrupt land mid-stream,
at an exact instruction, anywhere in the guest — including inside a stretch of
code that performs no I/O and takes no trap. Prescriptive V-time gives that
capability up, deliberately. Interrupt delivery stays fully deterministic — the
placement contract of §2.1 fixes every delivery to an exact, reproducible
instruction boundary — but the set of boundaries available is the guest's exit
stream, at the density the guest supplies, rather than every branch. We accept
this because the preemption capability is precisely what binds the design to an
exact vPMU, a pinned single-tenant host, and a patched kernel — the whole cost
side of §1 — while the interleavings it uniquely reaches are those inside
exit-free stretches, and the platform's bug-finding leverage concentrates
elsewhere: schedule permutation over kernel-mediated events and instrumented
code (where exit density is high and steerable), device-level and crash-recovery
faults (which live at exits by nature), and workload permutation. Where the
instrumented and kernel-supplied exit density thins out, M3 measures the gap and
the liveness monitor reports the stretches; where an exact vPMU exists — the
x86 box — descriptive V-time retains the full capability. The two modes are the
same VMM making a different trade per host, and M4's suite is the standing
instrument for measuring what the trade costs in found bugs.

x86 descriptive V-time on the box continues unchanged. Prescriptive V-time is how
consonance runs everywhere else, arm64 first.

## 2. Design

### 2.1 The clock

`VClock` is reused as-is. Its model is already
`vns(work) = vns_base + work·ratio`; prescriptive V-time carries the whole clock
in `vns_base` and holds `work` at zero. Every advance goes through the existing
`VClock::advance_idle` — the one mutation the clock already defines for time
moving without measured work (idle skip and snapshot restore today).

At each VM exit the run loop advances the clock by an increment determined by the
exit itself:

| Exit | Increment |
|---|---|
| Doorbell hypercall (SDK yield, input fetch, paravirtual tick) | Carried by or derived from the request: a payload-declared duration, or the tick period |
| Device MMIO access | A per-device-class constant from the determinism contract |
| Trapped time read (counter-shaped sysreg, pvclock refresh) | A small constant, so time-polling loops make progress |
| WFI (idle) | `IdlePlanner::plan` — advance to the next `TimerQueue` deadline, exactly as today |

Every increment is a pure function of (exit class, exit payload). The constants are
normative contract values, recorded alongside the MSR/sysreg dispositions.

`TimerQueue` is unchanged. Delivery: after advancing at an exit, pop every due
deadline and inject before reentry — the paths the codebase already defines as
`PlanOutcome::TargetInPast` and `IdleAdvance::already_due` become the universal
delivery path. **The delivery contract:** each deadline `D` is delivered exactly
once, at the first exit whose post-advance vns is at or after `D`, with equal
deadlines in `TimerQueue`'s FIFO order. Which exit that is is itself
deterministic, so two same-seed runs deliver every interrupt at the same
instruction boundary. `run_until` under prescriptive V-time runs to the next
exit; a deadline never stops the guest mid-stream — it is met at the exit that
follows it. Delivery latency is therefore bounded by exit density, which the
guest supplies (the paravirtual tick, SDK yields, device access, WFI) and every
run measures (§3, M3's histogram).

**Liveness monitor.** The run loop carries a host wall-clock watchdog that fires
when the guest executes past a budget with no exit. The watchdog **aborts the
run** and reports the guest PC range — it never injects into or otherwise
perturbs the guest, so it reads host time without that time ever reaching guest
state, and determinism is unaffected. This is the design's kill condition for
an exit-free stretch: the run ends loudly, the workload is recorded as
incompatible, and a payload whose runs abort this way fails its milestone.
Supported workloads are those whose execution reaches exits at the density
their own I/O, the paravirtual tick, and (when instrumented) the SDK
thresholds supply; the watchdog converts everything outside that into a
finding.

The pvclock page is stamped at exits, exactly as `docs/PARAVIRT-CLOCK.md`
specifies today; guest-visible time already only changes at exits, so the guest
side of the clock protocol is untouched.

### 2.2 Backends

The `Backend` trait (`consonance/vmm-backend/src/backend.rs`) is reused as-is:
one impl per (substrate, arch) pair, nothing above it branching on substrate.

- **`HvfBackend` (M1 Max) — the bring-up backend.** A new impl of the trait
  over Hypervisor.framework: `hv_vcpu_run`, WFI/MMIO/sysreg exits, interrupt
  injection before reentry, and `hv_vcpus_exit` as the watchdog's abort path.
  Interrupt state lives in the userspace `consonance/gicv3` model; injection
  happens at exits. `run_until` here is run-to-next-exit per §2.1's delivery
  contract — the backend needs no guest-work counter and no mid-stream stop,
  which is what makes Hypervisor.framework's exit surface sufficient.
- **`Arm64KvmBackend` (msr1, follow-up §5).** The existing stock-KVM/arm64
  skeleton (`arm64_kvm.rs`) grows the pieces its `capabilities()` currently
  reports absent — interrupt injection and `run_until` (run-to-next-exit, per
  §2.1). Delivery on KVM/arm64 requires a decision the `gicv3` crate already
  frames as the AA-6 verdict: stock KVM couples the GICv3 CPU interface and
  the timer PPI to the in-kernel vGICv3, so the backend either creates the
  in-kernel vGICv3 and injects through `KVM_IRQ_LINE` — with the vGIC's
  save/restore folded into `state_hash` and its bit-identical round-trip
  demonstrated as part of F1's oracle — or carries an arm64 kernel patch that
  routes injection and the ICC sysregs to the userspace `gicv3` model. §5
  records the decision when F1 starts. This takes up the work the arm64
  delivery ruling in `AGENTS.md` deferred — that deferral is superseded by
  this plan.

The run loop drives either through `run_until` with prescriptive advancement; the
`WorkSource` in use reads zero, and the injection planner's overflow/single-step
states are never entered.

### 2.3 Guest image

The existing arm64 harmony-Linux image is already built for this discipline: its
image audit rejects any surviving live counter-read or LL/SC opcode (LSE atomics
only), and it carries the pvclock page clocksource and `/dev/harmony`
(`harmony-linux/linux/patches/arm64/`, `build-arm64-kernel.sh`).

Additions:

- **ISA baseline.** One pinned feature set, chosen conservatively so later hosts
  can run the same image (ARMv8.2-class + LSE — implemented by the M1 and by
  common server cores; captured in the ID-register policy and asserted by the
  kernel build). One image, attested by `MANIFEST.sha256`.
- **Paravirtual tick.** A kernel patch that rings the doorbell at deterministic
  points in the kernel's own execution — timer-tick processing sites, every Nth
  syscall entry, context switch. Each ring is a pure function of guest execution,
  so it adds exits (places where time advances and timers deliver) at kernel
  event density. The counter N is a contract constant.

### 2.4 SDK path

`harmony-linux/libvoidstar` already speaks the Antithesis SDK ABI to
`/dev/harmony` (assertion JSON, deterministic entropy). The coverage callback —
currently inert — becomes the userspace yield: each instrumented basic block
increments a per-thread counter, and crossing a threshold prescribed by the
hypervisor at the previous exit rings the doorbell. For the first workload below,
only the SDK input-fetch call is needed; the threshold protocol lands with the
instrumented-payload milestone (M4).

### 2.5 First workload: the NES searcher

The `dissonance` searcher (PR #193) is written against a `Machine` trait that
mirrors the control-protocol verb set — snapshot / drop / branch / replay / run /
read — precisely so it can move from the in-process NES emulator to a
control-protocol client unchanged. The guest payload is TetaNES headless linked
against the SDK; its loop is: fetch the next `ButtonChord` through the SDK
(doorbell exit), emulate the hold, report the frame count (doorbell exit),
repeat.

- `branch(snap, env)` → control-proto `Branch` with the chord suffix staged as
  the `RecordedEnv`; each input fetch consumes one entry.
- `run(until)` → control-proto `Run`, stopping at the `SnapshotPoint` yield whose
  reported frame count reaches the deadline. Stop conditions are expressed in
  frames (yield events), never in nanoseconds.
- `read(addr, len)` → control-proto `Read` against the payload's WRAM buffer,
  pinned at a guest-physical address registered at startup (the
  `live_moment_address` pattern). The searcher's WRAM decoders are untouched.
- `snapshot` / `replay` → the snapshot store. Prescriptive restore is simpler
  than today's: `vns_base` carries the whole clock, with no counter reset and no
  sub-nanosecond remainder.

The searcher binary runs on the M1 host, speaking control-proto to the VMM;
archive, energy selection, and parent policy are unchanged code.

## 3. Verification

The failure mode this plan is designed against: a bring-up that "works" because
its checks are too weak to fail. Three rules apply to every milestone:

1. **Full-log comparison over complete state, never final-state comparison.**
   Each run produces two logs. The **raw log** is backend-local: every exit as
   the substrate reports it, kept for debugging. The **normalized log** is the
   guest-visible record and the unit of all determinism claims: the ordered
   sequence of (event index, event class + payload digest, vns after advance,
   interrupts injected at this event), with `Vmm::state_hash` — the canonical
   serialization of *all* observable state: RAM, vCPU registers and sysregs,
   device and GIC state, serial capture, V-time, entropy position, SDK channel
   (`vmm-core/src/vmm.rs`, `state_blob`) — at every checkpoint interval and at
   the end. The prescriptive V-time and entropy chunks are wired into
   `state_blob` on these backends as part of M0. Two runs are "identical" only
   if their normalized logs, including every `state_hash`, are. Divergences are
   bracketed with `unison::compare_runs`.
2. **Delivery placement is checked against the schedule, not against another
   run.** An independent checker consumes a run's deadline schedule and its
   normalized log and asserts §2.1's delivery contract mechanically: every
   deadline delivered exactly once, at the first event whose vns is at or after
   it, in FIFO order for equal deadlines, with the masked-at-deadline,
   WFI-at-deadline, simultaneous-deadline, and reassertion-after-unmask cases
   each exercised by a dedicated workload. Two runs that agree with each other
   but both place a delivery late fail this checker.
3. **Every comparator is proven able to fail before its first real use.** Before
   a milestone's oracle counts, run it against a deliberately perturbed twin —
   one vns increment off by one, one interrupt delivered one exit late, one
   byte flipped in guest state — and record that it reports the divergence at
   the right index. A comparator that has never failed proves nothing. This
   demonstrates comparator sensitivity only; correctness of placement is rule
   2's job.
4. **Same bytes or no claim.** Any cross-host comparison first asserts the guest
   image, payload, and seed are byte-identical (`MANIFEST.sha256` + input digest)
   on both sides, and compares normalized logs (raw logs differ across
   substrates by construction). Every run's report includes exits/sec by class,
   wall time, and the guest-time/wall-time ratio, so cost regressions are
   visible in the same place as correctness.

### Milestones

Each milestone lists what is built, what passing means, and the conditions under
which a pass does not count. Later milestones depend on earlier ones; none is
declared done from a subset of its oracle.

**M0 — prescriptive advancement in pure logic.**
*Build:* the run-loop advancement and delivery rules of §2.1 in `vmm-core`,
driven against `MockBackend` and scripted exit streams; the normalized log and
its comparator; the delivery-placement checker of rule 2; the prescriptive
V-time and entropy chunks in `state_blob`.
*Passes when:* property tests hold — monotonicity, the placement checker green
over generated schedules including the masked / WFI-overlap / simultaneous /
reassertion cases, log equality for identical scripts, log divergence at the
exact index for perturbed scripts.
*Does not count unless:* the comparator's and the placement checker's failure
cases (rules 2 and 3) are themselves committed tests — including a script that
delivers every deadline consistently one exit late, which the comparator alone
cannot catch and the placement checker must.

**M1 — the M1 Max boots deterministically.**
*Build:* first a probe binary that confirms the required Hypervisor.framework
surface on this macOS version — WFI exit control, sysreg trap coverage,
injection timing, and **save/restore coverage**: which of the retained state
classes (general registers, SIMD/FP, sysregs including timer registers,
pending exception and debug state) the HVF get/set API captures on this
hardware — its findings recorded in the backend's docs; then `HvfBackend`
with `run_until`, userspace GICv3 delivery at exits, and WFI via `IdlePlanner`;
the paravirtual tick patch; boot the arm64 image to `/init`.
*Passes when:* ten same-seed boots produce one normalized log — identical event
sequences, identical vns at every event, identical interrupt placements,
identical `state_hash` at every checkpoint and at `/init` — **and** the
delivery-placement checker is green over each boot's log against its deadline
schedule, **and** no run hit the liveness watchdog.
*Does not count unless:* the log covers the full boot (first entry to `/init`),
interrupt placements are in the log, `capabilities()` reports the backend's
honest surface, and the comparator was first shown to catch a one-exit-late
tick injection on this workload (rule 3) — with the placement checker, not the
comparator, standing against the same error made consistently in all runs.
**State-completeness check:** for each state class retained in `state_blob` —
general registers, SIMD/FP, sysregs including the timer registers, pending
exception and debug state, the GIC model, device state, V-time, entropy
position — a committed test perturbs that class alone at a snapshot boundary
and shows `state_hash` changes and a restore round-trips it. A state class the
image makes unreachable (the exclusive monitor, under the image's LL/SC
prohibition) is documented as canonicalized at every sealable boundary instead,
with the audit that enforces the prohibition cited as the evidence.

**M2 — NES campaign on the M1 Max.**
*Build:* the §2.5 payload and the control-proto `Machine` client; run
`smb-smoke`, then a campaign of meaningful length.
*Passes when:* (a) two same-seed campaigns produce identical archive hashes;
(b) every archived lineage, replayed through the hypervisor, reproduces its
archive key byte-for-byte; (c) **snapshots are causally load-bearing**: the
campaign's continuations execute from restored snapshots (asserted by a restore
counter in the run report, with genesis replays counted separately), and on a
sampled set of branch points the restored continuation's per-chord `state_hash`
sequence equals the uninterrupted run's from the same point; (d) a
**cross-implementation differential** holds on a sampled set of lineages: two
independent implementations — the in-process `NesMachine` and the
consonance-hosted payload — plus the campaign's independently recorded
transport observations agree on WRAM at each chord boundary, with any
disagreement treated as unlocalized until component-level checks (chord
encoding, ROM/core configuration, boundary alignment) attribute it.
*Does not count unless:* the campaign ran long enough to exercise snapshot
churn at real scale — thousands of branch/replay cycles, with snapshots taken
while the guest is mid-workload rather than idle (each boundary itself a fully
serviced, sealable exit per the snapshot contract, `INTEGRATION.md` §4); the
archive-hash comparator was shown to catch a seeded divergence
(one chord altered in one lineage); and snapshot integrity checking was shown
to detect seeded corruption in each of a RAM page, a vCPU field, and a
GIC/device field of a stored snapshot (rule 3 applied to the restore path).

**M3 — liveness on a real payload.**
*Build:* the postgres container payload from the acceptance suite, booted and
driven under prescriptive V-time with the paravirtual tick.
*Passes when:* the payload's existing acceptance checks pass; no run hit the
liveness watchdog; dmesg is free of RCU-stall and soft-lockup reports; the
inter-exit vns gap histogram is recorded and its maximum stays under the tick
period times a small documented factor; throughput is reported next to the
descriptive-mode x86 number for the same payload.
*Does not count unless:* the gap histogram and throughput are in the report —
a payload that "passes" with an unbounded quiet stretch or a 100× slowdown is
a finding, and the report must be capable of showing it.

**M4 — instrumented concurrency payload (absolute finding measurement).**
*Build:* the SDK threshold protocol of §2.4; a small suite of deliberately racy
instrumented Go/Rust programs, each with a known bug and a known reproducing
schedule.
*Passes when:* (a) the searcher, given the schedule vocabulary of instrumented
yields, reproduces each seeded bug from its seed, deterministically; and (b)
for a held-out subset whose reproducing schedules are withheld from the
searcher and its fixtures, the searcher *discovers* a reproducing schedule
within a fixed, pre-declared budget per bug.
*Does not count unless:* each suite entry was first shown to *not* reproduce
under a wrong schedule (the bug requires the interleaving, i.e. the suite can
fail); the withheld subset's schedules are demonstrably absent from seeds and
fixtures; and results are reported per-bug, never as a single pass rate.

This suite is the standing instrument for schedule-expressiveness questions;
when the descriptive x86 design matures to a comparable state, running the same
suite there produces the comparative measurement.

## 4. Risks and their measurements

- **Quiet stretches delay timer delivery.** A long computation with no exits
  delivers its due timers late (at its next exit). Measured directly by M3's
  gap histogram; bounded by the paravirtual tick density and, for instrumented
  payloads, by the SDK threshold. A payload that exceeds the bound is reported
  by the run, with the gap and the guest PC range.
- **Hypervisor.framework surface unknowns.** Which sysregs trap, WFI exit
  behavior, and injection timing vary by macOS version. M1's probe binary
  resolves this before backend work starts, and the findings are recorded.
- **ISA baseline drift between hosts.** Implementation-defined behavior
  outside the pinned baseline (ID registers, FP corner behavior) would surface
  as a log divergence in the §5 cross-host experiment; `compare_runs` brackets
  it to an instruction range. The NES payload is integer-pure, which keeps that
  experiment's first pass free of FP questions; FP-heavy payloads extend the
  baseline audit when they arrive.
- **Snapshot rate under searcher churn.** VM snapshots are large and the
  searcher branches constantly. M2's report includes branch/replay throughput;
  optimization (dirty-page tracking, copy-on-write) is scheduled by that
  number, and the determinism oracles are unaffected by it.

## 5. Follow-up: msr1 and the portability claim

When the msr1 box goes idle, two pieces of deferred work turn the single-host
result into the portability claim. They reuse everything above unchanged.

**F1 — `Arm64KvmBackend` completes.** Interrupt injection and `run_until` on
the KVM/arm64 backend, per §2.2's delivery decision (in-kernel vGICv3 via
`KVM_IRQ_LINE` with bit-identical save/restore evidence, or a patched
injection ABI into the userspace `gicv3` model). The decision and its
supporting measurements are recorded here when this work starts. Passes
milestone M1's oracle verbatim on msr1 — ten same-seed boots, one normalized
log, placement checker green — on the same image bytes the M1 Max runs.

**F2 — cross-host determinism, the portability claim itself.**
*Passes when:* with byte-identical image, payload, and seed: (a) msr1 and the
M1 Max produce **identical normalized logs** (raw logs differ across
substrates by construction; only the normalized, guest-visible log is
compared) for the boot and for an NES campaign, and identical archive hashes;
(b) a snapshot taken mid-lineage on one host, restored on the other,
has canonical `state_hash` equality **immediately after restore**, and its
continuation's full normalized-log and `state_hash` sequence equals the
origin host's uninterrupted run from the same point — in both directions,
with the archive key match as the final consequence rather than the test.
*Does not count unless:* rule 4 held (bytes attested on both sides before the
run), the comparison is the full normalized log with `state_hash` checkpoints
(rule 1), at least one seeded cross-host divergence was demonstrated to be
caught (rule 3, run once with an increment constant deliberately differing
between hosts), and — when F1 chose the in-kernel vGICv3 — the kernel's GIC
state is normalized into the architectural representation the userspace
`gicv3` model defines before it enters `state_blob`, with committed
equivalence tests showing the two models hash identically for the same
architectural state.

## 6. In-tree placement

Everything lands in the existing crates: advancement and delivery in
`vmm-core`'s run loop, the two backends in `vmm-backend`, interrupt state in
`gicv3`, clock and queue in `vtime` (unchanged), guest changes under
`harmony-linux`, the workload under `dissonance` behind its `Machine` trait,
and the contract constants in the determinism-contract tables. Follow-up work
items are tracked as GitHub issues per repo policy.
