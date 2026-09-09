# Bounded context representatives after P04

P04's frozen coarse descriptor separates six of eight useful pairs still merged
by the finer position/pose key: pairs 0, 3, 5, 6, 12 and 13. Pair 0 has identical
recorded mechanical state, but differs in horizontal speed sign and has a
distinguishing existing suffix (trial 15). Pair 4 has identical values even in
the seven-byte motion tuple and still has useful disagreements. Motion is
therefore a concrete candidate descriptor, not sufficient Markov state or an
identified sole cause of the observed differences. All 32 endpoints replayed
twice and retained unchanged snapshots during reads, costing 1,587,338 physical
frames. No new suffixes or search campaigns were run in P04.

The next design preserves the existing retention slots and all selection
groups. Within a slot, retain the ordinary quality maximum and the best-quality
state with a different opaque motion context, at most two representatives under
the same byte budget. No facing or velocity sign is intrinsically preferred.
An ordinary top-two-quality policy is the capacity control; it distinguishes
the descriptor choice from merely allowing another representative. Production
retention remains a separate comparator where needed.

For a fixed candidate stream, each context has a quality maximum. The proposed
rule keeps the two highest-quality context maxima. It can update exactly from
the current two winners plus a candidate: if the candidate improves an existing
winning context, replace that context's maximum; if it creates or improves an
excluded context enough to enter the top two, discard the lower winner. Any
excluded context's previous maximum was below both current winners and cannot
become relevant without a newly observed improvement. This induction assumes
stable quality/context and excludes external evictions/imports and exact-input
deduplication, as the earlier streaming claims do.

This is not a reachability guarantee. A third, lower-quality context can contain
the only useful continuation and be discarded. Two states sharing a context
can also have different futures. Finite fixtures must include both a case where
the context distinction preserves a lost motion-dependent exit and a case
where this capacity limit loses an exit. Test the actual archive implementation,
not a separate idealized retention function. Full production coordinator replay
must exercise real alternatives, memory pressure and continuation dispatch.

Implementation boundary: add an optional opaque context method to the generic
archive key contract, plus explicit two-quality and two-context policy IDs.
Metroid can derive the three-byte context from its already cached current WRAM
at candidate creation and snapshot-key reconstruction. A versioned, opt-in
build feature may add the context to its key; it must not change observations,
snapshots, geometry, reward, terminal handling or action vocabulary. Both
experimental arms use the same feature-enabled binary and metadata. The
feature-disabled build must preserve historical behavior and streams. Do not
reinterpret the existing raw pose byte to hide the new context. Missing opaque
contexts follow the ordinary retention rule; they do not become fake contexts.

Before a fresh campaign: implement the bounded rule, its finite positive and
negative examples and replay checks; qualify the context read from cached WRAM
against P04's direct reads; verify versioned identities and default compatibility.
Then specify a small distinguishing pilot and its work limit before running it.
P04's pass authorizes that design and qualification work only. The failed K01
and Metroid R03 campaign gates remain recorded and are not reset. No long run,
default change or held-out panel is yet justified by this proposal.

## R04 registration, before ARM data

The small distinguishing pilot is two fresh 5,000-job campaigns (development
seed 3, four workers, 8 GiB, existing K01 smoke configuration, corrected v3
terminal handling). Both use the identical motion-feature binary. Compare
`quality_representatives_2_v1` with `context_representatives_2_v1`, sequentially
on cores 0–3. Each has a 240-second search limit plus 120-second verification
allowance. Full campaign replay must reproduce the report and checkpoint.
The qualification-only resident-snapshot census must show all keys carrying
contexts, at most two snapshots per slot, no same-context pair under context
retention, at least ten same-context pairs under quality retention, and at least
ten distinct-context pairs under context retention. This checks real opportunity
and mechanism activity, not future coverage or progress. Memory eviction can
make the census a subset of active entries; its scope is explicitly resident
snapshots. Existing P04 suffix results supply the behavioral motivation.

Before those cells, the feature-disabled binary must reproduce the frozen
5,000-job corrected Metroid, MM2 and Metroid job-sampling streams exactly.
Reconstruct original P04 pairs 0, 2 and 12 twice per endpoint to check cached
context and actual campaign key against the previously qualified direct RAM
read, including stationary, horizontal and vertical sign classes. No new
suffixes, pair selection or descriptor tuning is allowed in this check.

Only if all checks pass, run one fresh development pair at seed 3, quality
then context on cores 0–3: same feature binary, 8 GiB, four workers, alphabet
draws, 4,096 actions, 500,000-job/50-million-frame ceilings, and 1,800-second
search limit per arm plus 120 seconds for finishing. Stop at the first bound;
no retries or longer run on a negative result. Replayed new named milestones
or at least 20% less admitted work to a common missile/Norfair/tank milestone,
without losing a milestone reached by the completed control, can qualify one
new development seed. Wall-censored relevant work remains unresolved, never
a win. Boss defeats are stronger evidence but still require fresh replication.
If this gate fails, retire this motion-context family for this tranche.

## Qualification measurement correction (R04b)

R04's first three compatibility cells passed. Its quality cell's resident
snapshot census counted four entries in a slot: the checkpoint includes
inactive reconstruction anchors, so snapshot residency is not the active
retention set. This invalidates that census as a capacity check. Preserve all
five completed short cells and their work; do not relax the two-entry gate.

R04b moves the final census to the coordinator's actual active-key set,
independently of snapshot residency. An executable archive fixture explicitly
leaves superseded historical metadata and checks that it is excluded. Policies,
descriptor, seed, all limits and quantitative thresholds are unchanged. Rebuild
both binaries and rerun only the same five short qualification cells under new
output names. In addition to all historical stream hashes, require both new
policy streams to match R04 exactly, proving that this measurement repair does
not alter their search choices. The subsequent development pair remains gated.

Before any development cell, J03 completed and released the faster cores 8–11.
Amend only development placement to those cores for both arms, still quality
then context sequentially. This avoids spending roughly twice as long on the
slower group, without changing the seed, executable, work, memory, wall limits,
arm order or scientific gate. Qualification remains on 0–3. The driver records
placement explicitly; no fresh-development outcome informed this change.

## R05: one fresh seed with both controls

R04b passed at common 50M work: context retention found a replayed energy tank
in (36,975,176, 36,987,803] frames; quality retention found none. Neither arm
lost another named milestone or hit a wall limit. This qualifies one new
development seed, 20261210, absent from both committed research ledgers; the
launcher additionally rejects any existing msr1 evaluation summary with that
seed. This remains development, not held-out validation.

Run three fresh arms in fixed order ordinary/context/quality, sequentially on
cores8–11. All use the identical attested `context-002` executable, ordinary
16-pixel geometry, corrected terminal v3, alphabet-only/one-to-six draws,
4096 actions, four workers,8GiB,500k jobs/50M frames/1800s per arm, plus120s
finishing allowance and4GiB output cap. No imported gameplay input or resumed
archive is allowed. The added ordinary-retention arm checks whether the
replicated effect matters against production retention, while matching motion
metadata across all arms. Total service cap6000s/12GiB; no retries on a negative.

Evaluate all three at their common completed frame boundary, at most50M.
Against each control require an additional replayed named milestone or a
conservative20% reduction to a common missile/Norfair/tank milestone, with no
lost control milestone and no wall-censored arm. Report capacity replication
and ordinary-control advantage separately. Only passing both comparisons
qualifies specifying one longer development pair; its budget must be registered
before it starts. Otherwise stop longer motion-retention campaigns in this
tranche. This single fresh seed never establishes the breakthrough threshold.
