# A minimal prospective encounter observation

The no-cost depth screen, component screen and independent persistence
confirmation failed their gates. The next missing fact concerns actual boss
encounters in admitted work, rather than another precursor comparison. Existing
named-progress records cover milestones independently of retention, but omit
active enemy context. Eight E03 tapes and the separate NQ02 tape are selected
surviving trajectories, not a census of generated states. Their negatives do
not distinguish failure to generate an encounter from loss of such a state.

The smallest first measurement is whether a **live action endpoint** has a
snapshot-local boss classification, and a replayable input for the first such
endpoint. Do not add damage totals, encounter rewards, archive dimensions,
selection bonuses, a learned controller or a broad retention-history database.
Classification is not proof that the state can win a fight.

## Same-boundary requirement

`MetroidTarget::apply` receives every frame's WRAM from `run_action`, reads
cartridge RAM at the completed action endpoint, then decodes observations.
`decode_action_observations_with_policy` always emits the last frame unless it
breaks early at death. On a successful action with a live endpoint, its returned
`current_wram` therefore describes the same endpoint as that cartridge read.
Formally, for a completed n-frame action with no earlier terminal break,
the candidate pair is `(WRAM_n, cartridge_n)`. Existing intermediate observations
instead combine `WRAM_i` with `cartridge_n`; **do not classify those as same-time
snapshots**. A failed, empty or early-terminal action cannot satisfy the premise.

Before implementation, exercise this distinction with a planted within-action
slot change: a stale earlier WRAM attribute paired with final cartridge status
must never become an earlier encounter observation. Also cover an early death,
an empty/failed action and an ordinary live endpoint. Establish from the actual
candidate/snapshot path that the sampled endpoint is the producing state.

Reuse the existing `decode_context` and `boss_interval::classify` rules. F03's
positive reference traces establish the observed normal/hit/death cases; NQ02
checks native same-boundary reads and actual restores on a non-boss trajectory.
Neither alone is a positive native fight. Keep that missing end-to-end case
explicit; the first newly observed native episode must replay through the
standalone observer before supporting any stronger claim or local combat probe.

## Reporting and interpretation

Keep three conditions distinct: no eligible sample, an eligible endpoint with
no classified boss, and an eligible endpoint with classified boss slots. Merge
observed endpoints once with the ordered admitted action result, independently
of archive admission. Do not inherit a parent's encounter marker and recount it
as a fresh child observation. Bind a first positive input to its exact admitted
action, route endpoint and source/core/ROM identities.

A positive replay establishes that this campaign generated that observed
encounter state. It earns inspection of that particular episode and a separately
registered bounded local probe if useful. It does not establish an allocation
bottleneck, current archive recoverability or defeat reachability. Zero recorded
encounters in a completed budget describes eligible sampled endpoints only;
transient encounters between endpoints remain outside the measurement. Missing
or failed observations remain unavailable, not negative evidence.

If retention or selection becomes the next essential distinction, add only the
specific measurement needed after inspecting a qualified episode. Do not infer
either from a final cached maximum or a visited-map count.

## Qualification gate

This contract allocates no emulator frames and claims no performance gain.
First resolve the same-boundary and producing-snapshot premises using source
and executable fixtures. Any opt-in implementation must preserve default bytes,
input generation, terminal policy, archive keys and selection. Its identity and
memory/resource effects must be explicit. Native qualification then needs its
own bounded registration, using existing fixtures rather than reconstructing a
long campaign. No new long search, extra reference-movie sweep or unchanged
action-policy panel is earned by this design alone.

## Completed source and fixture check

`endpoint_context_must_not_pair_earlier_wram_with_final_cartridge` exercises the
real decoder and classifier. Both true frame boundaries have no boss, but the
earlier stale WRAM attribute combined with final cartridge status fabricates a
classification. The two emitted observations retain their distinct frame
numbers, and the cached live endpoint returns the final WRAM. Empty input yields
no new observation; a missing cartridge read is an error. The underflow test
also verifies that early death returns earlier WRAM rather than the final frame.
All four observation-module tests pass without emulator work.

In `execute_suffix`, a campaign candidate is snapshotted after `apply` and
read-only milestone/observation queries, guarded by `!dead && !failed`, before
any next action. `snapshot` captures emulator state and the current observation;
it does not run another action. This establishes the producing-state connection
for the intended live endpoint. Existing native snapshot/restore qualification
remains separate from these source and decoder checks. This source/fixture
checkpoint preceded the opt-in implementation below.

## Opt-in implementation

`metroid-boss-context-audit` attaches an optional six-slot mask only to the live
final observation. `None` means ineligible; `Some(0)` means a sampled endpoint
without classified boss context. Ordered admission merges constant-size counts
even for a rejected candidate. Origins do not count. The first positive event
reconstructs its producing input exactly once, independent of whether an output
path is configured. `nes-eval` verifies two identical replays ending at that
encounter and publishes their measured route-frame cost explicitly. Its fresh
genesis campaign guarantees that a producing prefix's earlier admitted positive
would already have been the first campaign event. This verification path must
be revisited before extending the evaluator to imported snapshot origins.

Versioned stream/checkpoint/result identities and an explicit observation-policy
entry distinguish feature builds. Default schemas remain unchanged. Added
snapshot metadata participates in existing size-based memory accounting; a first
encounter adds deterministic input reconstruction and two verification replays.
No blanket wall/memory neutrality claim follows from reporting-only ownership.
The local checks include fabricated mixed-time context, early death, missing and
zero samples, origin exclusion, rejected-candidate reporting, one reconstruction,
publication independence and rejection of a replay ending after its encounter.
Native qualification still needs a prospectively bounded registration. No new
performance allocation or positive native fight is claimed by this implementation.

The separately frozen [native Q01](../endpoint-encounter/README.md#q01-result)
now passes on msr1 and ms02. It checks exact default bytes, complete replay,
audited buffering equality and the declared physical/decision projections at
a reused 2,000-job fixture. It counts 6,628 eligible live endpoints and no
classified encounter. This qualifies the ordinary negative path only; it does
not supply a positive native episode or authorize unchanged performance sweeps.
