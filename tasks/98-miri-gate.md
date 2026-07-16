# Task 98 — Close the vmm-core Miri gate under the checked-in nightly workflow

Bead: `hm-4yj` (P1, bug). Filed 2026-07-11 from a quality review.

## Problem

The checked-in vmm-core Miri command in `.github/workflows/nightly.yml` fails **before
producing a UB verdict**: a control test reaches snapshot-store tempfile materialization
under Miri isolation and dies on the isolated filesystem op. The workflow currently grants
`-Zmiri-disable-isolation` only to the separate snapshot-store job, so the vmm-core job can
never complete. A Miri gate that cannot run is gate theater: the unsafe code it exists to
check is unwatched.

## Deliverable

Make the **exact vmm-core command from nightly.yml** pass on the pinned nightly, without
losing Miri coverage of the unsafe logic.

1. Reproduce on current main first: run the workflow's vmm-core Miri command verbatim and
   record the failure.
2. Repair via the test seam, a cfg boundary, or **narrowly scoped** `MIRIFLAGS` — whichever
   keeps the unsafe logic Miri-exercisable. Acceptable shapes: the offending control test
   gets a Miri-safe tempfile seam (preferred — a tested safe seam the host-only op hides
   behind); or the test is `#[cfg_attr(miri, ignore)]`d **only if** the unsafe paths it
   exercised remain covered by other Miri-run tests (show this, don't assert it).
3. Do **not** blanket-disable isolation for the whole vmm-core job and do not silently drop
   meaningful tests — this is gate correctness; the diff must argue why coverage is
   preserved.

## Gates

- The exact nightly.yml vmm-core Miri command passes locally on the pinned nightly.
- The crate implementation record (`scripts/IMPLEMENTATION.md` pattern / crate docs) states
  the command and any MIRIFLAGS with rationale.
- Standard portable gates: build, nextest, clippy, fmt, deny.
- Unsafe logic retains a Miri-exercisable path; any ignored host-only operation sits behind
  a tested safe seam.

## Notes

- Related context: memory records that snapshot-store was historically absent from the Miri
  job (task 95 M1 finding) — check whether the same gap class exists for other crates while
  in there; file beads for anything found, do not scope-creep the fix.
- Close `hm-4yj` on completion.
