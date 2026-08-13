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
