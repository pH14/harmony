# Task 136 — hm-esfd: marker-clamped run-forward for candidate-seal quiescence

**Bead:** hm-esfd (P1). **Ruling:** Paul-adopted 2026-07-21 (Fable-consulted) — **Option C**: a candidate seal MAY advance off its nominated Moment to the first fully-drained quiescent boundary ("wait a beat, then snap"). Read `bd show hm-esfd` (full mechanism + the adoption comment) before coding. This unblocks TWO frontier verticals: the SelectorV1 exploit-seal path (tasks/133, merged) and the cooperative maze M2 (tasks/134, PR #137 draft).

## The problem
On a step that materializes a FRESH candidate seal (barrier 2), a quiet-reseed can shift an RNG exit to coincide with the candidate's V-time Moment → the guest is mid-RNG-exit → `snapshot()` refuses with `NotQuiescent`, and the client path aborts the whole campaign (`Err(e) => return Err(e)` at `campaign.rs:631`). The naive retry-forward also overshoots staged reseed markers because it picks deadlines *below* the staged Moment so the server's exact-arrival machinery never arms.

## The fix (per the Fable analysis — verify each claim against current code)
The remedy is **already the shipped contract** (`materialize_candidate` seals at "the first valid `sealed_at` at or after the candidate moment"; barrier 2 keys occupancy/CellFn/env at the ACTUAL seal) and **`seal_base` already implements run-forward-to-quiesce twice** (per the `ControlServer::run` "caller runs a little further and retries" contract). The candidate path just never copied it. Implement:

1. **Marker-clamped retry-forward in `materialize_candidate`** (dissonance/explorer/src/campaign.rs): on `NotQuiescent` (and `SnapshotWhileArmed` — they collapse to the same remedy, adapter.rs), retry: run a little further and re-attempt the seal, **clamping each retry deadline to `min(vt + step, next_staged_marker)`** — decode the next staged reseed marker from the branch env (the campaign holds the codec; `EnvSpec::Recorded::reseeds()`). Clamping lets the server's *own* already-reseed-aware exact-arrival machinery (`ControlServer::run`, control.rs) drain every marker — **zero vmm-core/server/codec changes**.
2. **Seal only once both schedules are drained** — the RNG exit completed AND no staged marker sits between the candidate Moment and the seal.
3. **Bounded attempts.** On cap-exhaustion, **drop the candidate like `NotSealable`** (`campaign.rs:630` — the disappearing-state posture), do NOT abort the campaign.

Mirror `seal_base`'s existing loop; keep it a client-side change.

## MANDATORY BLOCKING sub-gate (the one open determinism surface)
An entry sealed via retry-forward MUST re-materialize **bit-identically from its ledger env alone** — direct `run(deadline = sealed_at)` + seal, **zero probes** (the task-68/78 identity shape) — AND the drained marker must appear in the entry's recorded env at its exact Moment. This is the risk Fable flagged: the retry-forward's live history contains failed probes + extra deadline legs the ledger env doesn't record; if those legs perturb real-KVM state (dirty-tracking / pvclock side effects) the reproducer wouldn't reproduce. **If this gate FAILS: STOP and escalate** — the fallback (recording the retry schedule as an env rider) is heavier, blob-version territory, and a partial task-78 re-open; Paul decides before you go there.

## Acceptance
- **The two verticals' own gates** (box, x86 `hetzner`, smoke-fire-once): SelectorV1 8-branch cascade config (seed 7, explore-period 3, deadline-delta 2s) completing `--repeat 2` **bit-identical**; maze Smoke-B unblocking the M2 SelectorV1 + FrontierOff arms.
- **The blocking re-materialization sub-gate above** — retry-forward entry re-materializes bit-identically from ledger env alone.
- **Portable SealAnchor regressions**: a nominated-moment-non-quiescent toy and a staged-marker-beyond-candidate toy both seal-with-advance and replay bit-identically; a never-quiescent candidate dropped after the cap without campaign abort.
- **No task-63 re-cert, no task-78 fold re-proof** (markers/draws/compose untouched — confirm your diff touches none of that). Portable gates green.

## Environment
Mostly Mac-portable (the seal logic + SealAnchor toys). Box gates on `ssh hetzner` (x86, `/dev/kvm`): lease a core via `bash scripts/box-window.sh acquire t136`, pin per `docs/BOX-PINNING.md`, bundle-transfer, release when done. Guest artifacts/ROM per the merged task-132/133 setup. Open the PR after the portable fix + SealAnchor regressions are green; the box gates land on the same PR.
