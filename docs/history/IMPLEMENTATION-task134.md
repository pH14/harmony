# Task 134 — the cooperative maze gate: implementation record (M0 + M1 + M2)

**Bead:** `hm-cs5` (binding); M2 fix bead `hm-qcpp`. **Branch:**
`task/cooperative-maze-gate`. **Status:** M0 complete (portable), M1 complete to
the ruled boundary (live smoke green on the unblocked arm), **M2 unblocked** —
`hm-esfd` merged (#138, Option-C marker-clamped candidate seal) and the residual
static-deadline vacuity it exposed (`hm-qcpp`) fixed here with per-branch rolling
deadlines. See "The M2 record" below.

## What was built

- **`dissonance/maze`** — the workload crate: a pure-integer gauntlet
  (task 84's recommended maze; junction-reset semantics — a wrong door returns
  the walk to `(0,0)`). Correct doors derive from the manifest `maze_seed`;
  `reachable_cells` is exact (asserted against exhaustive closure); the
  random-plateau non-vacuity property is *measured*, at the campaign's
  per-rollout budget, not asserted in prose. Shared by the host toy and the
  guest agent, so the two cannot drift.
- **`dissonance/campaign-runner::mazecampaign`** — the campaign driver:
  `run_maze_campaign` through the two-barrier `DifferentialCampaign` with
  wire-v2 X/Y instrumentation (`MazeDeclaredMachine`, the task-132
  `DeclaredMachine` pattern verbatim: v1 guest catalogs upgraded in place,
  the toy's standalone v2 declaration prepended with a catalog-inclusive cut),
  same-`Moment` X/Y tuples nominated via `Nomination::EventMoments`,
  `MazeObservationCells` keying `(x, y)` at the actual `sealed_at`,
  best-Entry-per-cell occupancy, `RetentionProfile::Full` from rollout one,
  and the quiet reseed-only exploit move (`QuietCodec`, shared with SMB).
  Configurations (task 84's ruled trio, identical branch budget):
  `SelectorV1` = the archive-guided subject; `PureRandom` = frontier held
  empty (candidate cap + replay budget zeroed); `FrontierOff` = machinery on,
  selector never exploits. `MazeToyMachine` walks the real maze logic with
  the walker state in every snapshot — exploit branches genuinely
  return-then-explore.
- **`dissonance/benchmark`** — `ExplorationConfig::FrontierOff` (permanent
  diagnostic control) + the maze gate report (`MazeGateManifest` /
  `MazeGateReport`): subject-vs-pure-random strict beats on cells AND depth,
  a live control, and the maze's own non-vacuity gate (control median below
  the exact documented reachable frontier); goal witness derived from
  `depth == levels`.
- **`harmony-linux/maze-agent`** + `build-maze-image.sh` + `maze-init.sh` +
  `make maze-image` — the guest half: static-musl agent over the doorbell
  (flow-agent pattern), one entropy byte per step, X-then-Y state registers
  (a mid-tuple cut can never fabricate an unvisited deeper tile),
  `assert_reachable` on the goal edge, `setup_complete` as the base-seal
  SnapshotPoint. The agent prints `MAZE_SPEC … pace=N` then `MAZE_READY` on
  the serial; the box driver cross-checks the spec against its flags on every
  boot and refuses a mismatch.
- **CLI**: `campaign-runner maze mock|box` (mirrors `game mock|box`, minus
  ROM/billboard; plus the spec cross-check and the maze manifest emission).

## Portable gates (all green under nextest; macOS + Linux-target cross-check)

`maze` 7 tests · `benchmark` 52 · `campaign-runner` 232 (incl. the 5 maze
integration gates: two-barrier evidence path end-to-end with a persisted
nonempty ledger; same-seed ⇒ bit-identical outcomes for all three configs;
frontier-off ≡ pure-random on the toy (the machinery-neutrality tripwire —
now also under a live-shaped rolling deadline, M2 below);
archive-guided strictly out-reaching both controls on depth AND cells over 20
seeds with only it reaching the goal; retained-Entry restoration from a
reopened ledger with collection refused under the Full profile). clippy
`-D warnings` (host + `x86_64-unknown-linux-gnu` cross-target), fmt, deny,
public-api snapshots regenerated (additive). No `unsafe` in any workspace
crate touched (no Miri obligation; the guest agent's doorbell `unsafe` is the
established harmony-linux pattern, outside the workspace).

## The live M1 record (box `hetzner`, lease `t134`, leased core 2, 2026-07-21)

Runbook (from the repo, per boot):

```sh
ssh hetzner 'bash /root/box-window.sh acquire t134'      # patched-KVM window
# bundle-transfer the branch; on the box:
export PATH=$PATH:$HOME/.cargo/bin
( cd harmony-linux/linux && taskset -c 2 ./build-maze-image.sh )
taskset -c 2 cargo build --release -p campaign-runner --all-features
taskset -c 2 ./target/release/campaign-runner maze box \
  --config pure-random --max-branches 8 --repeat 2 --deadline-delta 10000000
ssh hetzner 'bash /root/box-window.sh release t134'      # revert + verify stock
```

**Calibration findings (both fixed on the branch):**

1. *Marker ordering*: `MAZE_SPEC` is the agent's line; init's pre-exec marker
   had to become `MAZE_LAUNCH` so a boot driven to `MAZE_READY` (now printed
   by the agent) always has the spec line on the serial. The first smoke's
   refusal was the cross-check working.
2. *The V-time stop grid*: the VMM stops the guest only at its interception
   grid (the pvclock delta-work; measured quantum 10,416,667 ns). An unpaced
   maze walk is hypercall-dense (~161 ns of V-time per step), so the smallest
   stoppable rollout crammed **62,106 steps** — flooding the bounded SDK
   capture (evidence truncated to the tail: `distinct_cells=1`, the goal edge
   lost) and quantizing every deadline. Fix: a deterministic fixed-count
   integer pacing spin in the agent (`--pace`, default **200_000**,
   box-calibrated: pace 60k ⇒ 173 steps/quantum ⇒ 200k ⇒ **52
   steps/quantum**), giving the maze the same V-time shape as the game's real
   emulation between emissions. Consequence for any future driver: rollout
   deadlines must be multiples of the grid quantum (`--deadline-delta
   10000000` = one quantum ≈ 52 steps).

**Smoke A — pure-random, 8 branches, `--repeat 2` (PASS, exit 0):**
52 steps/rollout weakest, 20 distinct cells, depth 4, repetition 2/2
bit-identical over the whole boot → seal → campaign pipeline, and the deep
trace retained under the *identical* content-addressed id in both repetitions
(`8345f46b9c8b…`). Log: `/root/t134-smoke3.log` on the box (sha256
`646d7ae4cc360e3620b339fcbabf426a186cbc9e794360eb964fd5f6e8b31709`); evidence
`/root/t134-evidence/smokeA/rep-{1,2}/`.

**Smoke B — selector-v1, 16 branches (BLOCKED, the expected class):** the
first candidate-seal materialization died `snapshot requested at a
non-quiescent point` (`MachineError::NotQuiescent` out of
`materialize_candidate`) — the **open `hm-esfd` P1** (candidate seals at
nominated moments lack a ruled quiescence strategy; naive retry-forward
overshoots staged reseeds). The maze reaches it through the EventMoments door
(candidate moments are SDK-emission boundaries). Escalated onto `hm-esfd`
with this evidence — **not** patched locally (the ruling "may a candidate
seal advance off its nominated Moment" is the integrator's).

## What is blocked, exactly

M2 (the ≥20-seed three-configuration live gate) needs candidate-seal
materialization for its **SelectorV1 subject and FrontierOff diagnostic**
arms — blocked on the `hm-esfd` ruling. The moment that ruling lands, M2 is
mechanical: the `maze box` driver, `--repeat` for the 25-run determinism
gate, per-config `--logs-out` accumulation, and `MazeGateReport` rendering
are all on the branch; portable analogs of every step are green.

## Findings escalated / filed

- `hm-19l0` (P2, filed): wire-v2 `MustHit` satisfactions never clear the
  absence view (`satisfies_must_hit` keys on the v1 verb, which v2 catalogs
  deliberately do not carry). The driver's `goal_hits` (host-side fold over
  the same committed evidence) is the authoritative progress witness until
  ruled; documented on `MazeCampaignOutcome::open_expectations`.
- `hm-esfd` (open P1, evidence added): above.
- The bounded SDK event capture truncates long rollouts silently (tail-only
  evidence) — with pacing the maze sits far inside the bound (~150
  events/rollout); noted here because any unpaced cooperative workload will
  rediscover it.

## Judgment calls (for review)

1. **Junction-reset over absorbing dead ends** (task 84 allows both): absorbing
   dead ends fill the frontier with permanently-dead entries that a uniform
   simple selector wastes ~40% of exploits on; reset keeps every retained
   corridor entry exploitable while random search still pays a full re-climb
   per wrong door (measured: 64 seeds × 48-step rollouts → median 1, max 4 of
   6 levels).
2. **The control mapping** (task 84's ruled definitions on the two-barrier
   controller): PureRandom = `GenesisSelector` + candidate cap/replay budget
   zeroed (frontier held empty); FrontierOff = `GenesisSelector` + full
   machinery. On the toy their logs are provably identical (asserted); on a
   live backend any divergence between them is a determinism finding.
3. **Subject = `SelectorV1`** (not `Signal`): the maze gate's subject *is* the
   simple archive-guided selector (DISSONANCE-STRATEGY: simple selector before
   advanced selection); `Signal` stays refused until an advanced-selector
   artifact exists.
4. **Workload-side pacing** over changing the shared pvclock/boot plumbing:
   no consonance or boot_server change; the pace is part of the workload
   manifest (rides `MAZE_SPEC` on the serial).

## The M2 record (`hm-qcpp`; box `hetzner`, lease `maze-qcpp`, core 2, patched-KVM, 2026-07-22)

`hm-esfd` merged (#138, Option-C marker-clamped candidate seal) unblocked the
SelectorV1 subject + FrontierOff diagnostic arms — but exposed a residual
**static-deadline × Option-C** vacuity (`hm-qcpp`): the rollout deadline was one
campaign-wide **absolute** Moment (`base_vtime + deadline_delta`); under Option-C
a candidate seals onto its planted marker, whose window `== deadline_delta`, so a
candidate can seal *at* the deadline. An exploit off it starts at-or-past its own
deadline → `run()` deadline-stops immediately → span 0 → the vacuity guard
(correctly) refuses the arm.

**Fix — per-branch rolling deadlines.** Each rollout runs
`branch_origin_moment + deadline_delta` (its own span budget from its own branch
point). Entirely in the maze layer's `MazeDeclaredMachine` wrapper: it learns
each snapshot's seal Moment, records the origin at each `branch`/`replay`, and
imposes `origin + delta` **only** on the quiet-arm rollout (the run carrying no
deadline — every seal / probe / setup run names an explicit `Some` deadline and
is forwarded verbatim). The campaign `until` now carries `deadline: None`. No
Option-C / reseed / `compose` / `materialize_candidate` / `seal_base` change; the
diff is two files under `dissonance/campaign-runner/`. Neutrality holds because
the deadline keys off the branch **origin** — the genesis-only controls stay at
`base_vtime + delta`; only SelectorV1 exploits (non-genesis origins) change.

Runbook: bundle-transfer the branch (`git bundle` + scp + clone; push is
classifier-blocked), reuse the M1 maze image verbatim (guest unchanged — source
parity verified byte-for-byte), rebuild only `campaign-runner`. Hold the
box-window lease in **one long-lived process** start→finish (box-window PPID
landmine: acquire as a *direct child*, never in a `$(...)` subshell, or the lease
is swept mid-run). Smoke-fire-once, then the arms, then release (reverts to stock
1396736 — verified).

### Box results (all arms `--repeat 2`)

| Arm | Config | Exit | `--repeat 2` | Weakest rollout span / steps |
|---|---|---|---|---|
| smoke | SelectorV1 4b @1e7 | 0 | — | 10 372 040 ns / 52 |
| **SV1 @1e7** | SelectorV1 16b @1e7 | **0** | **bit-identical** | **10 073 185 ns / 50** |
| SV1 @3e7 | SelectorV1 16b @3e7 | **1** | — | overshoot (see below) |
| PureRandom | PureRandom 8b @1e7 | 0 | bit-identical | 10 416 667 ns / 52 |
| FrontierOff | FrontierOff 16b @1e7 | 0 | bit-identical | 10 416 667 ns / 52 |

The SelectorV1 arm the vacuity guard refused pre-fix now **completes,
non-vacuous** (weakest rollout advances a full `deadline_delta` past its branch
point) and is **bit-identical** — at `1e7`, the config the box run used.
PureRandom ≡ FrontierOff work evidence *on the box* (identical span/steps) —
the machinery-neutrality tripwire holds live under the rolling deadline.

### `@3e7` — the open boundary (escalated to Paul, `hm-qcpp`)

`SV1 @3e7` fails **not on vacuity** (the fix works) but with `run overshot staged
Moment 237012404 (now at V-time 244341956); schedule unsatisfiable` — the
`hm-esfd` second-blocker (overshoot poison) class, but on the **rollout run**
rather than the candidate seal. Mechanism: an exploit env's `quiet_mutate` reseed
marker is planted at an **arbitrary** offset in `[1, window]` with
`window == deadline_delta`. At `1e7` the window is ≈ one pvclock intercept
quantum (measured 10 416 667 ns), so the marker always sits inside the single
grid step the rollout stops at, and is drained there. At `3e7` the rolling window
spans ~3 quanta, so a marker can land in a **later** quantum at a non-grid Moment;
the rollout run (`StopMask::NONE`, single deadline) crosses it without an
exact-arrival stop and the server rejects the schedule. Pre-fix `@3e7` avoided
this only by *truncating* exploits below their markers (the very vacuity being
fixed). The rolling deadline is thus **correct and safe for `delta ≲ one
quantum`** (the M1 finding already constrains deltas to grid multiples); `@3e7`
is a multi-quantum, out-of-regime config whose original purpose — a *headroom*
diagnostic — is moot under rolling deadlines (headroom is never used).

Resolving `@3e7` needs one of the reserved fallback directions (bound the marker
window to within one intercept quantum, decoupled from `deadline_delta`; or
nominate only post-marker candidates) or a determinism-core marker-clamp of the
rollout run — **Paul's ruling**, not a worker improvisation (the `hm-qcpp`
mandate). Distinct from `hm-x1ss` (the schedule-closure root cause) but adjacent.
