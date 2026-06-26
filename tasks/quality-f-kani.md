# Task quality-f — formal proofs on vtime arithmetic (Kani)

Read `tasks/00-CONVENTIONS.md` first. **Rule 1 waived** only to add `#[cfg(kani)]` proof
harnesses + a CI job + an IMPLEMENTATION.md section. You must NOT change non-test code.

## Dependency
**Requires `quality-a` merged.** Branch from updated `main`.

## Environment
Runs on: Linux (Kani is Linux-focused). Requires: Rust + `kani-verifier` (`cargo install
--locked kani-verifier && cargo kani setup`). If Kani cannot run on this worker's host, run
it via the box (`ssh <det-box>`) or document precisely why a gate did not run, per Convention
"a gate that didn't run is a gate that failed unless the integrator can see why."

## Context
`vtime`'s saturating `u128` arithmetic and `work_for_vns` ceil-division are small, pure,
integer functions whose "never panics / law holds for ALL inputs" claims are exactly what
bounded model checking proves — strictly stronger than proptest sampling. See
`docs/CODE-QUALITY.md`.

## Deliverables
1. Add `#[cfg(kani)]` proof harnesses to `consonance/vtime` for at least:
   - `VClock::vns` and `tsc` never panic and saturate correctly for all `(config, work)`;
   - the round-trip law `vns(work_for_vns(t)) >= t` holds in the reachable regime;
   - `VClock::new` rejection rules (`ratio_den==0`, `ratio_num==0`, pre-saturation) hold.
   Bound inputs where needed for tractability and DOCUMENT each bound.
2. Add a CI `kani` job (ubuntu): install `kani-verifier`, `cargo kani setup`,
   `cargo kani -p vtime`.
3. Add a "## Formal proofs (Kani)" section to `consonance/vtime/IMPLEMENTATION.md` listing each
   harness and its bound.

## Acceptance gates
1. `cargo kani -p vtime` passes (or, if unrunnable locally, passes via the documented path —
   state where it ran in IMPLEMENTATION.md).
2. The `kani` CI job is present.
3. `git diff` adds only `#[cfg(kani)]` harnesses, `.github/`, and `IMPLEMENTATION.md` — no
   change to compiled (non-kani) `vtime` code or its public API.

## Non-goals
Proving the stores (vtime arithmetic only this PR). Changing vtime logic.
