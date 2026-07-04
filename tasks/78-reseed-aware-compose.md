# Task 78 — reseed-aware compose: bit-identical folds under entropy draws

**Ruling source:** `docs/INTEGRATION.md` §6c ruling 3 (integrator, 2026-07-03), resolving PR
#58's escalated **sequential-entropy-splice** finding. Defensive posture: bit-identical
compose folds are a **requirement**, not a documented limit.

## The problem (as shipped in task 68)

`ControlServer::restore` reseeds the sequential entropy stream at every `branch`
(`SeededEntropy::new(seed)`), and `EnvCodec::compose` cannot express reseed points. A
compose-folded materialization (or a composed genesis-complete reproducer) is therefore
bit-identical to its hop-by-hop original **iff no RDRAND/RDSEED draw lands inside a collapsed
interval**. Pinned portably in
`dissonance/conductor/tests/materialize_loopback.rs::sequential_entropy_splice_diverges_a_collapsed_fold_documented_limit`
(a draw-carrying mock script: the two-hop leg reproduces itself; the fold diverges). Task 63
never measured a mid-chain reseed; task 68's live gates passed only because post-readiness
Postgres spans are draw-free. The moment a workload draws entropy mid-window, folded
reproducers silently stop replaying.

## The ruled fix: store the reseed points

The env format learns **reseed markers**: a branch's entropy reseed (seed value + the Moment
it took effect, i.e. the branch origin) is recorded in the `Recorded` env, `compose` splices
markers positionally exactly like overrides (blob-frame relative keys; the adapter's single
wire conversion re-anchors them, PR #58's "Coordinate frames" doc is the authority), and the
`ControlServer` **honors stored reseeds on `branch`**: replaying a folded env re-executes each
collapsed hop's reseed at its recorded position instead of reseeding once at the fold's root.

Requirements:

1. **`dissonance/environment`**: `Recorded` gains the reseed-marker table (deterministic
   container, integer keys; no floats). Version bumps per the crate's rules (EnvSpec blob +
   any goldens); public-api snapshot refreshed. `compose(b, d, cut)` splices markers with the
   same relative-cut arithmetic as overrides; `mutate` slices them consistently.
2. **`consonance/vmm-core` (`ControlServer`)**: `branch` with a marker-carrying env arms each
   reseed at its Moment (exact-arrival discipline, task 59's plane); a reseed staged beyond
   the trajectory is the same loud `ScheduleUnsatisfiable` class as a crossed fault. The
   no-marker path is byte-for-byte unchanged (goldens prove it).
3. **`dissonance/explorer` adapter**: `recorded_env` records the branch reseed into the blob
   frame (the inverse conversion, per the frame doc); `SocketMachine::branch` ships markers
   through the single conversion point.
4. **The pin flips.** The task-68 documented-limit test is replaced by its positive twin: the
   draw-carrying fold is now **bit-identical** to the hop-by-hop leg. Keep the old test name
   in a comment pointing here (the retire-with-escalation-note discipline from PR #58).

## Gates

1. Standard suite (fmt / clippy `-D` / nextest / deny) + refreshed goldens + public-api
   snapshots; the no-marker byte-identity golden is mandatory.
2. Portable: the flipped pin (draw-carrying fold bit-identical over the real wire —
   `SocketMachine` + `ControlServer<MockBackend>`), plus proptests (≥256) over random chains
   with draws in random intervals: fold == hop-by-hop, always.
3. **Box (FRONTIER)**: task-68's `live_materialization.rs` extended with a draw-carrying hop
   (guest issues RDRAND mid-window via the task-73 SDK entropy service or a raw RDRAND loop):
   folded + from-genesis re-materializations bit-identical to the hot seal on real KVM,
   pinned per `docs/BOX-PINNING.md`, stock-KVM revert verified.

## Non-goals

- Moment-keyed counter-mode entropy (the task-93 deeper option) — explicitly out of scope;
  this task makes the *sequential* scheme compose-safe.
- Any change to seed derivation or the entropy stream cipher itself.
