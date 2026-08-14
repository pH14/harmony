# Selection correction lab log

## 2026-08-14 — setup

- Authority is the H56 registration and its mechanism-builder requirements at
  the end of the SMB completion lab log, recorded at this branch's base commit
  `746eedfa`. This experiment implements the mechanism and its gates only; the
  registered acceptance arms return to the search experiment after the merge.
- Integrator requirements beyond the H56 text, received as instructions: the
  selector policy threads through both execution modes — the serial experiment
  engine and campaign mode — defaulting to frozen everywhere; the campaign
  stream header and every archive report record which selector policy ran;
  inertness evidence covers both modes against two published recorded
  artifacts; construction is additive with minimal edits to shared files.
- Worktree `/Users/phemberger/workspace/harmony-selection-fix`, branch
  `exec/selection-correction`, cut from `746eedfa`. Nothing here touches any
  other worktree.
- Inputs verified before use. The external ROM's SHA-256 is
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`, matching
  the recorded M4 value. The published conquest source archive at
  `steers/c49-world2-origin/archive-live.json` matches its manifest at
  `b57e72e42c2c87942b02947ba43014d18c88ee70fdadeebb63d7d0251f9f1273`. The
  published inertness references at `steers/h56-inertness-references/` all
  verify against `MANIFEST-SHA256.txt`: the recorded M53 gate report at
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` and the
  recorded gate-2 campaign stream, live archive and live report at
  `689ef0ee…`, `5613ab35…` and `03210261…`.
- The recorded M53 gate recipe was identified from the artifact itself rather
  than assumed: its bootstrap chain tops out at a 351-action input whose
  endpoint is the maximum recorded tuple `(1, 0, 144)`, so the run resumed
  from the shortest input at the maximum mechanical tuple — the C49 frontier
  rule — under probing retention and the extended ladder at seed
  `0x5eed_ef00` for 256 executions with the 512-action bound. That is exactly
  the existing command-line mode `archive-resume-frontier-viable-ladder`.
  Byte equality of the reproduction is the check on this identification.
- Read before design: the H56 sections; the frozen selector
  (`Archive::choose_parent`, `insert`, `active_ids` in `fuzzer/src/phase4c.rs`);
  the campaign coordinator and replay (`fuzzer/src/campaign.rs`); the
  campaign-mode lab log for the additive-policy pattern; the generated-ranking
  harness template in `smb-model-host.rs` for the play-bucket resume rule.

## SC0 — design, registered before code

### The policy, in the established additive pattern

- A new enum `SmbArchiveSelectorPolicy { Frozen, CorrectedTieClass }`, default
  `Frozen`, carried exactly as the retention, key and ladder policies are:
  threaded through the serial engine internals and the campaign configuration,
  selected per campaign, defaulting to frozen at every existing entry point.
- The frozen selector function is not edited. `Archive::choose_parent` keeps
  its body and its randomness byte for byte; a new dispatch method
  `select_parent` routes to it under the frozen policy and to the corrected
  algorithm under the corrected one. Both existing call sites — the serial
  engine loop and the campaign coordinator's selection — call the dispatch.
- Identifier strings: `frozen_frontier_128` (existing) and
  `corrected_tie_class` (new), used in the campaign stream header's
  `parent_scheduler` field, the campaign report, and the experiment-mode
  summary. Campaign replay derives the policy from the recorded header, so a
  recorded stream can never be replayed under the wrong selector.

### The corrected algorithm, mapped to the three registered changes

- The one-in-four uniform draw over all active entries is byte-for-byte the
  frozen behaviour and is deliberately untouched, including its position in
  the random stream: first the frontier-versus-uniform draw, then the uniform
  index draw when uniform wins. H56 diagnoses the dilution and leaves it
  alone; exhaustion governs only the tie-class path.
- **Change one, the key.** The tie-class path ranks active entries by the
  corrected `(world, level, progress)` tuple, ordered lexicographically, with
  M52's ladder semantics. `milestone_key` plays no part. The four frozen rungs
  stay in every report untouched.
- **Change two, the tie classes.** Active entries group by `(world, level)`
  pair, considered in descending pair order. Within a pair, classes are
  successive progress bands: the deepest remaining progress anchors a class
  of all entries within the fixed `FRONTIER_PROGRESS_BAND` of 8 below it —
  the same saturating arithmetic the recorded experimental band selector
  uses, `progress + 7 >= anchor` — and the next class anchors at the deepest
  remaining progress below that band. The first class with an unexhausted
  member is the frontier; the draw is uniform over its unexhausted members.
  A class whose members are all exhausted is skipped and counted. This
  within-pair banding follows the registration's own narrative: when the
  `(1, 0, 144)` singleton exhausts, "the frontier becomes the band below it"
  at the play frontier 124 — the same pair.
- **Change three, the accounting.** Per entry: how often it was selected as a
  parent, how often a selection produced at least one retained descendant,
  and how many selections have passed since the last retained descendant. An
  entry is exhausted when the last counter reaches the fixed threshold of 64;
  a retained descendant resets it. If the tie-class path finds no unexhausted
  member in any class of any pair, every active entry is exhausted: all
  exhaustion counters reset to zero, the reset is counted, and selection
  proceeds — the search cannot deadlock.

### Decisions the registration leaves open, settled here

- "All counters reset to zero" is read as the exhaustion counters — the
  selections-since-last-retained-descendant of every entry. The cumulative
  per-entry selected and productive totals are not erased, because G4's
  checkability depends on that record surviving; the reset itself is counted
  and reported.
- A campaign-mode pre-execution duplicate skip counts as a selection of its
  parent: it consumed a draw and produced nothing, which is exactly what the
  accounting exists to notice.
- Exhausted members of a surviving class are not sampled; the draw is uniform
  over the unexhausted members only. This is the plain reading of "not
  sampled while any unexhausted member of its class remains".
- The exhaustion state read at selection time in a live campaign is the state
  the coordinator holds at that moment, which lags in-flight jobs by at most
  the worker count. This is the same already-accepted trade as selection
  seeing the live archive: live-schedule input, identity by the recorded
  stream, never re-derived.

### Reporting, shaped so every frozen artifact keeps its bytes

- `SmbArchiveReport` gains a `selector` accounting block — policy identifier,
  uniform and tie-class selection counts, productive selections, classes
  skipped, counter resets — serialized only when it differs from the frozen
  default, exactly the ladder's absent-under-frozen pattern. Every recorded
  artifact deserializes with the field defaulted and re-serializes without
  it.
- `SmbArchiveEntryReport` gains an optional per-entry `selector` counter pair
  (selected, productive), `None` and unserialized under the frozen policy,
  populated for every entry at report finalization under the corrected one.
- Campaign stream records gain an optional `selector` annotation — draw path,
  classes skipped by that draw, whether the draw performed the global reset —
  absent under the frozen policy, so frozen streams keep their recorded byte
  shape. Under the corrected policy every skip and job record carries one.
- Counter update points are the stream record writes: the skip write at
  selection time and the job write at admission, both on the coordinator, so
  replay reproduces the final counters exactly by processing the stream in
  order. The serial engine updates at selection and at execution end, which
  re-runs identically by construction.
- Replay adopts the selector annotations as recorded identity — it verifies
  their presence matches the header's policy and rejects a frozen stream
  carrying annotations or a corrected stream missing them, but does not
  recompute selection-time state, exactly as it adopts recorded parent ids.
  Everything admission-derivable (per-entry counters, productive selections)
  is recomputed, not adopted.

### Command-line surfaces

- `smb-campaign run` accepts an optional `--selector corrected_tie_class`;
  omitted means frozen. `replay` needs no flag: the policy comes from the
  recorded header. `serial-arm` stays frozen-only.
- `smb-completion` gains two modes for the acceptance-arm shape —
  `archive-resume-play-viable-ladder` (frozen selector) and
  `archive-resume-play-viable-ladder-corrected` (corrected selector) — both
  resuming from the shortest input at an operator-supplied play bucket of the
  source archive's maximal `(world, level)` pair, the M53 resume rule the
  generated-ranking harness applies, with probing retention, the extended
  ladder, stratified durations and one-or-two suffixes. Every existing mode
  is untouched and keeps its exact current call path.

### Gate plan, from the registration and the integrator requirements

- **G1, inertness, both modes.** Experiment mode: the rebuilt tree reruns the
  recorded M53 gate — `archive-resume-frontier-viable-ladder` on the verified
  conquest archive, seed `0x5eed_ef00`, 256 executions, 512-action bound —
  and the report's SHA-256 must equal the recorded
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360`.
  Campaign mode: the rebuilt binary replays the recorded gate-2 stream from
  genesis and must reproduce the recorded archive and report hashes
  `5613ab35…` and `03210261…` byte for byte.
- **G2, determinism.** One corrected experiment-mode arm from the conquest
  archive at its deepest play bucket runs twice from the same seed with no
  model and must be byte-identical. One corrected campaign-mode run on the
  real ROM must replay byte-identically from its recorded stream.
- **G3, quality.** `cargo fmt --check`, `cargo clippy --all-features` with
  `-D warnings`, `cargo nextest run --all-features`, `cargo deny check`.
- **G4, honesty.** Every corrected report carries the selection and novelty
  counters, the classes-skipped count and the counter-reset count; unit tests
  pin the starvation, fall-through and reset behaviours; the recorded G2 arm's
  counters are read back and stated here with numbers.
- Evidence destination: `target/perf-evidence/selection-correction/`.

### Milestones

- SC1 — the mechanism in `phase4c.rs` with its unit tests.
- SC2 — campaign-mode threading in `campaign.rs` with its unit tests.
- SC3 — the two command-line surfaces.
- SC4 — gate evidence: G1 both modes, G2 both modes, G4 readback, G3
  throughout; recorded here with numbers.

## SC1–SC3 — the mechanism, both modes, both surfaces

Built and committed as one change: the archive constructor carries the policy,
so the serial engine, the campaign coordinator and the binaries thread it
together or not at all.

- `phase4c.rs`. The policy enum and the accounting types; three per-entry
  counter vectors and the campaign accounting inside `Archive`; the
  `select_parent` dispatch, with `choose_parent` — the frozen selector — not
  edited at all; the corrected algorithm as registered (corrected key,
  descending pair order, successive progress bands of the recorded width 8,
  uniform draw over unexhausted class members, fall-through with skip
  counting, deterministic all-exhausted reset); `record_selection` and
  `record_selection_outcome` as the only counter-mutation points, both no-ops
  under the frozen policy; the `run_smb_archive_search_with_selector` wrapper;
  the report fields, absent under frozen by the ladder's own serde pattern.
  Every existing wrapper passes the frozen policy explicitly.
- `campaign.rs`. The config, header and replay carry the policy through the
  `parent_scheduler` identifier; skip and job records carry the optional draw
  annotation; the coordinator records selections at the two stream-write
  points so replay reproduces every counter from the stream; replay rejects
  annotation presence that disagrees with the header policy and otherwise
  adopts draws as recorded identity, recomputing everything
  admission-derivable.
- Binaries. `smb-campaign run` takes `--selector corrected_tie_class`
  (replay needs nothing — the header decides); `smb-completion` gains
  `archive-resume-play-viable-ladder` and
  `archive-resume-play-viable-ladder-corrected`, both resuming by the M53
  play-bucket rule with the bucket as an explicit argument. No existing mode's
  call path changed.
- Unit tests, seven new, 83 total. The corrected selector draws only from the
  maximal-pair band; starves an exhausted singleton and falls through to the
  band below with the skip counted; resets deterministically when everything
  is exhausted and counts the reset; the frozen wrapper is equal to the
  frozen reference and serializes no selector field; a corrected serial run
  replays equal with selections summing to the budget; a frozen campaign
  stream and report carry no selector fields; a corrected campaign replays
  byte-identically with an annotation on every record and counters summing to
  jobs plus skips.
- Gates at this commit: `cargo fmt --check` clean; clippy under `-D warnings`
  printing only the known pre-existing configuration warning from
  `clippy.toml`; `cargo nextest run --all-features` 83 of 83 passed;
  `cargo deny check` advisories, bans, licenses, sources ok.

### Ruling — the dual-policy structure is temporary

- Integrator ruling, received 2026-08-14 while the SC4 evidence runs were in
  flight: the frozen selector survives only as long as the acceptance panel
  needs it as the control arm. Immediately after the corrected selector is
  promoted, the frozen selector path is deleted outright in a follow-up
  commit — the corrected selector becomes the only picker, and historical
  results re-verify by checking out the commit that recorded them, not by
  carrying dead code forward.
- Recorded here so the merge record shows the carry is scheduled for
  deletion. Nothing in this build assumes the frozen path is permanent: the
  frozen selector is one untouched function behind one dispatch method, the
  policy enum and its threading are the deletion surface, and the corrected
  path shares no code with the frozen one beyond the uniform draw, so the
  follow-up deletion is a removal, not a rewrite.
- The gates below are unchanged by the ruling and were finished as specified.

## SC4 — gate evidence

### G1, inertness, experiment mode — PASS exactly

- The rebuilt tree reran the recorded M53 gate campaign through the untouched
  command-line mode: `archive-resume-frontier-viable-ladder` on the verified
  conquest archive, seed `0x5eed_ef00`, 256 executions, 512-action bound,
  probing retention, extended ladder, release build.
- The reproduced `archive-live.json` has SHA-256
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` — equal
  byte for byte to the recorded artifact. 597 entries, 597 retained, 1
  rejected, summary `parent_scheduler` `frozen_frontier_128`, exactly as
  recorded. This also confirms the recipe identification from the setup note:
  the recorded gate resumed by the mechanical-frontier rule.
- Evidence: `target/perf-evidence/selection-correction/m53-gate-repro/`.

### G1, inertness, campaign mode — PASS exactly

- The rebuilt `smb-campaign` binary replayed the recorded gate-2 stream —
  genesis origin, 20,000 executions, W = 6 — serially to completion.
  `replay_verified=true`, and every hash equals the recorded value: archive
  `5613ab357ade74011e17422c8edcc003668b3d6e170ef0c7d9a398f46b53cc01`, report
  `03210261f4900202c276ded0d91e9c0a007a2b5d33ce004cad0ca246c810f111`, stream
  `689ef0eeb16f5e906dcd1d4debaedb268dd3027fe22599b0153de5a3d812f88f`. The
  recorded frozen-policy stream deserializes, replays and re-serializes with
  no selector field anywhere, so every recorded campaign artifact keeps its
  bytes under the new code.
- Input provenance: the recorded stream and live artifacts were taken from
  the integrator's published copies and verified against
  `MANIFEST-SHA256.txt` before and after staging.
- Evidence: `target/perf-evidence/selection-correction/gate2-20000/`.

### G2, determinism, campaign mode — PASS

- One corrected live campaign on the real ROM: conquest-archive origin
  (resume input the recorded 351-action frontier entry), campaign seed
  `0x5eed_ca60`, W = 6, 500 executions, 512-action bound,
  `--selector corrected_tie_class`. Its complete serial replay is
  byte-identical: `replay_verified=true`, archive SHA-256
  `2169317d5e4c37b0a3b49c6263ee1694996dba55063a3e3498e9d6c77dc999f6`, report
  SHA-256 `7e1d418f4b7400f4e5ce604c7cf84c08b60a2afeba61602930d3f32a6f515dd0`,
  stream SHA-256
  `0dd0a5468d51afe1215d2cd95a8dcd2312d30f39451d3e458442a3af8e9d55a0`.
- The header and report record `parent_scheduler` `corrected_tie_class`; every
  stream record carries its draw annotation.
- G4 accounting readback, internally consistent: 174 uniform draws plus 432
  tie-class draws equals the 500 executed jobs plus 106 pre-execution skips;
  all 674 entries carry per-entry counters; per-entry selected sums to the
  same 606; 296 selections were productive. No class was skipped and no
  counter reset fired in this arm — recorded honestly: at 500 executions from
  the frontier origin the deepest band kept producing, so exhaustion never
  starved it; the starvation, fall-through and reset behaviours are pinned by
  the unit tests and are expected to appear in longer arms.
- Evidence: `target/perf-evidence/selection-correction/g2-campaign-corrected/`.

### G2, determinism, experiment mode — PASS

- One corrected serial arm through the new
  `archive-resume-play-viable-ladder-corrected` mode: the verified conquest
  archive resumed by the M53 play-bucket rule at bucket 124 (the H56 source
  shape — the 422-action shortest input at the deepest play bucket), seed
  `0x5eed_ef01`, 1,000 executions, 512-action bound, no model, run twice by
  `--replay`. Both runs are byte-identical: `replay_verified=true`, both
  `archive-live.json` and `archive-replay.json` at SHA-256
  `faf856a1bde4511a16952aa07faaa6f23eff63c7cb3d18e6a73728a76a2d555b`.
- The run is a real corrected-selector workout at the acceptance shape:
  1,288 entries retained, 305 rejected, 24 deaths, watermark and ladder
  maximum `(1, 0, 124)`.
- G4 accounting readback, internally consistent: 253 uniform plus 747
  tie-class draws equals the 1,000 executions; per-entry counters present on
  all 1,288 entries and summing to the same 1,000 selected; 750 selections
  productive. No class skip and no reset fired here either — with
  three-quarters of selections productive, the deepest band kept refreshing
  its exhaustion counters — and no entry reached the threshold without
  producing. The summary records `parent_scheduler` `corrected_tie_class`,
  `source_selection` `deepest_play_bucket`, `play_bucket` 124.
- Evidence: `target/perf-evidence/selection-correction/g2-serial-corrected/`.

## Closing status — all gates green, mechanism complete, stopping as specced

- G1 inertness passed exactly in both modes: the recorded M53 experiment-mode
  gate reproduces at its recorded SHA-256 on this tree, and the recorded
  gate-2 campaign stream replays to its three recorded hashes with the
  rebuilt binary. G2 determinism passed in both modes: a corrected serial arm
  at the acceptance shape is byte-identical across two runs from one seed,
  and a corrected live campaign replays byte-identically from its stream. G3
  quality gates passed at every commit. G4 honesty holds: every corrected
  report carries the selection and novelty counters, the classes-skipped and
  counter-reset counts, and per-entry counters, with the readbacks above
  checked against the draw totals.
- One honest negative for the record: neither G2 arm exercised a class skip
  or a counter reset in flight — at these budgets the deepest band stayed
  productive. Those behaviours are pinned by unit tests
  (`corrected_selector_starves_exhausted_parents_and_falls_through`,
  `corrected_selector_resets_deterministically_when_all_are_exhausted`), and
  their live appearance is expected at the panel's 5,000-execution budgets.
- Per the registration's division of labour, the acceptance arms —
  controls against challengers on the development seeds — are not run here.
  They return to the search experiment once this mechanism merges. The
  dual-policy carry is temporary per the integrator's ruling above, scheduled
  for deletion after promotion.
