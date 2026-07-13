# Task 104 — Restore conductor's full lib suite under Miri with a demonstrated budget

**Bead:** `hm-d4y` (P1). **Prerequisite reading (binding):**
`consonance/vmm-core/IMPLEMENTATION.md` task-98 section — this task applies the SAME
recipe (the hm-d8o treatment) to `dissonance/conductor`; the recipe is proven and its
gotchas are recorded there. Also `bd show hm-d4y` and the nightly.yml comments around
the `miri — conductor (UNSAFE-ONLY slice)` step.

## Problem

The full `cargo miri test -p conductor --lib` has NEVER completed in CI (interpreting
at 57+ min when a 90-min ceiling killed it; ~1h50m real on an idle Mac; unbounded under
box contention). PR #99 restored the crate's one `unsafe` block (the task-78
`CountingBackend::map_memory` forward, `src/mock.rs`) to nightly Miri as a 1.3s
UNSAFE-ONLY slice with a filter-rot guard — so the unsafe⇒Miri invariant already
holds. THIS task's deliverable is the rest: the full lib suite runnable under Miri
inside a demonstrated budget, restored to the nightly.

## Deliverable (the vmm-core recipe, applied)

1. **Profile first.** Run the lib suite under Miri with `--report-time` to rank the
   tail (ranking is valid, budgets are not — libtest's "finished in" under Miri
   isolation is a VIRTUAL clock ~2× real; every budget number must be timed
   externally with `time`. This gotcha is recorded in vmm-core's IMPLEMENTATION.md;
   do not re-learn it).
2. **Shrink test RAM under `cfg(miri)`.** The tail is sha256-dominated (state-hash
   over the `MEM` chunk scales with test RAM, ~2 s/KiB interpreted). Drop oversized
   test RAM constants to the smallest size covering the protocol constants they
   exercise — native runs must stay byte-for-byte unchanged (vmm-core's `BIG_RAM`
   `if cfg!(miri)` pattern).
3. **Gate the extreme unsafe-free tail** (`#[cfg_attr(miri, ignore = "...")]`), each
   with a rationale string that names (a) why it's safe to skip (pure safe code, no
   `map_memory` on the path — the unsafe forward stays covered by the slice test),
   and (b) which cheaper Miri-run sibling keeps its family covered. Never gate a test
   that reaches the unsafe forward in a shape the slice test does not.
4. **Proptests**: reduce cases under Miri or ignore with the restore-class rationale,
   per the vmm-core precedent (`pcfg(native)` pattern).
5. **Demonstrate the budget.** Time the exact prospective nightly command externally:
   locally first; then, ONLY IF the box is free of the nested-posture re-cert window
   (check with the foreman — do NOT touch the box while the N-3/metal re-cert runs),
   a box dispatch. If the box window isn't available before the task is otherwise
   done, hand off with the local measurement + the ~1.12× Mac→box scaling observed in
   PR #99, and the foreman schedules the demonstration dispatch.
6. **Restore the nightly step**: replace the UNSAFE-ONLY slice with the full
   `cargo miri test -p conductor --lib` step ONLY when its ceiling is measured-honest
   (the PR #99 derivation pattern: ~2.5× the quiet-hour wall, absorbing the observed
   2× contention). Keep the slice's filter-rot-guard idea if any filter remains.
   Update the removal comment and hm-d4y.

## Gates

- Native: `cargo nextest run -p conductor --all-features` byte-for-byte identical
  tallies (146 as of PR #104), clippy -D (host + x86_64-unknown-linux-gnu), fmt, deny,
  public-api unchanged (this task should add no public surface).
- The full conductor lib suite green under Miri on the pinned nightly
  (`nightly-2026-06-16`, `MIRIFLAGS=-Zmiri-permissive-provenance`), externally-timed
  wall figure recorded in IMPLEMENTATION.md.
- The unsafe forward (`mock_vmm_composes_maps_memory_and_ticks_per_exit`) still runs
  under Miri — never gated.

Done = suite green under Miri in a recorded budget + nightly step restored (or handed
off budget-demonstration-ready if the box window is unavailable) + hm-d4y updated.
