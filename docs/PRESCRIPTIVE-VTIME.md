# Prescriptive V-time — assigned virtual time, and consonance on ARM

Plan of record for bringing up consonance with **prescriptive V-time** — virtual time
the run loop *assigns* at VM exits — on two ARM hosts: `msr1` (arm64 Linux/KVM, via
ssh) and an M1 Max (macOS, Hypervisor.framework). The term comes from the existing
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
delivery path. A timer's deadline is met at the first exit at or after it; which
exit that is is itself deterministic, so two same-seed runs deliver every
interrupt at the same instruction boundary.

The pvclock page is stamped at exits, exactly as `docs/PARAVIRT-CLOCK.md`
specifies today; guest-visible time already only changes at exits, so the guest
side of the clock protocol is untouched.

### 2.2 Backends

The `Backend` trait (`consonance/vmm-backend/src/backend.rs`) is reused as-is:
one impl per (substrate, arch) pair, nothing above it branching on substrate.

- **`Arm64KvmBackend` (msr1).** The existing stock-KVM/arm64 skeleton
  (`arm64_kvm.rs`) grows the pieces its `capabilities()` currently reports
  absent: interrupt injection and `run_until`. Interrupt state lives in the
  userspace `consonance/gicv3` model; injection happens at exits. This takes up
  the work the arm64 delivery ruling in `AGENTS.md` deferred — that deferral is
  superseded by this plan.
- **`HvfBackend` (M1 Max).** A new impl of the same trait over
  Hypervisor.framework: `hv_vcpu_run`, WFI/MMIO/sysreg exits, interrupt
  injection before reentry. It uses the same userspace `gicv3` crate as the KVM
  backend, so interrupt behavior is decided by shared code and is identical
  across substrates by construction.

The run loop drives either through `run_until` with prescriptive advancement; the
`WorkSource` in use reads zero, and the injection planner's overflow/single-step
states are never entered.

### 2.3 Guest image

The existing arm64 harmony-Linux image is already built for this discipline: its
image audit rejects any surviving live counter-read or LL/SC opcode (LSE atomics
only), and it carries the pvclock page clocksource and `/dev/harmony`
(`harmony-linux/linux/patches/arm64/`, `build-arm64-kernel.sh`).

Additions:

- **ISA baseline.** One pinned feature set both hosts implement (ARMv8.2-class +
  LSE; the intersection of M1 and msr1's core, captured in the ID-register policy
  and asserted by the kernel build). One image, byte-identical on both hosts,
  attested by `MANIFEST.sha256`.
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
instrumented-payload milestone (M6).

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

The searcher binary runs on the host (M1 or the msr1 login), speaking
control-proto to the VMM; archive, energy selection, and parent policy are
unchanged code.

## 3. Verification

The failure mode this plan is designed against: a bring-up that "works" because
its checks are too weak to fail. Three rules apply to every milestone:

1. **Full-log comparison, never final-state comparison.** The unit of evidence is
   the **event log**: the ordered sequence of
   (exit index, exit class + payload digest, vns after advance, interrupts
   injected at this exit), plus a guest-memory hash at every checkpoint interval
   and at the end. Two runs are "identical" only if their logs are. Divergences
   are bracketed with `unison::compare_runs`.
2. **Every comparator is proven able to fail before its first real use.** Before a
   milestone's oracle counts, run it against a deliberately perturbed twin — one
   vns increment off by one, one interrupt delivered one exit late, one byte
   flipped in guest memory — and record that it reports the divergence at the
   right index. A comparator that has never failed proves nothing.
3. **Same bytes or no claim.** Any cross-host comparison first asserts the guest
   image, payload, and seed are byte-identical (`MANIFEST.sha256` + input digest)
   on both sides. Every run's report includes exits/sec by class, wall time, and
   the guest-time/wall-time ratio, so cost regressions are visible in the same
   place as correctness.

### Milestones

Each milestone lists what is built, what passing means, and the conditions under
which a pass does not count. Later milestones depend on earlier ones; none is
declared done from a subset of its oracle.

**M0 — prescriptive advancement in pure logic.**
*Build:* the run-loop advancement and delivery rules of §2.1 in `vmm-core`,
driven against `MockBackend` and scripted exit streams; the event log and its
comparator.
*Passes when:* property tests hold — monotonicity, delivery of every deadline at
the first exit at or after it, log equality for identical scripts, log
divergence at the exact index for perturbed scripts.
*Does not count unless:* the comparator's failure cases (rule 2) are themselves
committed tests.

**M1 — msr1 boots deterministically.**
*Build:* `Arm64KvmBackend` interrupt injection + `run_until`; userspace GICv3
delivery at exits; WFI via `IdlePlanner`; the paravirtual tick patch; boot the
arm64 image to `/init`.
*Passes when:* ten same-seed boots produce one event log: identical exit
sequences, identical vns at every exit, identical interrupt placements,
identical memory hash at every checkpoint and at `/init`.
*Does not count unless:* the log covers the full boot (first entry to `/init`),
interrupt placements are in the log, and the comparator was first shown to catch
a one-exit-late tick injection on this workload.

**M2 — NES campaign on msr1.**
*Build:* the §2.5 payload and the control-proto `Machine` client; run
`smb-smoke`, then a campaign of meaningful length.
*Passes when:* (a) two same-seed campaigns produce identical archive hashes;
(b) every archived lineage, replayed through the hypervisor, reproduces its
archive key byte-for-byte; (c) a **three-way agreement** holds on a sampled set
of lineages: the in-process `NesMachine` and the consonance-hosted payload,
given the same chord sequence, produce identical WRAM at each chord boundary —
the in-process emulator is an independent reference implementation, and any
disagreement indicts the guest or the VMM, never the payload.
*Does not count unless:* the campaign ran long enough to exercise snapshot
churn at real scale (thousands of branch/replay cycles, non-quiescent
snapshots), and the archive-hash comparator was shown to catch a seeded
divergence (one chord altered in one lineage).

**M3 — HvfBackend brings up the same image on the M1 Max.**
*Build:* first a probe binary that confirms the required Hypervisor.framework
surface (WFI exit control, sysreg trap coverage, injection timing) on this
macOS version; then the `Backend` impl; then M1-boot with the M1 as host.
*Passes when:* the M1 passes milestone M1's oracle verbatim: ten same-seed
boots, one event log, on the same image bytes as msr1.
*Does not count unless:* the probe's findings are recorded in the backend's
docs (which traps exist, which required emulation), and `capabilities()`
reports the backend's honest surface.

**M4 — cross-host determinism, the portability claim itself.**
*Build:* nothing new — the experiment.
*Passes when:* with byte-identical image, payload, and seed: (a) msr1 and the
M1 Max produce **identical event logs** for the boot and for an NES campaign,
and identical archive hashes; (b) a snapshot taken mid-lineage on msr1,
restored on the M1, continues to the same archive key, and vice versa.
*Does not count unless:* rule 3 held (bytes attested on both sides before the
run), the comparison is the full log (rule 1), and at least one seeded
cross-host divergence was demonstrated to be caught (rule 2, run once with an
increment constant deliberately differing between hosts).

**M5 — liveness on a real payload.**
*Build:* the postgres container payload from the acceptance suite, booted and
driven under prescriptive V-time with the paravirtual tick.
*Passes when:* the payload's existing acceptance checks pass; dmesg is free of
RCU-stall and soft-lockup reports; the inter-exit vns gap histogram is
recorded and its maximum stays under the tick period times a small documented
factor; throughput is reported next to the descriptive-mode x86 number for the
same payload.
*Does not count unless:* the gap histogram and throughput are in the report —
a payload that "passes" with an unbounded quiet stretch or a 100× slowdown is
a finding, and the report must be capable of showing it.

**M6 — instrumented concurrency payload (absolute finding measurement).**
*Build:* the SDK threshold protocol of §2.4; a small suite of deliberately racy
instrumented Go/Rust programs, each with a known bug and a known reproducing
schedule.
*Passes when:* the searcher, given the schedule vocabulary of instrumented
yields, reproduces each seeded bug from its seed, deterministically.
*Does not count unless:* each suite entry was first shown to *not* reproduce
under a wrong schedule (the bug requires the interleaving, i.e. the suite can
fail), and results are reported per-bug, never as a single pass rate.

This suite is the standing instrument for schedule-expressiveness questions;
when the descriptive x86 design matures to a comparable state, running the same
suite there produces the comparative measurement.

## 4. Risks and their measurements

- **Quiet stretches delay timer delivery.** A long computation with no exits
  delivers its due timers late (at its next exit). Measured directly by M5's
  gap histogram; bounded by the paravirtual tick density and, for instrumented
  payloads, by the SDK threshold. A payload that exceeds the bound is reported
  by the run, with the gap and the guest PC range.
- **Hypervisor.framework surface unknowns.** Which sysregs trap, WFI exit
  behavior, and injection timing vary by macOS version. M3's probe binary
  resolves this before backend work starts, and the findings are recorded.
- **ISA baseline drift between M1 and msr1.** Implementation-defined behavior
  outside the pinned baseline (ID registers, FP corner behavior) would surface
  as an M4 log divergence; `compare_runs` brackets it to an instruction range.
  The NES payload is integer-pure, which keeps M4's first pass free of FP
  questions; FP-heavy payloads extend the baseline audit when they arrive.
- **Snapshot rate under searcher churn.** VM snapshots are large and the
  searcher branches constantly. M2's report includes branch/replay throughput;
  optimization (dirty-page tracking, copy-on-write) is scheduled by that
  number, and the determinism oracles are unaffected by it.

## 5. In-tree placement

Everything lands in the existing crates: advancement and delivery in
`vmm-core`'s run loop, the two backends in `vmm-backend`, interrupt state in
`gicv3`, clock and queue in `vtime` (unchanged), guest changes under
`harmony-linux`, the workload under `dissonance` behind its `Machine` trait,
and the contract constants in the determinism-contract tables. Follow-up work
items are tracked as GitHub issues per repo policy.
