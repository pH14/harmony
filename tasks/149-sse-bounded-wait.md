# Task 149 — SSE phase-2 wait: bounded accumulating data-frame wait + keepalive-skip coverage (hm-38kv)

**Bead:** `hm-38kv` (P2, PR #149 discovery F1 family + F2 folded — `bd show hm-38kv`
first; the judge's repro details are in the PR #149 adjudication comment).
**Surface:** `consonance/telemetry` **test code only** — no server change.

## Fix (one structural closure)

Replace the unbounded, non-accumulating `while !frame.contains("data: ")` retry in
`streams_events_as_sse_frames` (server.rs:701-704) with ONE bounded **cumulative** wait
for a complete `data: …\n\n` frame that fails explicitly (printing the accumulated
bytes) when the budget expires. This closes all three confirmed members:
- F1a hang-on-regression / hot-spin-on-EOF (no attempt/deadline budget);
- F1b split-marker permanent hang (`: keepalive\n\ndat` — bytes after a frame
  terminator are discarded because `frame` is replaced, never accumulated);
- F1c premature-exit flake (a read ending exactly after `data: ` exits with an
  incomplete frame).

## Coverage (F2, folded)

Add a deterministic keepalive-before-data exercise of the skip path — today the
while-body executes zero times in every normal run (`KEEPALIVE_EVERY`(600) ×
`POLL`(5 ms) = 3 s idle before the first keepalive; the test emits immediately).
Make the keepalive arrive before the event deterministically (e.g. delay the emit past
one keepalive interval with a tightened test-local constant, or inject a comment frame —
pick the least invasive mechanism that stays test-only and does not slow the suite by
seconds).

## Gates

telemetry nextest full crate; clippy `-D warnings`; fmt. Stress: re-run the deflaked
test ≥500× (match the PR #149 record) and quote the count. No server, dependency,
wire-format, or public-API changes.

## Deliverable

PR from `task/sse-bounded-wait` closing `hm-38kv` with the merge. Minimal diff.
