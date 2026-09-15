# Step 4: move the retained-input table from the SMB driver into the searcher

Base: the step 3 merge commit. Line numbers from `58d07433`; refresh before
editing, and expect `draw.rs` and the mixture identifiers to have changed in
step 3.

## What is wrong

The searcher's input draw mixes an alphabet sampler with a table of steps
folded from retained inputs. The table type, `EmpiricalStepTables` in
`src/search/empirical_steps.rs`, and the mixture in `src/search/draw.rs` are
generic. Everything that feeds, scopes, checkpoints and replays the table
lives in `workloads/nes/src/smb/campaign.rs`, about 600 lines plus tests, so
only Super Mario Bros has it. Metroid, Mega Man 2, Nova, Super Tilt Bro and
the fault workloads pass a sampler that always returns `None` and draw from
the alphabet alone.

The game interface is meant to be the key, the preference, the progress
relation and the action alphabet. The SMB driver carries searcher logic.

## What it becomes

The searcher owns the table end to end. A workload declares its alphabet
sampler and, optionally, the archive depth that scopes the table. Every
other input-policy method keeps a default.

| Today in `smb/campaign.rs` | After, in `dissonance/searcher/src/search/` |
|---|---|
| `SmbDrawState { tables, versions }` (284) | `DrawTables<A>`, one concrete type for every workload |
| `SmbCampaignChordPolicy`, `SmbChordTableDerivation`, `chord_policy_identifier` (562), `chord_policy_from_identifier` (588) | one searcher policy identifier for the table parameters, recorded per run in `WorkloadPolicies` beside the selector and mixture identifiers |
| `SmbChordSource::{All, Level}` (519) and the source filter (511) | scope by archive group: `None` for one table per run, `Some(depth)` for one table per `group(depth)`; a draw uses the parent's group |
| `initial_chord_tables` (648) and `SmbChordTableHeader` | fold an origin archive's retained inputs into the initial tables by scope; the header carries source hash, parameters and initial checkpoint |
| `recorded_chord_tables` (723), `remember_chord_version` (746), `SmbChordTableVersion` | same logic, generic over the action type |
| the `InputPolicy` impl (848): `initial_draw_state`, `draw_checkpoint`, `expand_suffix`, `expand_suffix_recorded`, `finish_stream_record`, `remember_draw_version`, memory accounting | default methods on `InputPolicy` (`campaign.rs` 186-300) written once against `DrawTables` |
| `ChordFoldSource::{SuffixOnly, FullInput}` and `retained_inputs_need_full` | keep the one the SMB default uses today; delete the other |
| the tests in `smb/campaign.rs` that exercise fold, versions and replay | searcher unit tests over a test action type |

`CampaignTypes::{DrawState, DrawCheckpoint, DrawHeader}` (`campaign.rs`
150-152) become the searcher's concrete types and leave the trait.

Default scope is `None`, one table per run, which is what SMB's default
policy `SmbChordSource::All` does today. Default parameters are SMB's
current defaults: `prefix_steps 0, recent_successes 128, recent_weight 3,
all_history_weight 1, update_every_records 64, hash_every_records 1024`.

## Steps

### 1. Move the generic parts into the searcher

Create `src/search/draw_tables.rs` holding `DrawTables<A>`: the tables, the
version map, fold on `finish_stream_record`, checkpoint, recorded lookup,
version reuse, and memory accounting. Move the SMB test cases for these
across, generic over a small test action.

### 2. Defaults on `InputPolicy`

Give `initial_draw_state`, `draw_checkpoint`, `expand_suffix`,
`expand_suffix_recorded`, `finish_stream_record`, `remember_draw_version`,
`draw_state_memory_bytes` and `draw_state_memory_reserve_bytes` default
bodies written against `DrawTables`. Add two methods a workload may
override: `sample_alphabet(&self, run, rand) -> Action`, required, and
`table_scope_depth(&self) -> Option<usize>`, default `None`.

### 3. Policy identifier

Add the table identifier to `WorkloadPolicies` in the searcher, recorded on
every run, with an `off` value for paired measurement only. It is on by
default for every workload.

### 4. Origin seeding

When a campaign starts from an archive origin, fold that archive's retained
inputs into the initial tables by scope, as `initial_chord_tables` does for
SMB today, and record the header.

### 5. Bounds

One table per group can grow without limit at a fine scope depth. Cap the
number of tables, evict the least recently folded, and charge them through
the memory accounting. Fine scope depths are for measurement, never the
default, until the cost is known.

### 6. Strip the drivers

- SMB: delete the moved code. Keep `sample_chord_from_masks` and the
  vocabulary as the alphabet sampler.
- Metroid (`campaign.rs` 678-683), Mega Man 2 (613-618), Nova (535-540),
  Super Tilt Bro (506-511), faults (`workloads/faults/src/campaign.rs`
  460-465): delete the draw-state and suffix boilerplate; supply the alphabet
  sampler only.
- Each recorded stream now carries a draw checkpoint. Bump the stream format
  identifier where a workload declares one.

### 7. Tests

- The moved unit tests pass against the test action.
- Contract: a recorded stream from every workload replays bit-identically
  with the table on.
- Contract: with scope `Some(depth)`, a draw from a parent in one group never
  reads steps folded from another group.
- Contract: a workload with the table `off` draws from the alphabet only and
  replays.

### 8. Docs

Rewrite the input-policy part of `dissonance/searcher/README.md`. Search
the tree for any doc that names the SMB chord policy identifiers and update
it. Each driver README says the workload supplies an alphabet and nothing
else about drawing.

## Checks after merge

From `README.md`: local checks, SMB regression, quick panel, long panel.

The SMB regression is the exact check for this step. With the default scope
and parameters the searcher reproduces SMB's current table, so the per-seed
victory execution counts and the recorded stream hashes must match the step
3 run of the same manifest on the same build settings. If they differ, the
draw sequence changed; find why before the quick panel.

On the quick panel the other games now draw from a table for the first
time. Watch the film for Metroid and Mega Man 2 before reading numbers. A
paired run with the table `off` on the same seeds is the comparison, and it
is the one place the `off` value is used.
