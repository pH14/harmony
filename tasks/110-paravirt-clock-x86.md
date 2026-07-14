# Task 110 — Paravirt work-derived clock, x86 implementation (ABI v1)

Bead: `hm-rk5` (P2). Implements `docs/PARAVIRT-CLOCK.md` (ratified-to-build x86-first by the
2026-07-13 pre-build ruling, `docs/ARCH-BOUNDARY.md` §Pre-build ruling). **The design doc is
the ruling authority** — layout, update discipline, closure story, gates, and kill conditions
all live there; this spec binds deliverables and sequencing. If implementation reality
contradicts the doc, stop and escalate for a ruling; never silently diverge (ABI details
freeze through this PR's review).

High-complexity task (vmm-core run loop + vtime + guest kernel + determinism argument):
**Fable 5** worker. Branch `task/paravirt-clock-x86`.

## Scope

x86 only. The page mechanism is built once and arch-neutrally where possible (the stamping
function is `vtime`-level and arch-blind), but guest-kernel integration, the registration
transport, and the live gates are x86; the ARM closure (§4.2, CNTKCTL/CNTHCTL posture,
arm64 clocksource) is validated at ARM spike stage AA-5 and is **out of scope** here beyond
not painting it into a corner. The RDTSC trap is **retained** underneath as backstop and
oracle — this task must not weaken or remove it.

## Deliverables

1. **The rename ride-along (first commit(s), mechanical).** `VClockConfig::{tsc_hz, tsc_base}`
   → `{guest_clock_hz, guest_clock_base}`, `VClock::tsc()` → `VClock::guest_clock()`, and the
   `vm-state` `VtimeState` mirror in lockstep, per §5's table. Naming-only: byte layouts and
   arithmetic unchanged (prove with existing goldens). NOTE: tasks/108 already renamed the
   env-blob side (`vtime tsc_hz/tsc_base → guest_hz/guest_base`, `VClock::tsc → guest_ticks`
   per its C-list) — reconcile with what is actually on main at branch time and finish
   whatever residue §5 still names; do not re-rename what 108 already landed. No
   "formerly X" comment residue (standing ruling 2026-07-13).
2. **Page stamping + refresh discipline in the run loop** (`vmm-core`, engine level where
   arch-neutral, vendor/x86 where not): the §1 seqlock write protocol; refresh at all four §2
   points (natural exits, pre-injection deadline landings, idle warps, the Δ staleness-bound
   forced refresh via `run_until_overflow`). Δ is a config knob with a documented default.
3. **Registration transport**: guest publishes the page GPA via the hypercall doorbell or a
   contract-reserved MSR (§3.1 leaves the choice to this task — pick one, record why in the
   PR description); vmm validates the GPA lands in guest RAM and begins stamping. A guest
   that never registers gets exactly today's behavior (page is pure opt-in).
4. **Seal/snapshot canonical re-stamp** (§1.1): at every seal quiescent point the page is
   re-stamped to canonical form (`seq = 0`, values at the exact seal work count). The page is
   guest RAM — no new `vm-state` section, no `VM_STATE_VERSION` bump.
5. **Guest kernel clocksource** (§3.1): `CONFIG_HARMONY_PVCLOCK` kvmclock-shaped clocksource
   with the interpolation deleted (`.read()` = seqlock page load), TSC clocksource made
   unselectable for kernel timekeeping. Applied to the canonical guest kernel port
   (6.18-series patches; 6.12.90 box proxy for live validation, per the task-57 precedent).
6. **Reachability gate, x86 half** (§3.3): static `rdtsc`/`rdtscp` opcode scan of the built
   guest kernel image wired as a build gate (the LL/SC-scan discipline transposed). The
   W^X/rescan-on-exec runtime half may be specced-and-stubbed if it needs contract work —
   say so explicitly rather than faking it.
7. **Gates G1/G2/G3 + the N-4 perf measurement** (§6) as runnable harnesses, not prose:
   - G1 same-seed bit-identical `state_hash` with the page on (box, real KVM);
   - G2 page-stamp == RDTSC-trap-oracle function-equality at every refresh Moment
     (NOT whole-hash equality across page-on/page-off — §6 rules that out);
   - G3 busy-wait liveness within Δ;
   - perf: RDTSC-exit rate page-off vs page-on, boot ratio, det-corpus + postgres campaign
     smoke, reported as ppm-style ratios. Kill condition 3 threshold (<2× reduction =
     "not worth it") reported honestly either way.

## Sequencing & box discipline

- Portable first: stamping function, seqlock protocol, canonicalization, and the planner/Δ
  arming logic are Mac-testable (mock backend + Miri for any new unsafe). Box needed for
  guest-kernel build + G1/G2/G3/perf on real KVM.
- **Box window contention**: the nested-x86 re-cert (PR #98 chain) has box priority; take
  the box only in a foreman-granted window, pinned per `docs/BOX-PINNING.md`.
- **Smoke-fire-once** before any long gate/perf run: a minutes-long probe of the riskiest
  live assumption (guest registers page + reads sane time + G3 doesn't hang), reported
  before spending the full budget.
- Evidence-integrity bar applies (the PR-98/PR-108 species): no gate may pass vacuously —
  G2 must fail if stamping diverges (prove with a deliberate-fault test), G3 must fail on a
  frozen page, the opcode scan must fail on a planted `rdtsc`.

## Done means

All seven deliverables landed; G1/G2/G3 green on the box; perf deltas measured and reported
against kill condition 3; determinism gates on main unaffected (page off = byte-identical
behavior); review clean including the mandatory cross-model pass.
