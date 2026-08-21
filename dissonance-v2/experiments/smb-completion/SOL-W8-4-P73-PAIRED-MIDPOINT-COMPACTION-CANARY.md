<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol World 8-4 p73 paired midpoint-compaction canary

Status: preregistered after the p73 ordinary-harvest STOP result commit and
before implementation, recipe materialization, ROM loading, or live emulation.

## Frozen question and source

The ordinary full-source B1 policy advanced to World 8-4 p73, then a fresh
6,144-candidate continuation produced no final-active endpoint above that
source. Test the remaining generic selector-dilution hypothesis: after a fixed
midpoint, does restarting from the mechanically best retained endpoint and
discarding the accumulated selectable population improve the next fixed block
of otherwise identical one-action work?

This is one paired canary: 12 independent pairs, 128 frozen slots per arm.
`FULL` preserves its normal archive and selector accounting. `COMPACT` behaves
identically for slots0..63, then rebuilds a fresh singleton archive from its
deterministically ranked midpoint champion before slots64..127. The treatment
therefore tests the explicit restart/compaction bundle—active-population
deletion plus selector-accounting reset—not archive-capacity relief. Proposal,
target, probing, and admission mechanics remain identical and opaque.

- Code/result commit `fc62d470395bfaa84a89e0b03ce22f503630be07`.
- Authorizing p73 prereg `fbf2afb1`, implementation `c3902b4a`, result
  `fc62d470`, report SHA
  `5fc888c8fcb522b9b1216de9649223cebbddbf87709e68d1236a4e2031ff2e90`.
- Source
  `/root/harmony-smb-sol-w8-4-p61-harvest-v3-3aaeb783/results/adopted-world-8-4-progress-73-input.json`;
  compact/semantic SHA
  `d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c`;
  114,128 bytes; 3,554 actions.
- Alive Ok replay maximum/endpoint `(7,3,73)` at 167,340 frames;
  mechanical `(7,3,73,y=8,engine=8,dead=false,flag=false)`; key
  `(7,3,73,8,8,fingerprint=60)`; milestones `(195,true,true,true)`.
- WRAM SHA `bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734`;
  snapshot SHA `3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927`.
- Final chord `{buttons:0,hold_frames:3}`; mask00 survives exact 45-frame
  source probe. ROM SHA
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Seed label `sol-restart-w8-4-p73-paired-midpoint-compaction-v1`; SHA
  `83234f699265b0c82ff967e63e9410bd2e9c0f35ce75a2bafe5c7e006475509f`;
  first8 little-endian master `14461170082993087363`.

Binary `smb-w8-4-p73-midpoint-compaction-canary` takes input/output paths and
ROM only via `HARMONY_SMB_ROM`. Bound reads 2/16 MiB. Replay/verify all source
facts, source-probe, restore/re-hash, and record trace before recipes/workers.
It must not read any prior report. Mismatch is integrity STOP.

## Frozen recipes

For pair `r=0..11`, slot `s=0..127`:

```text
pair_seed = first8_le(SHA256(master_le || "w8-4-p73-compact-v1-pair" || r_le))
index = first8_le(SHA256(pair_seed_le || "w8-4-p73-compact-v1-action" || s_le)) mod 3554
selector_seed = first8_le(SHA256(pair_seed_le || "w8-4-p73-compact-v1-parent" || s_le))
```

Copy the full opaque source chord. Both arms receive the exact same ordered
`(index,chord,selector_seed)` slot stream. No retry, filter, dedup, semantic
inspection, or outcome feedback. Hash exact pair-major serde tuples
`(r,s,index,chord,selector_seed)`. Each pair projection is one bare slot-ordered
`Vec<(s,index,chord,selector_seed)>`; require all 12 exact projection byte
vectors pairwise distinct before workers, no retry.

## Execution and midpoint intervention

Each of the 24 arms begins with a fresh normal archive containing only the
trusted exact source as id0/parentNone/execution0. Policy is action limit4096,
archive129, Frozen key, ProbeAtAdmission45 `[00,01,81]`, FewestActions, real
ConcentratedRecency, absent waypoint/snapback/pin/update. Every slot uses a
fresh registered selector RNG, exactly one real selection, verified restore,
one full action, normal candidate probe/restore and ordinary admission, then
exactly one `record_selection` and one `record_selection_outcome` with realized
slot work. New allocation/replacement is productive; duplicate/refusal/death/
rejection is not. Ok-death consumes only that slot; non-Ok/error is integrity
STOP. Slots0..63 and their archive/target/accounting evidence must be byte-equal
between paired arms except arm labels; mismatch is integrity STOP.

After both arms consume slot63 and before either selects slot64, rank each
arm's final-active entries by full target watermark descending, actions
ascending, semantic input SHA ascending, then id ascending. Pre-intervention
paired evidence must yield the same champion input/snapshot/key/milestones.
`FULL` changes nothing. `COMPACT` constructs a fresh `Archive::new` with the
same policies and inserts that exact trusted champion directly as id0,
parentNone, execution0, active, with no probe/emulation; it verifies the fresh
singleton archive and resets selector accounting. All other entries, active or
inactive, are absent. Preserve external provenance from each post-midpoint
allocation to its pair, arm, slot, original selected parent, and exact input.
Created execution remains `s+1`. Inputs may contain at most `3554+128=3682`
actions, strictly below4096. Origin plus at most128 allocations fits archive129.

Exactly 12 persistent workers handle arm ordinal `(2*r + arm)` modulo12;
`FULL` precedes `COMPACT`. Workers emit no bytes. Coordinator buffers all 24
records, validates worker ownership/duplicates/missing/error replies, consumes
and reports pair-major/arm order, and selects the lowest canonical error.

## Work, gate, and adoption

Require exactly 3,072 scheduled/executed candidates and selection-accounting
events. Caps: action368,640; candidate probe414,720; replay167,340; source
probe45; 13 target setups4,693; **955,438 total frames**. Use checked exact
component, arm, worker, and global reconciliation; no wall authority.

An arm score is the greater of the immutable source watermark and its maximum
final-active newly allocated alive probe-surviving ordinary endpoint. Compare
the full `SmbProgressWatermark::Ord`. Ties are noninformative. Let `w` be
COMPACT wins, `l` FULL wins, `n=w+l`, and
`N=sum[k=w..n] C(n,k)`, `D=2^n`, all checked `u128`.

`PROMOTE_COMPACTION` iff, in this order:

1. `n>=8`;
2. `w>l` and exact one-sided `80*N <= D`; and
3. at least one final-active newly allocated post-midpoint COMPACT endpoint is
   alive/probe-surviving, strictly greater than `(7,3,73)`, and strictly
   greater than its paired FULL arm score.

`INCONCLUSIVE_SPARSE` iff `n<8`; otherwise `RETAIN_FULL`. No repeat, threshold
relaxation, or pooled prior evidence. A promotion authorizes only a separately
registered compact-restart search policy; it does not automatically import a
treatment endpoint.

Adoption is orthogonal. Across both arms, rank final-active newly allocated
alive probe-surviving ordinary endpoints by full watermark, fewest actions,
input SHA, pair, `FULL` before `COMPACT`, then id. **ADOPT** the sole exact
champion iff strictly greater than `(7,3,73)`, regardless of the structural
verdict; otherwise **NO_ADOPT**. Create-new canonical NDJSON binds header,
baseline, recipes, arms, pair classification, structural verdict, adoption,
summary, and all identity/config/recipe/trace/body/file hashes without paths or
timestamps. Terminal-like evidence remains diagnostic pending a separately
frozen mechanical credits predicate and artifact-only confirmation.
