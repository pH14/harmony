<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Sol common-parent fork canary

Status: preregistered before graph census or candidate emulation.

## Question

The coherent-replacement canary was a decisive null: arbitrary donor and
recipient states made 198 of 200 candidates die before donor order mattered.
This canary tests the next structural hypothesis: a coherent retained phrase
can be recombined with an existing tail when the phrase begins at the exact
retained parent state from which it was originally discovered.

The mechanism is game-neutral. It uses only stable ancestry identifiers,
exact parent snapshots reconstructed from an input prefix, opaque recorded
actions, action counts, seeded hashes, and target-reported outcomes. It may not
inspect or branch on button values, hold durations, route, coordinates,
world/level, progress windows, images, walkthroughs, or operator action
knowledge. Opaque action serialization, hashing, equality, ordering into a
multiset, and hold-total accounting are permitted only for identity, neutral
ranking, control construction, and pair-integrity checks; no action field may
be interpreted semantically or used to prefer an outcome.

## Frozen artifacts

- Code base before this experiment: `6820d141`.
- C119 stream SHA-256:
  `ab869286a526dab104f7846ae0313745de7087e3733e99016218defb42e90201`.
- C119 archive provenance SHA-256:
  `d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81`.
- Recovered C119 production binary SHA-256:
  `87fb11f300a7af9386eb06c8b55e7a7353d6cb3654b83ee6a5615806e72e2862`.
- Selected entry: `48076`, created exactly once at execution `49709` with
  reconstructed parent `29805`, with 3,297 actions and semantic input SHA-256
  `584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1`.
- Compact source byte SHA-256:
  `5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159`.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Verified baseline: alive at `(7,0,236)` after 155,148 deterministic frames.
- Fixed shuffle seed: `0xcdb1f18a3e80ad18`, derived from the first eight
  bytes of SHA-256(`sol-restart-c119-common-parent-fork-v1`) interpreted as a
  little-endian `u64`.

C119's header must name the frozen `one_or_two` suffix, `down_ten_mask`
vocabulary, and stratified duration distribution. `chord_policy` must be
absent and decode through the frozen serde default to uniform; `chord_table`
must be absent. No derived empirical table is permitted.

## Exact retained-edge fold

Read at most 64 MiB plus one byte of the newline-delimited stream. Reject more
than 100,000 records, two decisions per Job, 131,072 retained edges, or
1,000,000 enumerated path states; every count and range uses checked integer
arithmetic. Re-derive every
Job's one-or-two action suffix from its recorded mutation seed using the exact
production helper and header vocabulary. Decisions map to the leading live
action boundaries and may not outnumber suffix actions.

Start each Job at its recorded parent with an empty pending phrase. Append the
next opaque action before consuming its decision:

- `Rejected`, `ProbeRefused`, and `SnapRefused` leave the current parent and
  pending phrase unreset, so the just-appended action remains in that phrase.
- `Duplicate { id }` changes the current parent to `id` and clears the pending
  phrase, but creates no edge.
- `Retained { id }` creates one edge from the current parent to `id`, labelled
  by the entire nonempty pending phrase, then changes the current parent to
  `id` and clears the pending phrase.

Retained IDs must be unique and append-ordered, and every parent must be older
than its retained child. Skips create no edge.

Walk entry 48076 backward through these edges. Infer each parent's action
count by subtracting its edge length from the child's count, and require every
edge label to equal the corresponding slice of the compact source. The first
missing parent is the one permitted origin root; the remaining source prefix
is its exact input. Only common parents in the generic trailing 256-action
horizon, derived as `input_length - 256 .. input_length`, are eligible.

## Frozen census and recipes

For each eligible winning-lineage parent, enumerate descendant paths beginning
with a retained child other than the winning-lineage child. Follow only
retained edges and preserve edge and action order. A path is eligible at arm
length `L` only when its edge-label lengths sum exactly to `L`, where arms are
fixed at `[4,8,16,32,64]`. The parent's source suffix must contain at least `L`
actions. Deduplicate identical `(parent action count, opaque donor actions)`
materializations.

The read-only census runs before emulator construction and exposes only counts
and a digest of canonical identities. Its implementation is the exact planner
used by the live binary; census mode exits before loading the ROM. If any arm
cannot fill every slot below, STOP without emulation or recipe relaxation.
The canonical census identity list contains every distinct nonvacuous path as
the exact tuple
`(L_u64, parent_id_u64, parent_action_count_u64, child_ids_Vec_u64,
donor_semantic_sha256_lowercase_hex_String)`, sorted by ordinary tuple order.
Its digest is SHA-256 of `serde_json::to_vec` of that complete `Vec` with no
newline.

Rank each path by SHA-256 of `serde_json::to_vec` (no newline) applied to the
exact Rust tuple
`("fork-rank-v1", L_u64, parent_id_u64, parent_action_count_u64,
child_ids_Vec_u64, donor_actions_Vec_ButtonChord)`.
Sort by `(rank, parent_id, parent_action_count, child_ids)`. For each arm take
slots `j = 0..19` in arm order `[4,8,16,32,64]`; arm index is `a` and global
draw is exactly `d = 5*j + a`. For each slot, scan forward through that arm's
ranked unused paths. Permanently advance past a path whose control cannot be
made valid in retries `0..=256`, and choose the first valid path. Interleave
the chosen recipes by increasing `d`, yielding exactly 100
outcome-independent pairs. The census gate counts these control-valid selected
recipes, not merely coherent paths.

For draw `d`, derive a Fisher-Yates permutation from
`SHA-256(seed_u64_le || ASCII("fork-shuffle") || d_u64_le || retry_u64_le ||
upper_index_u64_le)`,
using the first eight bytes as a little-endian integer reduced modulo the
current range. Initialize the permutation as `[0,1,...,L-1]`; visit `upper`
from `L-1` down through `1`, compute `other = digest_u64 % (upper+1)`, and
swap indices `upper` and `other`. Retry only the permutation until the shuffled phrase differs
from both the coherent phrase and the source phrase it replaces. More than 256
retries rejects that path and advances to the next ranked path. The coherent
phrase must also differ from the source phrase.

## Candidate execution

For a common parent at source action count `P` and phrase length `L`:

- coherent = `source[..P] + retained_sibling_phrase + source[P+L..]`;
- control = `source[..P] + shuffled_identical_multiset + source[P+L..]`.

Both candidates remain exactly 3,297 actions. Their prefix, untouched tail,
replacement length, donor multiset, and donor hold-frame total must match.
Both replacements must differ from the source and from each other.

Replay the source once, keeping exact snapshots every 32 actions. Each pair
restores the nearest snapshot at or before `P`, replays the unchanged prefix to
the exact common-parent boundary, and then evaluates its candidate. Absolute
frames must reconcile as snapshot gameplay-genesis frames plus post-restore
frames. Use 12 persistent workers, six complete pairs per batch (the final
batch may be shorter), fixed candidate ordinal assignment `ordinal mod 12`,
and buffer both successes and failures for ascending-ordinal consumption.
For draw `d`, coherent is candidate ordinal `2*d` and shuffled is ordinal
`2*d+1`; batches contain consecutive complete draws, submit coherent then
shuffled, and consume coherent then shuffled. Recipes and outputs are
canonical and outcome-independent.

Every candidate record must contain its draw, arm, complete recipe, semantic
input hash, deterministic trace hash, full final snapshot/state hash, absolute
and post-restore frame counts, restored snapshot action and absolute frame
count, executed action count, death/failure status, and maximum target tuple.
Summarized-only outcome records are an integrity failure.

There is no routine campaign/archive replay audit and no automatic or
unpreregistered larger run after seeing this result. A completed report records the source, stream, ROM,
executable, configuration, census, recipe-set, body, and whole-report digests.

## Frozen decision

`useful` retains the prior absolute rule: a nonterminal candidate must either
reach strictly beyond `(7,0,236)` in no more than 155,148 frames, or reach
exactly `(7,0,236)` in fewer than 155,148 frames.

`nonterminal` means `death == false`, `failure == false`, and exactly 3,297
actions executed. After exactly 100 pairs, GO only if all three hold:

1. at least 10 coherent candidates are nonterminal;
2. coherent has at least five more nonterminal candidates than shuffled; and
3. coherent has at least one useful candidate.

Otherwise STOP and record exact-entry coherent recombination as null at this
scale. A GO authorizes one exact same-binary rerun; its canonical report must
be byte-identical before a separately preregistered campaign experiment is
allowed. A mismatch is an integrity stop. This canary never directly promotes
a policy.
