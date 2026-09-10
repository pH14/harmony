# CI01: inspect AP01's existing checkpoint at paused boundaries

AP01's fixed supplied-encounter pilot failed to defeat Ridley. Its champion is
not a census. Do not repeat that campaign or select a third diagnostic root.
Inspect the actual persisted checkpoint before choosing a generic hypothesis.

The new `metroid-checkpoint-inspect` command decodes current typed snapshots.
`inventory` initializes no emulator. The local AP01 inventory has 588 unique
cached IDs in 557 retention cells, with 171 classified endpoints among all
cached snapshots and 166 among the latest IDs per cell. It covers route frames
117875..119051. Raw boss HP is not present in the ordinary snapshot observation;
CI01 will read it through the existing paused-context getter, using one ordinary
constructor setup and no action calls. The E01 origin is a positive control.
Every restore/read must preserve the entire snapshot and physical frame counter.

## Why the active-ID mapping is justified for this run

This is a source-backed inference specific to AP01, not a new checkpoint API:

- `metroid/archive.rs` has one slot per exact `group(0)`, `slot_capacity() == 1`,
  and identity `complete()`. Optional motion metadata does not enter the group.
- The registered native challenge fixes default representative retention.
  `Archive::insert_after_observed` either rejects a new same-slot candidate or
  deactivates the incumbent and appends a strictly increasing stable ID. Duplicate
  inputs return the existing ID and do not create another admission.
- Other production `deactivate` paths are population/memory drops and liveness
  anchor handling. AP01 records zero entry drops and zero snapshot evictions.
  The population cap is 4,194,304, above all 1,032 admissions, so the special
  population-limited anchor retirement cannot occur.
- Starting at root length zero, the stream's parent links and at-most-six-action
  suffix law bound every admitted input at **156 actions**, below the 4,096 cap.
  This upper bound does not require recovering exact random draws. No active
  entry can disappear from the parent index through the action limit. Together
  with resident snapshots and absence of drops, the selector cannot become empty
  and reactivate an older anchor. Ordinary exhaustion counters reset the live
  walk, not active slot membership.
- Final cached-active diagnostics report no missing snapshots. Thus each cell's
  final active representative is present in the export and is its highest
  admitted stable ID. Inactive history may also remain cached. History compaction
  preserves stable IDs and active membership. The resulting 557 cells agree with
  both the independent active count and retention-context census; agreement is a
  consistency check after these preconditions, not the proof itself.

The offline scorer must enforce these recorded preconditions. If any fail,
report active membership as unresolved instead of applying the inference.

## Bounded native read and interpretation

Freeze source/build/checkpoint/origin/assets and one CI01 request before native
initialization. Use one CPU, at most 256 MiB process memory, 45 seconds child / 60
seconds service runtime and 8 MiB per output file. Hard input bounds are 32 MiB
and 1,000 unique entries. This separate read-only block permits at most 1,000
known auxiliary frames; expected work is exactly 929 constructor setup frames
and zero continuations. No search, seed sweep, full-campaign replay or retry is
allocated. Preserve hashes and raw context, not snapshot payload duplication.

Report classified snapshot-local HP distributions separately for active entries
and inactive cached history. Unclassified slots and HP255 are unavailable.
Differences from the origin's HP140 do not establish exact lifetime damage,
continuous episodes, recoverable progress or a retention cause. Report geometry,
Samus resources and same-slot comparisons to inform one next falsifiable generic
hypothesis. No HP-based reward, policy promotion or additional selected root
follows this measurement automatically.
