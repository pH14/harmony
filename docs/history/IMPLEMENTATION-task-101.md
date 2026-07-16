# IMPLEMENTATION — task 101 (paravirt work-derived clock: design spec)

Doc-only task (bead `hm-8h8`). Output: `docs/PARAVIRT-CLOCK.md`, the ratifiable design spec for
routing guest time reads through a work-derived paravirtual clock page. No code; a separate bead
implements after ratification. Filed as `docs/history/IMPLEMENTATION-task-101.md` per the
`IMPLEMENTATION-task-93/94.md` precedent (a docs-level task has no crate directory).

## What the spec rules

- **Layout** — a 4 KiB seqlock-versioned guest page, ABI `HARMONY_PVCLOCK_ABI = 1`, carrying
  **materialized** `vns`/`guest_clock` values (not kvmclock's base+live-delta). Fields, widths,
  update ordering, single-vCPU torn-read guard, and its place in the state hash (it *is* guest
  RAM → already hashed; canonicalized at seal so it carries zero refresh-history entropy).
- **Update discipline** — refresh at the four V-time advance points, enumerated against the
  actual `consonance/vtime` seams: natural `run_until` exits, `TimerQueue::pop_due` deadline
  landings (re-stamp *before* injection), `IdlePlanner`/`advance_idle` idle warps, and a
  staleness-bound forced PMU-overflow refresh (`run_until_overflow`) that keeps a busy-wait-on-
  time guest live and is the perf story (one exit per Δ-work window vs one per RDTSC).
- **Per-vendor closure** — x86: page + retained RDTSC/RDTSCP trap (backstop *and* oracle);
  ARM: page + contract-level `CNTVCT`/`CNTPCT` denial, ECV recorded as a probed fast-path never
  a dependency (no reachable ARM server chip has it).
- **Migration/rename** — `VClock::tsc()`→`guest_clock()`, `VClockConfig::{tsc_hz,tsc_base}`→
  guest-clock naming, ride-along per `docs/ARCH-BOUNDARY.md` §C.3; page fields mapped; restore is
  a no-op beyond re-stamping because the page holds absolute values.
- **Validation** — G1 same-seed bit-identical (page on), G2 page-stamp-vs-RDTSC-oracle (x86),
  G3 resolution/liveness, plus N-4 perf deltas.
- **Kill conditions** — unclosable page/injection ordering (both), ARM reachability escape
  (sharpest, no trap underneath), x86 staleness/perf collapse (kills only the *optimization*
  rationale, not ARM correctness).

## Key design decision (and the alternative rejected)

**Materialized value vs kvmclock-style interpolation.** The load-bearing choice is that the
guest reads a *finished* number from the page and does **no** arithmetic against a live counter —
as opposed to kvmclock, where the guest still executes `rdtsc`/`CNTVCT` and interpolates. The
interpolated form was rejected because it reintroduces a guest-side live-counter read, which is
exactly the untrappable `CNTVCT` on non-ECV ARM (correctness-fatal) and a determinism hazard on
x86 (defeats the purpose). Materialization costs *resolution* (piecewise-constant clock between
refreshes) — deterministic and monotonic, with the staleness-bound refresh (§2.4) as the
liveness backstop. It also makes snapshot/restore of the page trivial (absolute values don't
reference a counter origin), which is a second, independent argument for it (§5).

## Internal-consistency check (the gate)

Every mechanism claim is cited to a real seam. Spot-verified against the tree at this branch:

- `consonance/vtime/src/clock.rs` — `vns`:74, `tsc`:85, `advance_idle`:131, snapshot/restore
  carrying V-time in `vns_base`:135, `snapshot_vns`:147, `VClockConfig`:12.
- `consonance/vtime/src/planner.rs` — `CpuBackend::work`:16, `run_until_overflow`:25,
  `stop_at`/`ReadyToInject`:119.
- `consonance/vtime/src/idle.rs` — `IdlePlanner::plan`/`advance_idle`-only-touches-`vns_base`:102.
- `consonance/vtime/src/queue.rs` — `pop_due`:106.
- `consonance/vtime/src/lib.rs` — work = retired counted branches:6/24; the two V-time-without-
  work events:30.
- `consonance/vmm-backend/src/kvm.rs:520` + `exit.rs:164` (`deterministic_tsc`) — the retained
  RDTSC trap the x86 oracle uses.
- `consonance/vm-state/src/types.rs` — `VtimeState` mirror:148, `ratio_den==1` snapshot rule:152,
  `Halted` seal quiescent point:118; `lib.rs:69` `VM_STATE_VERSION`.
- Hardware facts from `docs/ARM-PORT.md` (ECV table:30, three mechanisms:41, `BR_RETIRED`,
  LL/SC:60, missed-PMI #3607:84); N-4 sizing recommendation `docs/NESTED-X86.md:282`; seam +
  rename ruling `docs/ARCH-BOUNDARY.md` (§B engine/vendor, §C.3 rename, :46 the Hz-scaled-counter
  observation).
- "vendor" terminology throughout (never "personality"); "V-time" kept as the mechanism name;
  `Moment`/`Span` used for points/durations per `docs/GLOSSARY.md`.

## Deviations considered and rejected

- **Reuse kvmclock's exact ABI** (wire-compatible). Rejected: our slope is work-derived and our
  value materialized; wire-compat would force the base+delta form we specifically forbid. We
  borrow the seqlock *shape* only and say so prominently (spec §0).
- **Carry the page as a new `vm-state` section.** Rejected: the page is guest RAM at a fixed GPA,
  already inside the memory image and its hash/dirty-log — a section would double-count it. Only
  the (renamed) `VtimeState` record stays a section.
- **Put IMPLEMENTATION.md at repo root or in a crate dir.** N/A — no crate; follows the
  `docs/history/IMPLEMENTATION-task-NN.md` precedent.

## Known limitations / what the integrator (and the follow-on impl bead) must know

- **Not ratified.** This is a spec for review, not a merged contract. The page GPA, the exact
  registration transport (hypercall doorbell vs reserved MSR), and Δ (the staleness window) are
  left as implementation choices for the follow-on bead — all named as already-modeled seams.
- **`VClock::guest_clock` is a *proposed* rename**, not present in the tree. Every use in the
  spec is flagged "(renamed §5)"; the arithmetic is `VClock::tsc` unchanged.
- **Task 100 (`docs/ARM-ALTRA.md`, `hm-x8g`) is the validating consumer** — it boots a guest
  whose only clocksource is the page and proves G1/G3 on real N1 silicon plus the kill-condition-2
  reachability ruling against a real guest image. The two docs cross-reference; neither
  duplicates the other (spec §8).

## Gates

Doc task: no cargo gates. The gate is internal consistency with the vtime seams (cited above,
files/lines) and ARM-PORT's hardware facts (cited), "vendor" terminology, and cross-reference to
`hm-8h8`↔`hm-x8g`. PR on `task/paravirt-clock-spec`; foreman review; close `hm-8h8` on merge
(foreman-owned).
