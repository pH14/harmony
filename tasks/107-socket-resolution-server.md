# Task 107 — Promote the socket `resolution::Server` adapter to production surface

**Bead:** `hm-7j0` (P2). **Origin:** spine finding escalated from the NES game-workload
bring-up (task 86 M0, PR #93): film's live gate needed a `resolution::Server` speaking
the control socket and had to embed a **test-local** adapter, because the production one
promised by the task-58 client/server split (resolution = the second client of the
control server) was never built.

Read first: the test-local adapter in `dissonance/campaign-runner/tests/live_film.rs`
(its module doc records the judgment call), `dissonance/resolution`'s existing `Server`
trait/consumers, `consonance/control-proto` (the wire), and the task-58/task-59 control
server surface in `consonance/vmm-core/src/control.rs`. The design context is the
"resolution = second client; (Environment, Moment) is the address" investigation —
`docs/RESOLUTION.md`.

## Deliverable

1. **Promote the adapter**: a production socket-backed implementation of resolution's
   server-side seam living in the right crate (follow the dependency direction the
   test-local one already proved out — likely `dissonance/resolution` behind a feature,
   or a thin adapter crate if GLOSSARY/layering says so; if genuinely ambiguous, put the
   placement question in the PR description as a `[question]` rather than guessing).
   Use the ruled vocabulary throughout (GLOSSARY: Reproducer, Moment/Span — this code
   post-dates the rename sweep; no legacy names).
2. **Its own tests**: loopback/in-process socket tests covering the verb surface the
   film gate exercises (read/regs/exec-class observation verbs), plus the error paths
   (disconnect mid-verb, malformed frame → typed error, never a panic — trust-boundary
   rule 4 applies to every length/index off the wire).
3. **Re-point the film live gate** at the production adapter and DELETE the test-local
   one — the gate must not keep a private copy (that is the finding).
4. **Hash-neutrality**: observation verbs must stay observation — no draw-stream
   contact, recorded envs untouched (the PR-51/observation-inertness line of tests is
   the precedent; extend, don't weaken).

## Gates

- `cargo nextest run -p resolution -p campaign-runner` (+ any adapter crate), clippy -D
  host + `x86_64-unknown-linux-gnu`, fmt, deny, public-api snapshots for touched crates.
- New `unsafe`: none expected (pure socket/protocol code); if any appears it must be
  Miri-reachable per the standing discipline. If the new tests are Miri-viable (no real
  sockets under the interpreter — follow PR #99's `serve_speaks_frames` precedent and
  gate real-socket tests with a rationale naming the Miri-run sibling), say exactly what
  runs under Miri and what doesn't.
- The film live gate itself is box-only and the box is under the re-cert window — do
  NOT touch the box; the re-pointed gate's next live run rides the box's normal
  schedule. Portable proof: the gate compiles against the production adapter and its
  loopback shape is covered by the new tests.

Done = adapter in production surface with tests, test-local copy deleted, film gate
re-pointed, gates green, PR open with the placement judgment recorded.
