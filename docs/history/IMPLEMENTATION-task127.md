# Task 127 — Capture seal evidence cuts across the VM control seam (`hm-bbx.6`)

Branch `task/seal-evidence-cuts`. The keystone child of the Differential-migration epic
(`hm-bbx`): bind every successful production seal to its exact **evidence cut** — the
server-stamped `(Moment, included SDK-event count)` pair — and carry that stamp through the
control protocol, the `Machine` seam, snapshot metadata, pending forks, and persisted
lineage, with **no second read as authority**. Capture + transport only: no SDK payload
decoding, no Differential relations, no reduction, no archive-occupancy change.

This file is the review-grounding write-up (multi-crate task ⇒ no single crate
`IMPLEMENTATION.md`); it is intended to be lifted into the PR description.

## What changed, layer by layer

### `dissonance/control-proto` (the wire)

- **`Reply::Snapshot` is now the ONE reply to `Request::Snapshot`**, tainted or not:
  `{ id: SnapId, at: Moment, sdk_events: u64, tainted: bool }` — handle, synchronized seal
  `Moment`, included SDK-event count, and taint, bound atomically. The cut is **half-open by
  prefix length**: SDK-capture positions `< sdk_events` are included (including the exact
  subset emitted *at* the seal's `Moment`), `>= sdk_events` excluded — never a `Moment`
  comparison (several events share one stamp; see `hm-ynt`).
- **`Reply::SnapId` (wire tag 2) is retired** — it carried a handle with no cut, so it could
  not honor the binding; the tag is reserved-never-reuse and the decoder rejects it
  (`retired_snapid_tag_is_rejected`). Keeping it decodable would have let a stale/hostile
  peer smuggle a cut-less handle past the contract.
- **`APP_PROTOCOL_VERSION` 7 → 8.** This is a *reshape* (the tag-10 body changed), not an
  addition, so a v7 peer must reject at `hello` rather than mis-decode mid-session.
- The count is `u64` (the persisted-vector coordinate), not the paging verb's `u32` offset —
  no truncation question on the contract value.

### `consonance/vmm-core` (the server)

`ControlServer::snapshot` stamps the cut from the **same stopped state** the seal captures:
`at` = the sealed `vm_state`'s own `vtime().snapshot_vns` (exactly the floor a later restore
validates against — not a second `effective_vns` read), `sdk_events` = the SDK capture
vector's length. Nothing advances the VM between the seal and the reply (one verb, one
thread), so handle/Moment/count/taint are one atomic observation. Every error path
(`NotQuiescent`, `SnapshotWhileArmed`, `ScheduleUnsatisfiable`, …) returns neither a usable
handle nor a cut, and mints nothing.

### `dissonance/explorer` (the transport)

- `Machine::snapshot` returns `(SnapId, EvidenceCut)`; `EvidenceCut { at, sdk_events }` is
  the spine's local mirror of the wire cut (serde, `Copy`, `Ord`).
- `SocketMachine` records the stamp verbatim into `SnapMeta` and **uses the stamp as the
  branch re-anchor origin** (previously the client's own `pos` bookkeeping — the one place a
  client-derived value stood in for the server's floor). `snapshot_cut(SnapId)` exposes the
  stored stamp with no wire traffic. A `tainted: true` reply is refused loudly (the campaign
  machine never improvises; a tainted seal means the session was improvised out from under
  it).
- The cut rides `PendingFork` → `VirtualExemplar.cut` (persisted frontier) → `Lineage.cut`
  (persisted lineage) — replacing the bare `at: Moment` field in both, so the full pair
  travels or nothing does.
- **Re-materialization re-verifies the stamp**: after the landed-`Moment` check, the
  re-sealed cut must equal the entry's recorded cut or `materialize` fails with the new
  `MachineError::CutDivergence` (fresh handle released, nothing cached). Determinism makes
  the replayed state re-stamp identically; a difference is a determinism/transport
  violation to escalate, never a stamp to overwrite.
- The **toy machine stamps a non-trivial cut** (its answer-log prefix as the stand-in
  ordered capture), so every existing engine gate — determinism, replay, GC, eviction
  safety, materialization folds — now exercises cut transport and the divergence check
  implicitly.

### Downstream (mechanical transport of the same reshape)

`campaign-runner` (toy machines stamp zero-SDK cuts; `materialize.rs`'s `seal_here` returns
the stamp and keys lineage/exemplars on it rather than its client-side V-time cursor;
`record.rs` accepts only an untainted seal-bound reply), `resolution` (consumes handle +
taint, deliberately does not surface the cut — an investigation client, not the campaign
plane), `sdk-events` (test literals), and the vmm-core Linux live tests. `docs/GLOSSARY.md`
records the ruled `cut`/`EvidenceCut` vocabulary (ratified by `docs/DISSONANCE-STRATEGY.md`;
the entry records the type name per the strategy header's same-change rule).

## Acceptance criteria → tests

| Criterion | Test(s) |
|---|---|
| Goldens cover tainted AND untainted cut-carrying snapshot replies | `control-proto/tests/golden.rs`: `reply_snapshot_untainted_carries_the_cut`, `reply_snapshot_tainted_carries_the_cut` (hand-written bytes) |
| Hostile decodes | `control-proto/tests/adversarial.rs`: `snapshot_reply_hostile_bodies_are_rejected` (every field-boundary truncation, non-canonical taint byte, trailing bytes), `retired_snapid_tag_is_rejected`; plus the existing arbitrary-bytes / mutation / truncation properties now generating the new shape (`tests/common`) |
| Same-Moment fixture: before-seal included, after-seal excluded, by prefix length NOT Moment | `vmm-core control::tests::same_moment_fixture_cuts_by_prefix_length_not_moment` — constant scripted work puts two doorbell events and two seals all at ONE stamped Moment; the seals' cuts differ only in count (1 vs 2); events have identical id, payload, and stamp, so position is the only discriminator |
| Branch/replay preserves the captured SDK prefix length | `vmm-core control::tests::branch_and_replay_preserve_the_captured_sdk_prefix_length` (verbatim replay AND reseeding branch restore the prefix; the re-seal re-stamps the identical cut); explorer-side: `CutDivergence` re-verification in `Materializer::materialize` + `materialization::cut_divergence_is_loud_and_releases_the_fresh_seal` |
| Cut identical across same-seed runs and platforms | `vmm-core control::tests::the_cut_is_identical_across_same_seed_sessions` (bit-identical cuts/captures/console across two sessions); platform half: the suite is wall-clock/iteration-order free and runs on macOS (here) + Linux (CI `gates` job); Linux compile proven by cross-target clippy (below) |
| Failed/non-quiescent seal returns neither handle nor cut | error replies carry no cut structurally; minted-nothing assertions added to `snapshot_while_a_fault_is_staged_is_rejected` (handle contiguity across the refusal) and `snapshot_at_an_unsynchronized_point_is_not_quiescent` (no wire handle exists) |
| Server stamp carried without a second read as authority | `explorer adapter::tests::snapshot_records_the_server_stamped_cut_as_the_sole_authority` — the script stamps `at` ≠ client `pos` and asserts the wire `Branch` re-anchored on the STAMP; `snapshot_cut` is metadata-only; `engine_pins::admitted_entries_and_lineage_carry_the_stamped_cut` pins frontier + lineage carrying the stamp verbatim |
| Console bytes cannot enter the SDK count | structurally: the serial capture has no cursor in the cut; tested in the same-Moment fixture (serial bytes before and after the seal; counts unmoved, console drained separately) — and the tainted reply binds the same cut (`snapshot_reply_carries_the_taint`) |

## Judgment calls (for the reviewer)

1. **Retiring `Reply::SnapId` vs. keeping it decodable.** Retired. The bead makes the cut
   part of every successful snapshot reply; a decodable cut-less reply is a hole in that
   contract. The version bump makes the break visible at `hello`.
2. **Field order** in the reshaped tag-10 body: `id · at · sdk_events · tainted` (handle,
   cut, taint). Goldens pin it.
3. **`VirtualExemplar`/`Lineage` carry `cut: EvidenceCut` replacing `at: Moment`** rather
   than adding a parallel field — one authority, no drift between a bare `at` and the cut's
   `at`. This is a (pre-`Entry`-rename) shape change to serde-persisted types; the crate has
   no persisted golden artifacts of these shapes (verified: `tests/reference/` is the
   pre-refactor *code* twin, not data).
4. **`SnapMeta` origin now comes from the stamp.** Previously `branch` re-anchored at the
   client's last-observed stop V-time; the stamp IS the server's restore floor, so this
   removes the one client-derived stand-in on that path (test pins stamp ≠ pos).
5. **SocketMachine refuses a tainted seal** (loud `Transport` error) instead of threading
   taint through the campaign engine — the explorer never `exec`s, so a tainted reply is
   session interference, and `RecordedEnv` would refuse the timeline anyway. Resolution
   keeps consuming taint as data (its verb).
6. **`Machine::sdk_events()` semantics unchanged** (cumulative capture). Baselining it per
   branch (console-cursor style) is the ingestion contract of `hm-bbx.4` ("append only the
   child positions after the parent cut"), deliberately out of scope here.
7. **Toy cut = answer-log prefix.** Makes the cut non-trivial in every engine gate; honest
   for the toy (its ordered capture *is* the answer log) and replay-stable, so the
   `CutDivergence` check is exercised by the whole existing suite.
8. **`resolution` does not surface the cut** — minimal transport change outside the named
   surface; the cut is campaign-plane evidence.

## Gates run, and where

On this Mac (all green):

- `cargo nextest run --workspace --all-features` — **2037/2037** (32 skipped = the usual
  ignored/live set).
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` — native, and
  `--target x86_64-unknown-linux-gnu` (the cfg(linux) live tests compile with the new reply
  shape), and the aarch64 seam check
  (`CARGO_FEATURE_NO_NEON=1 cargo clippy --target aarch64-unknown-linux-gnu --all-features --all-targets`).
- `cargo fmt --all -- --check`, `cargo deny check`.
- Public-API snapshots regenerated (reviewable diffs): `control-proto` (the reply reshape),
  `explorer` (`EvidenceCut`, `Machine::snapshot`, `snapshot_cut`, `CutDivergence`,
  `Lineage.cut`), `campaign-runner` (trait-impl ripple). All other crates byte-identical.
- Scoped Miri: `cargo +nightly-2026-06-16 miri test -p vmm-core --lib control` (the touched
  module; the full vmm-core Miri suite is the nightly job). No new `unsafe` anywhere in this
  change.

**Handed to the foreman (box lane).** Shipping the unpushed branch to the box was denied by
the session's policy classifier, so the Linux/KVM half runs post-push:

- CI `gates` job = the portable suite on Linux (the platform half of the cut-identity
  criterion; the suite is deterministic by construction — no wall clock, no map order).
- Box gates (pinned per `docs/BOX-PINNING.md`, stock-KVM revert discipline): the updated
  live suites compile against the new reply and should be re-run as usual —
  `cargo test -p vmm-core --release --test live_sdk -- --ignored --nocapture` (gate A also
  now exercises the stamped cut implicitly: same seed ⇒ same event stream ⇒ same
  `sdk_events` count), plus `live_host_plane` / `live_exec_improvisation` /
  `live_moment_address` / `live_pvclock` / `live_dirty_remap` per their headers.

## Known limitations / integrator notes

- The cut stamps `Moment(0)` for a V-time-unwired composition (`snapshot_vns` of the
  unwired blob shape) — the same degenerate axis those compositions already live on.
- `Materialization` (the depth-accounting report) still reports `at: Moment` only; the cut
  equality check covers the sdk half. Extending the report struct was churn without a
  consumer.
- `hm-ynt` (SDK event stamps are anchor lower bounds) is orthogonal by design: nothing here
  compares event Moments — that is exactly why the cut is a prefix length. When `hm-ynt`
  lands exit-Moment stamps, this contract is unchanged.
- Coordination: `dissonance/sdk-events` got a 2-literal test fix (`VirtualExemplar.cut`);
  tasks/126 (in flight on that crate) will rebase over it trivially.
- `hm-bbx.4` consumes: `Machine::snapshot`'s cut, `SocketMachine::snapshot_cut`, and the
  `VirtualExemplar.cut`/`Lineage.cut` fields; the ingestion append-after-parent-cut rule is
  its side of the contract.
