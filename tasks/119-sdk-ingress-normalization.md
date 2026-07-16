# Task 119 — SDK ingress normalization + state-semantics declaration (`hm-bbx.1`)

First implementation child of the Differential observation/materialization migration
(epic `hm-bbx`, ruled by `docs/DISSONANCE-STRATEGY.md`; lane unblocked by Paul
2026-07-16 to run concurrently with the consonance work). Claim `hm-bbx.1` before
starting (`bd update hm-bbx.1 --claim`).

## Deliverable (from the bead, binding)

Implement the `sdk-events` data boundary for **both** LAYERS R-L3 ingress formats:

- Decode ordinary Antithesis assertions as **occurrence/property evidence**.
- Normalize numeric-guidance verbs **only to their explicit monotone extremum** — SDK
  watermark filtering cannot reconstruct arbitrary current state; do not pretend
  otherwise.
- Accept a **versioned workload instrumentation declaration** only for an emission path
  that reports **every required update**.
- Preserve **binary v1** identities and fired-event operations **without guessing
  never-fired reducers**; add a **binary wire-v2 declaration** carrying
  occurrence/state classification, value shape, and base update operation for the
  production cooperative path.
- Persist: declarations, normalized `SdkSchema`/`SdkEvent` serde, raw unknown data,
  ordering scope, and **typed errors**.
- **Keep assertion judgment out of the decoder crate** — the boundary decodes and
  normalizes; it does not judge.

## Context you must read first

- `docs/DISSONANCE-STRATEGY.md` (the ruling this implements) and `docs/LAYERS.md` §R-L3
  (the two ingress formats; the Antithesis SDK surface adoption).
- `dissonance/sdk-events/` as it stands (the GuestEvent→SdkEvent and catalog→SdkSchema
  renames landed in the vocabulary sweep; task 118's doc reconcile is current).
- The epic bead (`bd show hm-bbx`) for the coordinator contract this feeds.
- Known open issue `hm-ynt` (SDK event Moments are V-time-anchor lower bounds, not
  emission Moments) — do not silently fix or worsen it; note interactions.

## Rules

Conventions (`tasks/00-CONVENTIONS.md`) apply in full: determinism discipline (BTreeMap
or sorted iteration anywhere near hashes/serde; no float in state; total decode over
untrusted bytes — rule 4, fuzz-or-adversarial-property coverage for the new decoders),
scope isolation (your crate + its tests), typed errors, public-api snapshot updated
deliberately.

## Gates

Standard: build/nextest/clippy `-D warnings`/fmt/deny + property tests (≥256 cases) on
the codec/normalization laws; public-api snapshot. Mac-local; no box work.

## Definition of done

Both ingress formats decode to the normalized model with the constraints above; wire-v2
declaration round-trips; unknown data preserved raw; PR opened with the review-grounding
description (implementation write-up lives in the PR body per the hygiene ruling — no
docs/history file). `hm-bbx.1` closes on merge.
