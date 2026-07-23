# Task 143 — deflake `streams_events_as_sse_frames` (telemetry SSE-frame timing)

**Bead:** hm-ftok (P2 bug). One flaky test intermittently reds the whole `gates` CI job.

## Problem

`consonance/telemetry/src/server.rs:680` `server::tests::streams_events_as_sse_frames`
panicked once on PR #138's gates run (1/2089, run `29887364236`, diff-unrelated), passed
on re-run. SSE-frame streaming is a timing-sensitive test class: the test presumably races
the server's frame flush against its read.

## Work

1. **Diagnose the actual race first** — read the test + the server's flush path and name
   the exact interleaving that can fail (don't just wrap it in a retry). If the panic
   message from the CI run is recoverable (`gh run view 29887364236 --log`), quote it in
   the write-up.
2. **Make the wait deterministic**: a state-based wait on the frame boundary (or a fake
   clock / explicit flush hook if the server side needs a seam), NOT a sleep and NOT a
   bare retry that can mask a real protocol regression. Conventions rule 4 applies —
   no wall-clock time in anything that affects assertions.
3. If a genuine server-side bug is exposed by the diagnosis (a frame that can be lost or
   split incorrectly, not merely observed late), STOP and report it — that would be a
   real telemetry bug, not a test problem.

## Acceptance

- The named race is documented in `consonance/telemetry/IMPLEMENTATION.md` (a short
  appended section) with the fix rationale.
- The test passes under stress: `cargo nextest run -p telemetry --all-features` plus a
  repeat run (`--no-fail-fast` with the test filtered, e.g. 200 iterations via
  `cargo nextest run -p telemetry -E 'test(streams_events_as_sse_frames)' --no-capture`
  looped, or nextest's retries=0 profile with a shell loop) showing 0 failures.
- Full portable gates green (build + nextest + clippy + fmt + deny). No behavioral
  change to the SSE wire format (bytes identical — if the fix touches server code, the
  existing frame-content assertions must be unchanged).

## Scope

`consonance/telemetry/` only. No other crates, no CI files.

## Environment

Mac-local only. No box, no Nimbus. Quick/narrow task.
