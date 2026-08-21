# CPU-QUALIFICATION — the rung-1 suite

This document specifies the suite that implements rung 1 of `docs/TESTING.md`. The suite
is a tool that runs on demand on a physical chip. It decides whether the chip can host
the determinism machinery, and it measures the constants the machinery needs on that
chip. `docs/TESTING.md` remains the authority for what rung 1 proves; this document
says what to build.

Today each hardware program builds its own measurement harness. The suite replaces
that. One binary qualifies a new chip, re-certifies a known one after a microcode or
kernel change, and re-checks a box's host conditions at the start of a run window.

## What a run produces

A run produces two artifacts. Both names come from `docs/TESTING.md` and the spike
program docs.

The first is **the qualification report**: the evidence of one run. It holds every
check's result, the retained raw records behind each result, and the counts per
classification bucket. It lives in the run's evidence directory and is never checked
in.

The second is **the measured-constants pack**: the per-chip data the VMM consumes.
It holds the work-clock event config, the skid margin, count offsets per exit class,
event density, single-step semantics, and the standing host conditions. One pack file
per chip baseline, named in the `det-cfl-v1` pattern, checked in at
`docs/chips/<baseline>.toml`. Packs are embedded and hash-pinned the same way
`docs/cpu-msr-contract.toml` is.

The suite reports pass or fail per check, and nothing softer. Dispositions above
pass/fail (GO, PROVISIONAL GO) belong to the program that runs the suite.

## The crate

The crate is `consonance/cpu-qualification`: a library plus one binary of the same
name. The portable core — check definitions, pack format, report format, floor
recomputation — compiles and is unit-tested everywhere. The measurement code is
Linux-only behind `cfg`, mirroring the gating in `vmm-backend/src/lib.rs`.

Three commands:

- `cpu-qualification run --stage <0..3> --baseline <name> --evidence-dir <dir>`
  runs one stage and everything below it.
- `cpu-qualification check --baseline <name>` re-runs stage 0 and compares the rows
  against the checked-in pack, exiting nonzero on any change. This is the run-window
  entry point.
- `cpu-qualification report --evidence-dir <dir>` recomputes every floor from the
  retained raw records and prints the verdict. Recomputation from records is the only
  path to a pass; a summary line is never an input.

## The known-chip table

The crate carries a static table, one entry per supported chip family, in the shape of
rr's `PerfCounters.cc`:

| Field | Example (Intel) | Example (AMD Zen) | Example (Neoverse N1) |
|---|---|---|---|
| vendor + family/model match | `GenuineIntel` 06_9EH | `AuthenticAMD` families 17h–1Ah | MIDR Neoverse N1 |
| work-clock event config | `0x1c4` | `0x5100d1` | `0x21` |
| expected PMU shape | arch perfmon v4 | legacy per-counter MSRs; PerfMonV2 where advertised | PMUv3, `BR_RETIRED` in `PMCEID1_EL0` |
| required host conditions | NMI watchdog off, governor pinned, SMT policy | the Intel list, plus SpecLockMap disabled on every core, SSB mitigation pinned, AVIC off | pinning policy per the ARM program |
| contract column | `docs/cpu-msr-contract.toml` | `docs/cpu-msr-contract-amd-draft.toml` | (none yet) |

A chip that is not in the table gets a refusal, with a machine-readable report of what
was found. The suite never guesses an event for unknown silicon.

The table says what a known chip should look like. Every entry is then measured, never
trusted: rr validates lightly at startup because its bar is a usable debugger, and the
bar here is exactness, so the table only selects which measurements run. Event configs
come from rr's production table and stay traceable to it. A value that departs from
rr's needs a recorded reason in the table entry.

## Stages

The stages are ordered by what they need, from a stock kernel up to the full patched
stack. `run --stage N` runs stage 0 through stage N. A stage that cannot run — a
missing capability, image, or patched kernel — reports what is missing and fails.
This is the `docs/TESTING.md` rule: hardware gates fail loudly, never skip silently.

### Stage 0 — host and chip check (minutes; root; stock kernel)

Stage 0 answers: is this the chip we think it is, and is the host in the required
state?

It reads the chip identity (vendor string, family/model/stepping, microcode revision)
and matches it against the known-chip table. It opens the work-clock event and
confirms it is pinned, non-multiplexed, and capable of guest-only filtering. It then
checks every required host condition from the table entry, on every core: MSR state,
SMT state, NMI watchdog, governor, `/dev/kvm`, and the loaded KVM module's identity
(stock or patched, by content hash). On AMD the MSR check covers SpecLockMap in
`LS_CFG` on all cores, with the SSB mitigation mode pinned so the kernel cannot
rewrite it. AMD also gets the probe rr uses: a `lock add` loop must move the retired
lock-instructions counter. If the counter does not move, the workaround is not in
effect, regardless of what the MSR read said.

The output is a set of expect-vs-found rows. Every row is confirmed or explicitly
dispositioned; a favorable deviation is still a deviation. For a full qualification
the rows must come out identical across two reboots.

### Stage 1 — counter measurement (hours; root; isolated core; stock kernel)

Stage 1 measures the counter itself. Four measurements and a discipline:

- **Count exactness.** Run payloads whose work-clock count is known by analysis —
  straight-line and looping payloads with counted branches — host-side and in a
  guest. The counter must match the analytical count exactly. The oracle is the
  analysis, never a second counter.
- **Overflow delivery.** Arm at least 10⁶ overflows. Each must be delivered exactly
  once: zero lost, zero duplicate, every sample accounted for in the records.
- **Skid.** Measure the overflow skid distribution across payload classes and
  periods. The pack records the observed maximum, the derived margin, and how the
  margin was derived. Overshoot past an armed deadline is an error the machinery
  must surface loudly, so the pack also states what happens when the distribution's
  tail is exceeded.
- **Save/restore fixpoint.** The full vCPU state, including the extended-state
  image, must survive save → restore → save unchanged.
- **Interference probes.** Repeat a fixed slice of the exactness run with a busy SMT
  sibling, a co-tenant on another core, and memory pressure. Counts must not move.

The discipline: before each long campaign, run one short slice, project the total
duration from its rate, and record the projection in the report.

### Stage 2 — mechanism and contract (hours; patched kernel)

Stage 2 exercises the pieces that need the patched kernel.

**Single-step exactness.** Exact step counts against an analytical oracle, with the
chip's semantics recorded in the pack. On SVM that is the trap-flag path, since there
is no monitor-trap facility.

**Force-exit landing.** At least 10⁶ armed deadlines through the patched module's
deterministic exit. Every landing must satisfy `work == target`, never overshoot, and
never exceed the stage-1 margin. Every run carries mechanism attestation: the patched
module's content hash, the exit reasons observed, and the patch markers. A stock
module must be structurally unable to produce a passing record.

**The classification sweep.** Enumerate the chip's advertised instruction and feature
surface — a CPUID walk on x86, an ID-register walk on aarch64 — and classify every
entry into the three rung-1 buckets: deterministically pure, must-trap, forbidden.
Must-trap entries are demonstrated on silicon (`RDTSC`, `RDRAND` and kin exit and are
serviced). Forbidden entries are demonstrated unreachable from the guest. The report
gives counts per bucket, and an empty bucket must be explained.

**Contract enforcement.** Execute the chip's contract column on silicon: frozen CPUID
reads back exactly, denied MSRs fault. On a chip whose column is still a draft, this
stage produces the ratification evidence.

### Stage 3 — determinism gate (hours; patched kernel; guest images)

Stage 3 is the end-to-end gate: at least 1,000 same-seed repetitions with bit-identical
full state hashes, over the payload matrix and a Linux guest with events injected at
seeded-random points.

One documented command from a fresh checkout must build the pinned stack, boot the
subject, and pass this gate. That command is part of the pack.

## Consumers

The suite integrates at four named points. Each is an edit in the consumer, not a new
mechanism.

The VMM refuses to run on an unqualified chip. The host-assert path
(`consonance/vmm-core/src/vendor/x86/hostassert.rs`) selects the pack matching the
live chip identity and refuses when there is none. Today an AMD host would silently be
given Intel's event config; the refusal replaces that.

Per-vendor constants come from the pack. That covers the work-clock event config,
today duplicated in `vmm-backend/src/arch/x86/mod.rs` and
`vmm-core/src/vendor/x86/work_perf.rs`, and the skid margin, today one global in
`vmm-backend/src/run_until.rs`.

Every hardware lane opens its run window with `cpu-qualification check`. The retired
det-cfl-v1 lane's gate script did this with a hand-rolled host-baseline function and a
module-size comparison; `check` replaces both for the next box that registers, and the
module-identity check works for `kvm_intel` and `kvm_amd` alike.

The `box.yml` qualification lane runs the suite. The ARM lane already names rung 1 as
its content; this crate is that content. Baseline names stay aligned three ways: the
pack filename, the `HostId` variant in `consonance/acceptance-suite/src/manifest.rs`,
and the fourth runner label.

## Port, do not rewrite

The measurement logic exists and has run on silicon. Port it from:

- the `spike/amd-epyc` branch: `spikes/amd-epyc/harness/` (the C probes for overflow
  delivery, skid, guest windows, force-exit, and single-step, already parameterized
  by event), `schemas/check-floors.py` (floor recomputation from raw records), and
  `host/posture.sh` (the AMD host-condition apply/attest/restore steps);
- the `spike/arm-altra` branch: `spikes/arm-altra/harness/`, the aarch64 halves of
  the same probes;
- `consonance/vmm-backend/tests/live_preemption.rs` and the task-07 skid apparatus,
  the Intel measurements that produced today's margin.

## Acceptance

The acceptance case is **the AMD box** (issues #180, #174, #179). The suite must
express the full restart program: `0x5100d1` as the work clock; the SpecLockMap probe
and MSR discipline as standing host conditions in the pack; skid re-measured from
scratch; the SVM force-exit patch with its cap-advertisement hunk; ratification of the
draft contract column; and an rr-parity check, meaning the event config, SpecLockMap
handling, and skid handling are each traceable to rr's source or carry a recorded
reason for the difference. The numbers that lead the report: 10⁶ overflows delivered
exactly once, 10⁶ landings with zero overshoot, 1,000 same-seed identical repetitions.

The second customer is **`msr1`**: the ARM box gets a pack, and the placeholder step
in its `box.yml` lane is replaced by the suite.

**`det-cfl-v1` is transcribed, never re-run.** Its machine is retired along with its
hardware lane. The pack is transcribed from the constants already in code and the
ratified contract; it is the record the consumer edits read, and the template the next
chip's pack follows.

## Order of work

1. Crate skeleton: pack format, report format, known-chip table, `report`
   recomputation. Portable, fully unit-tested.
2. Stage 0, plus `check`. The transcribed `det-cfl-v1` pack lands here; lane wiring
   waits for the next registered box.
3. Stage 1, ported from the spike harnesses.
4. Stages 2–3, plus the `box.yml` lane.
5. Consumer edits: the host-assert refusal, constants read from the pack.

Blocked items: record why, continue to the next.

## Reference anchors

- rr `src/PerfCounters.cc` — the known-chip table shape and the event configs.
- rr `scripts/zen_workaround.py` — the SpecLockMap MSR discipline.
- `perf_event_open(2)` — pinned, non-multiplexed, guest-only counting.
- Intel SDM vol. 3, performance monitoring — the Intel PMU model.
- AMD PPR for family 19h — the Zen PMU model and `LS_CFG`.
- Arm ARM D13 (PMUv3) — the aarch64 PMU model.
