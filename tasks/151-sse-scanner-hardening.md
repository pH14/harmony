# Task 151 — SSE test-infra hardening: generic scanner + scripted-read coverage + header anchor (hm-b5km, hm-8c5m)

**Beads:** `hm-b5km` (P2, PR #152 F-B — the umbrella) and `hm-8c5m` (P2, PR #152 F-A —
folds in per the adjudication). `bd show` both first. **Surface:**
`consonance/telemetry` **test code only** — no server change.

## 1. `hm-b5km` — genericize `read_sse_data_frame` over `io::Read`

The helper (server.rs:568) currently takes `&mut TcpStream` + `set_read_timeout`, which
blocks scripted reads; its cross-read accumulation is therefore untested (judge-checked
mutation: clearing `acc` between reads stays green everywhere except adverse
interleaving). Genericize over `io::Read` (timeout setup moves to the caller) and add
scripted-read tests asserting:
- cross-read retention: a read returning `": keepalive\n\ndat"` followed by
  `"a: hello\n\n"` yields the complete data frame;
- budget exhaustion: a no-data stream panics with the accumulated bytes.

Optional while reshaping (PR #152 F-E, judge-sanctioned opportunistic): the
`split_inclusive` form of the frame walk — take it only if it stays net-simpler.

## 2. `hm-8c5m` — phase-1 anchor (one token)

In `streams_events_as_sse_frames`, anchor the phase-1 header wait on `"\r\n\r\n"` (the
full header terminator) instead of `"text/event-stream"` (server.rs:793) — both
`head.contains` asserts still hold. Closes the residual-header-tail hole given the
emit-after-phase-1 ordering.

## Gates

telemetry nextest full; clippy `-D warnings`; fmt; stress the integration test ≥200×
and quote the count. No server, dependency, wire-format, or public-API changes.

## Deliverable

PR from `task/sse-scanner-hardening` closing both beads with the merge. Minimal diff.
