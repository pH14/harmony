# tasks/167 — hm-xkh5: the task-78 draw probe vs the entropy-stream timeline

Lane record for bead `hm-xkh5` (P1). Two load-bearing instruments disagreed
about whether the Postgres guests draw entropy in the task-78 hop/tail
windows. This lane reproduced the disagreement, ran the named experiment, and
resolved it.

## Determination: (a) — the draw probe has a false positive

**The trailing-reseed draw probe's hash mismatch on the Postgres hop-3 and
tail windows is not a draw.** The entropy stream is right; the probe was
wrong. A restored+reseeded branch does NOT draw where the live boot does not
— branch (b) is refuted by direct measurement.

### The chunk-diff evidence (the experiment named in the bead)

`dissonance/campaign-runner/tests/live_draw_probe_diagnosis.rs` (new, box-only
`#[ignore]`) replicates the chain protocol's per-hop draw probes in-process
against a real `ControlServer` (`ControlServer::handle` — the verb sequence is
byte-identical to the wire path) on the pinned pr44 Postgres pair, and diffs
`Vmm::state_components()` between each hop's plain leg and probe leg. Box run
2026-07-28, core 2, patched KVM (`/root/t167/diag1.log`); the instrument
reproduced the production gate's exact pattern (hops quiet/quiet/quiet/FIRES,
tail FIRES):

| window | probe | `vtim:entropy` | differing chunks | probe-only arrival landing |
|---|---|---|---|---|
| hop 0 `[442987686, 445256198]` | quiet | equal (`9e1fb946911491d5`) | none | — |
| hop 1 `[445256198, 447256516]` | quiet | equal | none | — |
| hop 2 `[447256516, 449281972]` | quiet | equal | none | — |
| hop 3 `[449281972, 451324095]` | **FIRES** | **equal** | `regs`, `RAM:16M..` | `[2042123]` (= window width exactly) |
| tail `[451324095, 452468139]` | **FIRES** | **equal** | `regs`, `RAM:16M..` | `[1144044]` (= window width exactly) |

On both firing windows: stop V-time, effective V-time, synchronized flag,
serial length, and idle landings are identical across the legs; the entropy
stream word sits at the branch seed and never moves. The registers differ
(hop 3: RIP `0xffffffff81971ee5` plain vs `0xffffffff81326e62` probe, plus
rflags/rax/rbx/rdi/rsp/r12/r14) and one RAM region differs. Cross-check: each
plain leg's stop state is bit-identical to the chain leg's own seal replayed
verbatim, so the plain legs reproduce the chain exactly.

### Mechanism

The probe env carries a trailing reseed marker at the landed boundary; the
server stages it and `run` arms the **exact-count arrival seam** at that
`Moment` (`control.rs` step 3 → `Vmm::arm_arrival`). The arrival freezes the
guest **between instructions at exactly the retired-branch count** — recorded
as the probe leg's lone preemption landing, at precisely the window width in
the restore-rebased work epoch. The marker-free plain leg is never clamped:
it reaches the same V-time at its first **natural** deterministic intercept,
a few (non-branch) instructions later in the same instruction stream. The two
stops are the same `Moment` with the same entropy stream but different
micro-positions, so `state_hash` differs and the probe reported a "draw" on a
window whose stream never moved. Hops 0–2 stay quiet because their probe legs
never take a forced landing (the guest reaches a natural intercept at the
staged count first), so both legs stop identically.

This is a probe-inference defect, not a substrate defect: the substrate never
claimed two stop mechanisms at the same `Moment` land on the same
micro-state, and the task-78 fold gates ((b)/(c)) compare like against like
(both sides carry the same markers), which is why they pass bit-identically
throughout.

### Smoke (fire-once, both instruments reproduced before any fix work)

- Gate (`/root/t167/smoke1-gate.log`): `draw probes (task 78): hops [false,
  false, false, true]; tail window DRAWS` — `GATES PASS` on pr44 defaults.
- Timeline (`/root/t167/smoke2-timeline.log`): stream parked at
  `5382e1a597a4908f` from `Run /init` (422419380 v-ns) through `GUEST_READY`
  (455499740 v-ns) — no draw anywhere in the Postgres phase, on the same
  boot composition (doorbell channels wired as `ControlServer::new` wires
  them).

## The fix (probe-side only; no substrate or gate-semantics change)

`campaign_runner::materialize`: **settle both probe legs one boundary past
the marker `Moment` before hashing** (`settle` — a `run` to `boundary + 1`
under `StopMask::NONE`). Past the marker nothing is armed in either leg, so
both stop by the same natural mechanism and the clamp residue converges; a
genuine draw still fires because the stream state itself differs after a
reseed-to-seed that followed k > 0 draws, and no later common-mode draw can
re-align the two legs. Draws inside the settle extension are common-mode
(with a draw-free window both legs draw identical values from identical
positions), so the probe still measures exactly `[origin, landed]`. The tail
probe re-runs its plain leg settled; `leg_hash`/`replay_hash` (the gate-(c)
anchors at the landing) are untouched.

### Proof — the positive/negative pair (`tests/live_draw_probe_pair.rs`, box)

Production probe (`materialize_client` over the real socket), both arms:

- **Fail-before (W1), pre-fix tree:** the pair test built against the
  UNFIXED `materialize.rs` — negative arm RED exactly as filed
  (`/root/t167/pair-prefix.log`, rc=101, `test result: FAILED. 1 passed; 1
  failed`): hops `[false, false, false, true]` + tail on the draw-free
  Postgres guest. The pre-fix bridge arm also fired on EVERY window (`hops
  [true, true, true]` + tail) including its two draw-free early hops — the
  artifact is pervasive wherever the boundary count lands mid-execution.
- **Negative arm, fixed:** pr44 Postgres, the gate's own default layout
  (HOPS=4 × 2 M v-ns + 1 M tail): `hops [false, false, false, false]`, tail
  quiet, chain gates (round-trip, reproducer) still green
  (`/root/t167/pair-postfix.log`, rc=0, `2 passed`).
- **Positive arm, fixed:** the `/dev/harmony` bridge guest
  (`initramfs-bridge.cpio.gz`, the first workload that draws seeded entropy
  on demand). Draw map measured first with the timeline instrument
  (`/root/t167/bridge-timeline.log`): `BRIDGE_LAUNCH` at 113084697 v-ns;
  stream moves at `BRIDGE_ENTROPY_RAW` (113187715) and `BRIDGE_ENTROPY_LIB`
  (113205213); halt shortly after `BRIDGE_DONE` (113278064). Chain based at
  `BRIDGE_LAUNCH` (3 × 32 k hops + 40 k tail, 5 k seal-retry step): the fixed
  probe reads `hops [false, false, true]` + tail DRAWS — quiet exactly on
  the two measured-draw-free early windows and firing exactly where the draw
  map puts the draws (pre-fix, every window fired). The fold gates hold
  bit-identically across a chain whose windows genuinely draw — the first
  non-vacuous hardware exercise of the task-78 reseed-aware fold property.
  (Absolute V-times differ between the two instruments — the pair's bridge
  genesis sealed at ~109.67 M v-ns vs the timeline's `BRIDGE_LAUNCH` at
  113.08 M — because the timeline wires the doorbell channels before its
  drive while the gate composition wires them at `ControlServer::new`, the
  documented F10 ordering; the draw layout is relative to the launch marker
  and the firing pattern confirms coverage.)

Production-gate evidence, fixed tree (the two repro commands, green/red as
the determination demands):

- `REQUIRE_DRAWS=0` (diagnosis escape, evidence only — the committed default
  is untouched): `hops [false, false, false, false]`, tail quiet, and
  **`GATES PASS`** — depth, round-trip, and reproducer all green with the
  honest probe (`/root/t167/gate-fixed-rd0.log`).
- Default `REQUIRE_DRAWS=1` (repro command 1): honestly RED at the draw
  precondition with the corrected guidance message
  (`/root/t167/gate-fixed-rd1.log`).
- Repro command 2 (the marker timeline) is untouched by the fix and green
  (`/root/t167/smoke2-timeline.log`).

Portable: nextest `-p campaign-runner` 182/182 (the loopback draw-probe pins
— draws=false script probes false everywhere, the draw-carrying script
probes true on every window, terminal tail reports false — all pass on the
settled probe), clippy `-D warnings`, fmt. The portable mock cannot express
the micro-position skew (its arrival clamp lands on the same scripted state
as a natural stop), so the fail-before direction is a box demonstration, as
recorded above.

## Consequence the fix surfaces (filed, not buried)

With the probe honest, **`REQUIRE_DRAWS=1` cannot pass on either Postgres
baseline at any window layout** — those images draw nothing after early
userspace, so the pre-fix `HOPS=4` green was the false positive itself. The
production gate at pr44/jul9 defaults is now honestly RED at its
`REQUIRE_DRAWS` precondition (every substantive gate — depth, round-trip,
reproducer — still passes; run with `REQUIRE_DRAWS=0` for a diagnosis-only
draw-free run). The gate's operator guidance now says exactly that and points
at the bridge pair. Per the lane spec this record does NOT rewrite historical
evidence claims: the task-78 "bit-identical even when entropy is drawn inside
a collapsed interval" box-evidence wording, the hm-2nt/tasks-157 "HOPS 3
fails / 4 passes" reading, and the task-68 gate's drawing-baseline question
are re-labeled via the follow-up bead **`hm-pwmd`** (docs-match-evidence
species, P1) filed from this lane, which also owns the design call on
re-homing the `REQUIRE_DRAWS` precondition onto a drawing baseline; the
honest drawing-baseline evidence now exists (the bridge arm above).

## Deviations considered and rejected

- **Two-seed probe** (branch the pair under different seeds, trailing reseed
  to a common seed): stop-mechanism-symmetric, but the SDK channel's hashed
  chunk seeds its own streams from the branch env's seed, so different-seed
  legs hash differently with zero draws — a structural false positive.
- **Substrate change** (make the arrival clamp land on the natural-stop
  micro-position): rejected as out of lane and wrong — exact-count
  between-instructions arrival is the seam's contract (task 59), and the
  fold machinery depends on it; only the probe's comparison was unsound.
- **Comparing at the boundary but hashing a component subset** (exclude
  regs/RAM): unsound — a real draw can legitimately move only RAM.

## Box hygiene

All runs on the leased core (`box-window.sh` lease `hm-xkh5`, core 2,
renewed through the session); box left at stock 1396736 with zero leases,
verified by hand on a fresh connection (see the bead's closing comment).
Evidence logs under `/root/t167/` on the box and archived in the session
scratchpad. The box worktree `/root/harmony-ibl2` (tasks/157 branch head
f379c3c) is left with this lane's four files applied uncommitted —
byte-identical to branch `task/draw-probe-disagreement` (sha256-compared) —
so the foreman's box verification of the incoming branch can reuse the warm
target tree; `git checkout -- dissonance/campaign-runner && git clean -fd
dissonance/campaign-runner/tests` restores it if unwanted.
