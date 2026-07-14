# tasks/108 — box-gate results (PR #109)

**Head under test:** `72468d5` (`task/arch-boundary-restructure`).
**Baseline for the differential:** `c53dea9` (`origin/main`).
**Box:** i9-9900K, kernel 6.12.90 proxy, patched KVM (module 1400832) via
`box-window.sh`; one leased core, SMT sibling idle (`docs/BOX-PINNING.md`).
Window reverted to stock (1396736) and verified on close, both runs.

## Image pins — verified by content hash BEFORE boot (`hm-xdp` discipline)

| Artifact | sha256 (12) | Source |
|---|---|---|
| `bzImage` | `f06a34a79010` | the known-good pr44 build |
| `initramfs-postgres.cpio.gz` | `3c4a7f2f0db4` | the known-good pr44 build |
| `initramfs.cpio.gz` | `f0bb7c0da54e` | agreed across 3 independent copies |

Both runs used **byte-identical images**, re-hashed *after* copying into each
worktree. Note for the record: `/root/harmony-t81`'s postgres initramfs has
**drifted** (`82395d189e3b`) — exactly the mutable-canonical-path hazard `hm-xdp`
names. It was not used.

## Smoke (fired first, before spending the budget)

`live_determinism` on patched KVM — **PASS**. RDTSC reads `[0, 2, 4, 6]` (the
2-ticks-per-retired-branch contract clock), RDRAND `0x9f72a62a`, RDSEED
`0x42b87398` — the **seeded** stream, not host values. That is the restructured
`Exit::Arch(X86Exit::Rdtsc/Rdrand)` path surfacing and completing correctly on real
hardware, plus snapshot/restore clock transparency. The riskiest assumption held.

## Results — 6/8 pass on the head; both failures are PRE-EXISTING on main

| Gate | Expected (readiness table) | Observed on `72468d5` | Verdict |
|---|---|---|---|
| `live_m1_m2` | unchanged | ✅ rc=0 (5s) | pass |
| `live_linux_boot` | boots to `GUEST_READY` | ✅ rc=0 (4s) | pass |
| `live_preemption` | unchanged | ✅ rc=0 (6s) | pass |
| `live_snapshot_branch` | round-trips bit-identical | ✅ rc=0 (1s) | pass |
| `live_dirty_remap` | round-trips bit-identical | ✅ rc=0 (51s) | pass |
| `live_host_plane` | unchanged (`HP_VECTOR` now u32-parsed; identical for any x86 vector) | ✅ rc=0 (35s) | pass |
| `box_corpus` | same-seed `state_hash` identical, unchanged from main | ❌ rc=101 — **O1 PASSES**, O2 conformance FAILS on `insn-cpuid` | **PRE-EXISTING** (see below) |
| `live_nonquiescent_snapshot` | round-trips bit-identical | ❌ rc=101 — all 3 gates: restored VM resumes, `GUEST_READY=true`, terminal `Shutdown`, but `final_row=false` (the workload's tail row never prints — also absent from gate 2's *un-snapshotted* run) | **PRE-EXISTING** (see below) |

## The differential (authorized, one window) — `main` vs the head

### `box_corpus` — identical failure, identical digests ⇒ PRE-EXISTING

`main` reproduces the failure **byte-for-byte**:

| item | `main` (c53dea9) | head (72468d5) |
|---|---|---|
| `insn-rdtsc` | O1=PASS O2=PASS `1065ab4c…` | **identical** |
| `insn-rng` | O1=PASS O2=PASS `0fe06bf4…` | **identical** |
| `insn-cpuid` | O1=PASS **O2=FAIL** `cd321ad6…` | **identical** |

Same panic, same assertion (`box_corpus.rs:294`), same computed digest, same stale
golden (`746d8bb…`). The 20-pair repeat diagnostic reports **0 divergences** on both
trees, and `machine.state_hash` is `6163f110…` on **both** — the restructure
preserves the live state hash bit-for-bit.

**Root cause (pre-dates this PR).** Commit `9d60c75` (task 49 / PR #36) changed a
CPUID `eax` in `docs/cpu-msr-contract.toml` (`0x00000000` → `0x00000004`) and
`guest/golden/insn-cpuid.digest` was never re-blessed — it still dates to the
initial commit. `insn-cpuid` reports **raw CPUID values**, so its digest necessarily
moved; the V-time/PRNG-derived goldens (`insn-rdtsc`, `insn-rng`) were untouched by
a CPUID change, which is exactly why only this one item fails. The golden has been
stale **on main** since that merge.

Ruled out as a cause of this PR: the whole CPUID data path is byte-identical to main
— `dispatch_cpuid` is field-for-field the same (`eax→eax`, …), `kvm.rs`'s
`KVM_SET_CPUID2` marshalling has zero CPUID diffs, and the contract module's only
changes are the `include_str!` depth and two doc-comment renames.

**Not fixed here** (out of this PR's surface, and re-blessing a golden is a decision,
not a chore): file/flag a `DETCORPUS_BLESS=1` re-bless of `insn-cpuid.digest` on the
box, review the diff against the `9d60c75` contract change, and commit.

### `live_nonquiescent_snapshot` — identical failure on both trees ⇒ PRE-EXISTING

Gate 1 asserts the restored continuation prints `FINAL_ROW` (`row|20|407|20|3010`, a
hardcoded Postgres result row) **and** `GUEST_READY`. Observed on the head: the
restored VM **does** resume from the non-quiescent point, reaches `GUEST_READY=true`,
and terminates cleanly (`Shutdown`, 13036 steps, no step error) — only the workload's
final row is absent. That is a workload-tail assertion, the same family as `hm-xdp`
(the 2026-07-09 postgres-image rebuild breaking `live_materialization`'s tail).

**`main` fails IDENTICALLY — all three gates, same numbers ⇒ PRE-EXISTING.**

| | `main` (c53dea9) | head (72468d5) |
|---|---|---|
| seal point | step 99598, 56222 owned pages | **identical** |
| gate 1 restored continuation | `Shutdown`, 13036 steps, `final_row=false`, `GUEST_READY=true`, no step error | **identical** |
| gate 2 live (un-snapshotted) continuation | `Shutdown`, 13035 steps, `final_row=false` | **identical** |
| test result | `0 passed; 3 failed` | **identical** |

The **only** difference between the two trees anywhere in this gate is
`vm_state 20752 → 20754 bytes` — **exactly the +2 bytes of the v2 arch-tagged
header**. That is the deliberate step-4 change, appearing on real hardware precisely
where the readiness table said it would, with nothing else moving.

Note that gate 2's *live, un-snapshotted* continuation also lacks `final_row` — the
workload itself never emits the row on this image, so the snapshot/restore path is
not implicated at all. Root cause is the `hm-xdp` family (the Postgres image's
workload tail), and it reproduces on `main` with the same hash-pinned image.

**Not fixed here** (pre-existing, and out of this PR's surface): it belongs with
`hm-xdp`'s remaining work — retrofit the pin-by-content-hash discipline to the other
box harnesses and settle whether `FINAL_ROW` / the image's workload tail is the thing
to correct.

## Bottom line

Every gate that tests **what this PR changed** — the run loop, the two-level exit
dispatch, V-time/seeded completions, preemption, snapshot/branch/restore, the dirty
remap, and the host plane — **passes on real patched KVM**, and where the two trees
can be compared directly (corpus digests, `state_hash`, the 20-pair repeat) they are
**bit-identical to main**. The two failures reproduce identically on `main` with the
same pinned images, so neither is caused by this PR.

The two deliberate encoded-byte changes behaved exactly as the readiness table
predicted: no gate compared an absolute pre-task-108 `state_hash`, and every
snapshot-bearing gate (`live_snapshot_branch`, `live_dirty_remap`,
`live_nonquiescent_snapshot`'s round-trip) asserts *relative* (same-seed-twice /
restore-vs-fresh) equality, which holds.

## Box hygiene

Window released and KVM reverted to stock `1396736` (verified) after **both** runs.
Worktrees (`/root/harmony-t108`, `/root/harmony-t108-base`) and scratch scripts
removed on completion. Nothing else on the box was touched.
