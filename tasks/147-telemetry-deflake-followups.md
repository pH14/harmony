# Task 147 — Telemetry deflake follow-ups: SSE wait anchor, helper altitude, doc trim (hm-3r2k, hm-gfi2, hm-gnxr)

**Beads:** `hm-3r2k` (P2), `hm-gfi2` (P2), `hm-gnxr` (P3) — the PR #146 review parks.
Read each with `bd show <id>` before coding. **Surface:** `consonance/telemetry` only
(tests, one helper, IMPLEMENTATION.md). Small, narrow, mechanical — no behavior changes
to the server outside what is listed.

## 1. `hm-3r2k` — anchor the phase-2 wait on `data: `

The deflaked test `streams_events_as_sse_frames` waits for stream bytes in phase 2, but
a keepalive/comment frame or a header tail can satisfy the current wait before the first
real event frame arrives, re-opening a (narrower) race. Anchor the wait on the actual
event payload marker (`data: `) so the assertion phase starts only once a genuine event
frame is present. Keep the subscribe-before-announce structure from PR #146 intact.
Re-run the test in a loop long enough to demonstrate stability (the PR #146 record used
repeated nextest runs — match or exceed that count and quote it).

## 2. `hm-gfi2` — `announce_and_stream` helper altitude

The helper introduced in PR #146 has exactly one caller. Per the bead: either inline it
at the call site OR generalize it as the substrate other telemetry tests build on.
Decision rule: if no second caller exists in-tree today, **inline it** — do not build
speculative generality. Record the choice in one sentence in IMPLEMENTATION.md.

## 3. `hm-gnxr` — IMPLEMENTATION.md amendment

Compress the PR #146 deflake section: drop the unsupported claim the bead names (read
the bead for the exact sentence), keep the subscribe-before-announce rationale and the
flake-reproduction evidence. Net length should go down.

## Gates

`consonance/telemetry` nextest (full crate), clippy `-D warnings`, fmt. No dependency
changes, no wire-format changes, no public-API changes.

## Deliverable

PR from `task/telemetry-deflake-followups` closing all three beads with the merge.
Keep the diff minimal — this is cleanup, not redesign.
