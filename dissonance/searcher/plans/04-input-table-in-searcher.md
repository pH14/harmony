# Step 4: move the retained-input table from the SMB driver into the searcher

Base: the step 3 merge commit. Line numbers from `58d07433`; refresh before
editing, and expect `draw.rs` and the mixture identifiers to have changed in
step 3.

## What is wrong

The searcher's input draw mixes an alphabet sampler with a table of steps
folded from retained inputs. The table type, `EmpiricalStepTables` in
`src/search/empirical_steps.rs`, and the mixture in `src/search/draw.rs` are
generic. Everything that feeds, checkpoints and replays the table lives in
`workloads/nes/src/smb/campaign.rs`, about 600 lines plus tests, so only
Super Mario Bros has it. Metroid, Mega Man 2, Nova, Super Tilt Bro and the
fault workloads pass a sampler that always returns `None` and draw from the
alphabet alone.

The game interface is meant to be the key, the preference, the progress
relation and the action alphabet. The SMB driver carries searcher logic.

## What it becomes

The searcher owns one table per run for every workload, which is what
SMB's default policy does today. A workload declares its alphabet sampler.
Every other input-policy method keeps a default.

| Today in `smb/campaign.rs` | After, in `dissonance/searcher/src/search/` |
|---|---|
| `SmbDrawState { tables, versions }` (284) | `DrawTables<A>`, one concrete type for every workload |
| `SmbCampaignChordPolicy`, `SmbChordTableDerivation`, `chord_policy_identifier` (562), `chord_policy_from_identifier` (588) | one searcher identifier for the table parameters, recorded per run in `WorkloadPolicies` beside the selector and mixture identifiers |
| `SmbChordSource::{All, Level}` and the source filter (511, 519) | deleted; every retained input folds into the one table |
| `initial_chord_tables` (648) and `SmbChordTableHeader` | the campaign folds an origin archive's retained inputs into the initial table where `Evaluation::source_entries` (`campaign.rs` 418) is available; the header carries source hash, parameters and initial checkpoint |
| `recorded_chord_tables` (723), `remember_chord_version` (746), `SmbChordTableVersion` | same logic, generic over the action type, with versions dropped after their last recorded use |
| the `InputPolicy` impl (848): `initial_draw_state`, `draw_checkpoint`, `draw_checkpoint_version` (936, returns `checkpoint.records`), `expand_suffix`, `expand_suffix_recorded`, `finish_stream_record`, `remember_draw_version`, memory accounting | default methods on `InputPolicy` (`campaign.rs` 186-300) written once against `DrawTables` |
| `retained_inputs_need_full` (995, returns false) and the full-input branch in the searcher (`campaign.rs` 3156-3190) | deleted; the fold is suffix-only |
| the tests in `smb/campaign.rs` that exercise fold, versions and replay | searcher unit tests over a test action type |

`CampaignTypes::{DrawState, DrawCheckpoint, DrawHeader}` (`campaign.rs`
150-152) become the searcher's concrete types and leave the trait.

Default parameters are SMB's current defaults: `prefix_steps 0,
recent_successes 128, recent_weight 3, all_history_weight 1,
update_every_records 64, hash_every_records 1024`.

Whether a draw consults the table is decided by the mixture identifier, as
today: `alphabet_only` never does, `energy_splice` does through its
strategy weights (`draw.rs` 302). So a run with the table and a run
without one are two mixtures, and no new identifier value is needed.

## Steps

### 1. Move the generic parts into the searcher

Create `src/search/draw_tables.rs` holding `DrawTables<A>`: the table, the
version map, fold on `finish_stream_record`, checkpoint, checkpoint
version, recorded lookup, version reuse, and memory accounting. Move the
SMB test cases for these across, generic over a small test action.

Before deleting the SMB code, add a test in `workloads/nes/src/smb/` that
folds one recorded set of retained inputs through both `DrawTables` and the
SMB code and asserts the `table_sha256` of every checkpoint is identical.
Keep it until step 6 deletes the SMB code, then delete the test with it and
say in the pull request that it passed.

### 2. Defaults on `InputPolicy`

Give `initial_draw_state`, `draw_checkpoint`, `draw_checkpoint_version`,
`expand_suffix`, `expand_suffix_recorded`, `finish_stream_record`,
`remember_draw_version`, `draw_state_memory_bytes` and
`draw_state_memory_reserve_bytes` default bodies written against
`DrawTables`. Add one required method, `sample_alphabet(&self, run, rand)
-> Action`, which `expand_suffix` passes to `draw_suffix` as the alphabet
sampler; the biased sampler is the table.

### 3. Policy identifier

Add the table parameter identifier to `WorkloadPolicies`, recorded on every
run.

### 4. Versions

Versions exist for replay only: `remember_draw_version` is called at
`campaign.rs` 3354, 3564 and 3784, all in replay. Replay collects every
required version up front (3247-3261). Turn that set into a map from
version to the index of the last record needing it, and drop a version
after that record replays. Versions are not charged to the memory budget,
since replay memory must equal live memory (3778); the last-use rule is
what bounds them.

### 5. Origin seeding

When a campaign starts from an archive origin, fold that archive's retained
inputs into the initial table, as `initial_chord_tables` does for SMB
today, in the campaign code that already holds the origin report, and
record the header.

### 6. Strip the drivers

- SMB: delete the moved code. Keep `sample_chord_from_masks` and the
  vocabulary as the alphabet sampler.
- Metroid (`campaign.rs` 678-683), Mega Man 2 (613-618), Nova (535-540),
  Super Tilt Bro (506-511), faults (`workloads/faults/src/campaign.rs`
  460-465): delete the draw-state and suffix boilerplate; supply the
  alphabet sampler only.
- Every recorded stream now carries a draw checkpoint. Set
  `CAMPAIGN_SCHEMA_VERSION` to 5.

### 7. Tests

- The moved unit tests pass against the test action.
- Contract: a recorded stream from every workload replays exactly.
- Contract: replay of a long stream with many checkpoint versions holds at
  most the versions still needed; assert the version map shrinks as the
  replay advances.
- The SMB equivalence test from step 1 passed before its deletion.

### 8. Docs

Rewrite the input-policy part of `dissonance/searcher/README.md`. Search
the tree for any doc that names the SMB chord policy identifiers and update
it. Each driver README says the workload supplies an alphabet and nothing
else about drawing.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel, long panel.

The SMB regression is the exact check for this step. SMB's draws are the
same code and the same parameters as before, so for every seed and memory
budget the step 4 run must match the step 3 run of the same manifest on
the same build settings in two places: `executions_to_first_victory` and
`frames_to_first_victory` in the run results, and every record of
`campaign/progress.jsonl` in the run directory, field by field, apart from
wall-clock fields. Identical draws give identical admissions, and the
progress records carry the admission, entry and cell counts at every
progress point. The raw stream hash is not the comparison; the header and
record format changed. If anything differs, find why before the quick
panel.

On the quick panel the other games draw from a table for the first time,
and the step 3 quick panel is the comparison. Watch the film for Metroid
and Mega Man 2 before reading numbers.

Long panel: run `benchmarks/search/metroid-long-horizon-energy-splice.json`,
committed in step 3, and compare with step 3's run of the same manifest.
Both runs use `energy_splice:6`, so splicing and continuation are the same
in both and the table is the only difference. Compare by film, map images,
then numbers. The `alphabet_only` manifest never consults the table and is
not run in this step.
