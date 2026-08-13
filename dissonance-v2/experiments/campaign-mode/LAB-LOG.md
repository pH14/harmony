# Campaign mode lab log

## 2026-08-13 — setup

- Authority is the integrator specification
  `/Users/phemberger/workspace/steers/CAMPAIGN-MODE.md`. Campaign mode is a
  second, recorded execution mode for conquest-scale runs: one machine, all
  cores, one shared archive. Experiment mode stays serial, seed-pure, and
  untouched.
- Worktree `/Users/phemberger/workspace/harmony-campaign-mode`, branch
  `exec/campaign-mode`, cut from the search branch head
  `BASE_COMMIT=7fab4ce5fb44452849bd27e5aae04dbacd60fb87`. The branch is pinned
  to that base; the active search worker continues on the same crate, so every
  change here is additive and shared files are edited as little as possible.
- External ROM SHA-256 verified at
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`, matching
  the recorded M4 value.
- Read before design: the SMB completion lab log through C49, `NOTES.md`, the
  archive engine in `phase4c.rs`, the target in `phase4b.rs`, and the
  `smb-completion`, `smb-film`, and `executor-identity` binaries.
- This worktree carries no `target/` evidence. Recorded panel evidence and the
  C49 source archive live in the search worker's worktree, which is out of
  bounds here. Consequences are recorded per gate below; the demonstration
  run's source archive must be handed over by the integrator or regenerated.

## CM0 — coordinator design, registered before code

### What a campaign is

- A campaign starts from an origin — clean genesis or a recorded source
  archive resumed at its single shortest mechanical frontier input, exactly
  the C49 selection — and runs W workers on one machine against one shared
  archive.
- A job is (parent snapshot, mutation seed). The mutation seed alone
  determines the suffix: chord count from the frozen one-or-two distribution,
  chords from the frozen nine-mask vocabulary and stratified durations, all
  drawn from `StdRand::with_seed(mutation seed)`. Executing the suffix from
  the parent snapshot is a pure function of those two values; this is the
  already-proven job semantics, unchanged.
- The promoted stack and only the promoted stack: corrected M35 terminal
  condition, H45 probing retention, frozen parent scheduler, nine-mask
  vocabulary, stratified durations, one-or-two suffixes, capacity-two cells
  with fewer-actions replacement, 512-action bound, snapshot-resume archive
  executor. No model, no ranking, no generated mutator, no vertical-page key
  term.

### Worker pool

- W worker threads plus the coordinating thread. Each worker owns its own
  `SmbTarget` instance constructed from the same ROM bytes; nothing about
  emulation is shared.
- Per-worker RNG streams derive from (campaign seed, worker index): the
  worker stream seed is the first eight bytes, little-endian, of
  SHA-256(campaign seed LE bytes ‖ worker index LE bytes). The worker stream
  supplies parent selection draws and one `next()` per job as the mutation
  seed. Every random draw in a live run is therefore principled; the only
  nondeterminism is interleaving — which results reach admission in which
  order, and which archive state each selection saw.
- A worker loop: reserve budget and select under the lock, execute outside
  the lock, submit under the lock. Workers exit when the budget is fully
  reserved and their in-flight job is admitted.

### The admission lock

- One mutex guards everything the interleaving can touch: the shared archive,
  the aggregate report state (milestones, watermark, first-reached times and
  inputs, champion, curve, deaths), the budget reservation counter, and the
  stream writer. Because every archive mutation and every stream append
  happens under this one lock, the archive state at any stream position is
  identical in the live run and in replay. That single invariant is what
  makes the recorded stream a complete identity.
- Selection critical section: reserve one execution; choose a parent with the
  frozen scheduler distribution (one quarter uniform over active expandable
  entries, three quarters uniform over the 128-entry frontier window) driven
  by the worker's own stream; draw the mutation seed; derive the suffix; if
  every action-boundary input of the candidate job already has an archive id,
  release the reservation, append a skip record, and select again — this is
  the specification's reject-duplicates-by-hash-before-execution rule, applied
  only when the full prefix chain is already present, which is exactly the
  case where execution cannot change the archive or any maximum. Otherwise
  clone the parent's snapshot, input, and milestones, and leave the lock.
- Execution, outside the lock: restore the parent snapshot, apply the suffix
  chord by chord exactly as the serial engine does — same observation
  merging, same terminal handling, same per-boundary snapshot and admission
  probe — and collect one result: per action, its observations, accumulated
  milestones, death and failure flags, and for each non-terminal boundary the
  snapshot, archive key, and probe verdict. The probe verdict is computed
  worker-side because it is a pure function of the snapshot; admission then
  needs no emulator.
- Admission critical section: assign the next sequence number in stream
  order; merge the result's per-action observations into watermark,
  milestones, first-reached (at this sequence number), and champion, in
  action order; admit each candidate in order under the promoted retention
  rules through the same `Archive::insert` the serial engine uses — duplicate
  inputs resolve to the existing id, probe-refused candidates are skipped
  without advancing the parent chain and without touching the retained or
  rejected counters, exactly as in the serial engine; append the job record;
  push a curve point every 100 sequence numbers and at the end.

### Stream recording

- The stream is one JSONL file. Line one is the header: campaign seed, worker
  count, host, origin (kind, source path, source archive SHA-256, resume
  input SHA-256 and action count), execution budget, optional wall budget,
  action limit, policies, scheduler and executor identifiers, the worker-seed
  derivation rule, and the ROM SHA-256.
- Each executed job appends one record: sequence number, worker index, parent
  id, mutation seed, frames emulated (execution plus probes), the SHA-256 of
  the complete serialized job result, and the ordered admission decisions —
  retained with the assigned id, duplicate with the existing id, rejected by
  the cell bound, rejected by the archive bound, or refused by the probe.
- Each pre-execution duplicate skip appends one record: worker index, parent
  id, mutation seed. Skips consume no budget and change no archive state;
  they are recorded so the report's counters replay from the stream.
- The report records mode=campaign, worker count, host, campaign seed, and
  states plainly that the live schedule is not derivable from the seed alone;
  the recorded stream is the campaign's identity. Two live runs at the same
  seed may differ; each replays exactly. Report files contain no wall-clock
  values; live throughput measurements go to a separate live-only file.
- Outputs per run: `stream.jsonl`, `archive-live.json` (the standard
  `SmbArchiveReport` shape, so `smb-film` and every existing audit reads it
  unchanged), and `campaign-report.json` (the campaign wrapper embedding the
  archive report).

### Replay

- Replay needs the stream, the origin archive file, and the ROM; no model, no
  parallelism, one target instance. It reproduces the bootstrap from the
  origin, then re-executes each stream record serially: restore the recorded
  parent, re-derive the suffix from the recorded mutation seed, execute,
  verify the result digest and frame count byte-for-byte, re-apply the
  promoted retention rules, and verify every recomputed admission decision
  against the recorded one. Skip records are verified by re-checking the
  full-prefix-duplicate condition against the replayed archive at the same
  stream position.
- Replay then writes `archive-replay.json` and `campaign-report-replay.json`;
  the acceptance comparison is raw byte equality against the live files. Any
  digest, frame, decision, or byte mismatch is a loud failure, not a warning.

### What concurrency strictly requires, stated as deviations with rationale

- One RNG stream per worker instead of one campaign stream: a single shared
  stream would make every draw depend on interleaving, destroying the
  principled derivation of mutation seeds. Archive keys, novelty, cell
  bounds, retention, vocabulary, durations, and suffix lengths are unchanged.
- The mutation seed is drawn once per job and expands to the suffix through a
  fresh seeded RNG, instead of the serial engine's single continuous RNG:
  this is what makes a job a pure function of (parent snapshot, mutation
  seed) and therefore replayable from the stream. The sampled distributions
  are the frozen ones, sampled by the same code.
- Parent selection sees the live archive at selection time instead of the
  archive after the previous execution: unavoidable under concurrency; the
  distribution itself is the frozen one. The chosen parent id is recorded, so
  replay does not re-run selection.
- Aggregate maxima, first-reached times, champion, and the curve are merged
  at admission in stream order instead of during execution: admission order
  is the campaign's time axis; first-reached executions are stream sequence
  numbers.
- Pre-execution duplicate rejection exists only in campaign mode, as the
  specification directs, and skips only jobs whose entire prefix chain is
  already archived — jobs that cannot change the archive, any maximum, or the
  death count.

### Shared-surface changes to existing files, kept minimal

- `phase4c.rs`: `pub(crate)` visibility on the existing `Archive`,
  `ArchiveEntry`, `ArchiveCandidate`, `admission_is_viable`, `archive_key`,
  `sample_chord`, `merge_progress_watermark`, `merge_action_milestones`,
  `merge_milestones`, `milestone_key`, `update_first_inputs`, and the frozen
  constants, so the coordinator calls the same admission, key, probe, and
  sampling code instead of forking it. One additive public wrapper returns
  the serial search report together with its emulated-frame count for the
  throughput gate. No behavior change; the identity gate and the recorded
  frozen hashes are the proof.
- `phase4b.rs`: `SmbTarget` gains a monotonic `frames_clocked` counter
  incremented wherever the deck clocks a frame, with a getter. It is not part
  of any snapshot, report, or serialized state; restores do not touch it. It
  exists so both throughput arms and every stream record can report exact
  deterministic emulated work, probes included.
- `lib.rs`: one added line, `pub mod campaign;`.
- `smb-completion.rs` and every other existing binary: zero edits. The
  frontier-input selection rule (shortest input at the maximum mechanical
  tuple, earlier id on ties) is implemented in the campaign module as the
  shared extraction point; the identical private copy inside `smb-completion`
  stays untouched to keep the merge trivial, and folding it into the shared
  one is named integration work.

### Gate mapping and milestones

- CM1 — shared surface: the visibility and counter changes above, full
  quality gates, and the executor-identity gate reproducing the frozen M15
  semantic hashes (maze `6e1500f1…`, adventure `8debc5e9…`, SMB `085c22ee…`).
  The binary's own tenfold-frame acceptance clause is recorded false by M15
  and superseded by the integrator's EXECUTOR-REWORK amendment-3 ruling; gate
  1 here is the three identity bits plus the frozen hashes. Because this
  worktree holds no recorded panel evidence, byte-identity of previously
  recorded campaigns is additionally supported by construction (the diff to
  shared files is visibility, an inert counter, and an additive wrapper) and
  by the unchanged-serial unit tests; re-running a recorded arm against its
  published SHA needs a source archive file and is named integration work.
- CM2 — coordinator and replay on the synthetic NROM: unit tests prove
  cross-instance job purity (one job executed on two independently
  constructed targets yields byte-identical results), campaign-then-replay
  byte identity at W ≥ 4, and skip-record verification. No unit test reads
  the SMB ROM.
- CM3 — the `smb-campaign` binary: run, replay, and throughput modes; a
  real-ROM smoke campaign from clean genesis at W = 6 with exact replay and
  film.
- CM4 — gate 2: a recorded run at 20,000 executions or more, W ≥ 6, replayed
  to byte-identical archive and report. Gate 3: two live runs at one seed,
  diffed, each replaying exactly. The specification itself authorizes these
  sizes past the standing 20,000-execution ceiling.
- CM5 — gate 4: same wall-clock budget, W workers versus the untouched serial
  engine, same origin; report executions completed, frames emulated, and
  furthest corrected (world, level, progress) for both. The serial arm runs
  first at a fixed execution budget to set the wall budget; the campaign arm
  then runs with reservation stopped at that wall budget. The wall cutoff
  never enters campaign state — it only stops issuing reservations — so the
  recorded stream still replays exactly.
- CM6 — demonstration: 50,000 executions, all cores, from the current best
  source archive (`h45-viability/probe-e001/archive-live.json`, viable
  progress 114), recorded stream, exact replay, film of the best trajectory.
  Blocked on receiving that archive file; every prior milestone proceeds
  without it.

### Design decisions the specification leaves open, settled here

- Sequence numbers are assigned at admission, so the stream file is ordered
  by its own numbering and "executions completed" means admitted jobs.
- The host string is supplied by the operator on the command line and
  recorded in the header, keeping the report free of environment probes.
- A worker that draws duplicate after duplicate re-selects under the lock;
  after 1,024 consecutive skips it executes the next job anyway and lets
  admission deduplicate, so a saturated archive cannot livelock selection.
  Skips at that scale would themselves be recorded evidence.
- Genesis-origin campaigns pass a single empty input as the initial corpus,
  which retains gameplay genesis alone, exactly as the serial engine does.

## CM1 — shared campaign surface

- `SmbTarget` gained a monotonic `frames_clocked` counter incremented at every
  deck clock site — construction bootstrap, `apply`, the admission probe, and
  film clocking — with a getter. It is work accounting only: no snapshot
  carries it, `restore` and `reset` do not touch it, and no serial report
  reads it.
- `phase4c` items needed by the coordinator became `pub(crate)`: the archive
  and its insert and parent-selection methods, the admission probe, the key,
  the chord sampler, and the milestone and watermark merge helpers. One
  additive wrapper, `run_smb_archive_search_with_retention_and_work`, returns
  the unchanged serial report together with the target's lifetime frame
  total; the five existing wrappers discard the new second value. No control
  flow changed anywhere in either file.
- Quality gates at this state: build clean, `cargo fmt --check` clean, clippy
  under `-D warnings` printing only the known pre-existing configuration
  warning, `cargo nextest run --all-features` 70 of 70 passed, and
  `cargo deny check` ok. One nextest run flagged one test leaky; it passed,
  the flag did not recur, and no change here spawns a process.
- The executor-identity gate result is recorded below when its SMB arm
  completes; the acceptance is the three identity bits plus the frozen M15
  semantic hashes, with the binary's own tenfold clause recorded false and
  superseded by the integrator's EXECUTOR-REWORK amendment-3 ruling. Wall
  ratios measured on this run are non-evidence: the machine concurrently
  hosts the active search worker's two full-load runs, and M15's recorded
  ratios stand.

## CM2 — coordinator and replay

- The module is `fuzzer/src/campaign.rs` plus one added `pub mod campaign;`
  line in `lib.rs`. Every other existing file is untouched since CM1.
- The single admission lock is realized as the coordinator thread's serial
  event loop, a strictly stronger serialization with identical semantics:
  workers only execute jobs and return results over a channel; selection,
  admission, budget reservation, and stream writing all happen on the
  coordinator, so every archive mutation and every stream append is ordered
  by one thread and the archive state at any stream position is identical
  live and replayed. The arrival order of worker results is the run's only
  nondeterminism.
- A drawn job ships to its worker as (parent snapshot, parent length, parent
  milestones, suffix); the suffix expands from the mutation seed alone
  through the shared frozen samplers. Workers compute the admission-probe
  verdict per boundary, because it is a pure function of the snapshot, so
  admission needs no emulator. Archive entries are append-only and immutable
  once inserted, which is why a parent selected early and admitted against
  later is the same parent bytes replay reads at the job's stream position.
- Replay re-executes every recorded job serially from (parent id, mutation
  seed), and verifies four things per job against the stream: the result
  digest over the complete serialized result including snapshots, the exact
  emulated frame count including probes, the admission sequence number, and
  every recomputed admission decision. Recorded skips are verified to be true
  full-prefix duplicates at their stream position. Any mismatch is an error,
  not a warning.
- Unit tests on the synthetic NROM, no SMB ROM read: worker-seed derivation
  stability, suffix purity and bounds, job purity across two independently
  constructed target instances, a threaded W = 4 live campaign whose replay
  is equal as a structure and byte-identical as serialized JSON, a tampered
  skip record failing replay loudly, and an archive-origin campaign round-
  tripping through replay. Full gates: 76 of 76 tests passed, fmt, clippy,
  and deny clean.
- One synthetic-NROM finding recorded for honesty: with all-zero work RAM
  every entry shares one key, so the frontier resume selection legitimately
  returns the empty genesis input; the archive-origin unit test asserts the
  round trip, not a nonempty resume input.

## CM3 — the smb-campaign binary and the real-ROM smoke

- The binary has three modes. `run` records a live campaign into
  `stream.jsonl`, `archive-live.json`, and `campaign-report.json`, with wall
  measurements confined to a separate live-only `throughput-live.json`.
  `replay` re-executes a recorded run serially, writes `archive-replay.json`
  and `campaign-report-replay.json`, byte-compares them against the live
  files, and exits nonzero on any divergence. `serial-arm` runs the untouched
  serial engine on the same origin selection for the throughput gate,
  reporting its report plus exact emulated frames.
- Cross-instance proof on the real ROM, before the smoke: a W = 2, 40-
  execution genesis campaign — where workers routinely execute jobs from
  snapshots another instance produced — replayed byte-identically on a single
  fresh instance, every result digest, frame count, and admission decision
  matching. Evidence: `target/perf-evidence/campaign-mode/smoke-tiny/`.
- The smoke: genesis origin, campaign seed `0x5eed_ca10`, W = 6, 500
  executions, 512-action limit, on the local machine while the search
  worker's two full-load runs shared it. It completed 500 executions in 34.5
  seconds (14.5 executions per second live, co-tenant), emulated 102,782
  frames including probes and bootstrap, retained 486 entries, rejected 35,
  refused 38 candidates at the probe, skipped no duplicates, observed 53
  deaths, and reached 16-pixel progress bucket 30 in 1-1 with jobs spread
  `[82, 86, 83, 79, 79, 91]` across the six workers.
- Its serial replay is byte-identical: `replay_verified=true`, archive
  SHA-256 `489ab4ae76e80c19ad264e1fc495961dac1f4e0dfd5ef3aa44c974a16e57d09a`,
  report SHA-256
  `3236b51bfbcf4866bce74ff5b86f9f3c5f4f13227fa9c6e2474db337c1d3a40d`, stream
  SHA-256
  `b0b5dcb3100e9d1331a7a56a3338133a250c1ecad2fa0109a0c8ff0c28531803`.
- The unchanged film renderer read the campaign's `archive-live.json`
  directly and wrote the frontier strip and SHA-pinned manifest under
  `target/perf-evidence/campaign-mode/smoke-genesis-500/film/`.

### CM1 identity-gate result — recorded with a finding

- All three identity bits are true: legacy and snapshot-resume reports are
  bit-identical after normalizing only executor mode and work counters, on
  maze, adventure, and SMB, at the frozen seed `0x5eed_ee01` and 5,000
  executions.
- Maze and adventure semantic hashes equal the frozen M15 references exactly:
  `6e1500f1f0baa2a479f5828158e68995deef395d2ac5b6eeb45d858f5c6b7844` and
  `8debc5e902d71df10c18ab868df375a4b84478338686cc2bdff8f97f42a62153`.
- The SMB semantic hash is
  `3ff148641ed3db6f8f6432549019d654081d164c5428d2cd3e554aaa72bc111b`, not the
  frozen M15 value, and the legacy arm emulated 645,931 frames against
  M15's recorded 730,736. The explanation on the record: the frozen M15
  reference predates the promoted M35 terminal-condition correction, which
  changed `smb_player_is_dead` for every SMB path, ratchet included, so no
  tree at or after M35 can reproduce the M15-era SMB hash. The completion lab
  log records no re-frozen identity hash after M35.
- Consequence for gate 1, stated plainly rather than reinterpreted silently:
  the maze and adventure frozen hashes hold; the SMB frozen hash is
  unsatisfiable on this base for reasons unrelated to campaign mode. The
  substitute evidence run next is the same gate at the untouched base commit
  `7fab4ce5`: if it produces the same SMB hash, the campaign-mode changes are
  proven behaviorally inert, and the gate-1 SMB criterion is put to the
  integrator as needing a re-frozen post-M35 reference.
- Wall ratios from this run are non-evidence; the machine concurrently hosted
  the search worker's two full-load runs throughout. M15's recorded ratios
  stand. The binary's own acceptance flag is false only through its withdrawn
  tenfold clause, as M15 records.

### Gate-1 base verification — the campaign branch is behaviorally inert

- The same gate was rebuilt and run from the untouched base commit
  `7fab4ce5`, from a binary verified to differ from the branch build. Its
  report is equal to the campaign branch's on every field that matters: all
  three identity bits true, maze and adventure at their frozen hashes, and
  SMB at the same post-M35 hash
  `3ff148641ed3db6f8f6432549019d654081d164c5428d2cd3e554aaa72bc111b` with
  identical work counters (legacy 645,931 frames, snapshot-resume 411,927
  frames) on both trees.
- Conclusion recorded for gate 1: the serial path is untouched — the branch
  and its base are bit-equal on the identity gate — and the frozen M15 SMB
  reference `085c22ee…` is unreproducible on any tree at or after the
  promoted M35 terminal-condition correction. Re-freezing that reference is
  put to the integrator; it is not a campaign-mode defect. Evidence:
  `target/perf-evidence/campaign-mode/executor-identity/` and
  `target/perf-evidence/campaign-mode/executor-identity-base/`.
