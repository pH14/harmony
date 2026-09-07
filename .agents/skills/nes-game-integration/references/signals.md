# Design observations without teaching a solution

## Mechanical knowledge is still an inductive bias

An opaque key keeps game names out of the engine, but its grouping and ordering
still determine what the engine explores. Review adapter policy as carefully
as core policy. Source code and RAM maps are evidence for mechanics; walkthroughs
are not a source of search strategy.

A field being easy to decode does not mean it should affect search. Keep rich
diagnostics in reports, and give each search-visible field a concrete reason.
A useful test: would this field/order remain meaningful after rearranging the
rooms, changing a boss's weakness, or moving an item?

## Distinguish identity, progress, and preference

- Location IDs are labels. In a game with backtracking, larger room IDs, map X,
  or map Y do not imply progress. Inspect the current archive's comparison
  path before implementing ordering. Some branches expose `progress_cmp`;
  others use the key's `Ord` for progress as well as identity. Use an available
  progress hook deliberately. If those roles cannot be separated in the current
  API, document the limitation and propose the smallest generic change; do not
  invent a trait method or silently reward derived coordinate/state-ID order.
- Durable inventory/clear flags describe achievements. Counts are compact but
  may alias different abilities; preserve identity where two equal counts
  give different capabilities. Validate a proposed split before adopting it.
- Health and ammunition often belong in same-location preference, not separate
  dimensions multiplying every spatial slot. A lexicographic preference is
  a policy choice: missiles-first can starve health; health-first can erase
  useful ammunition. Summed weapon energy can hide which weapon has fuel.
  State the tradeoff and test it. Do not equate an arbitrary total with dominance.
- Movement direction, posture, open menu state, or an active tool may distinguish
  endpoints with different possible continuations. Keep only demonstrated
  distinctions. Pool transient menu/weapon states at coarser selection depths
  when otherwise they multiply draws without adding geographical coverage.
- Counting distinct places visited can reward wandering. Use coverage as a
  diagnostic; do not assume a lineage's larger visited set is closer to a win.
- Group depths describe pooling for retention and selection. Verify each depth
  against its intended role; do not assume every key field belongs at every depth.

Do not automatically reduce a partial capability order to a single scalar.
If the current archive interface cannot express the needed distinction, explain
that limitation and propose the smallest generic mechanism as a separate change.

## Lessons from the existing games

| Observed failure | Appropriate response | Strategy leak to avoid |
|---|---|---|
| SMB's camera stops while the player crosses the screen | Track actual player position as well as camera/area identity | A special coordinate reward for the known pipe or castle exit |
| A controller vocabulary omits leftward jumps or running | Verify bit order and cover physically meaningful chords | Retain only combinations that won the pilot |
| MM2 uses Start to reach weapons; Metroid uses Select for a gameplay toggle | Represent the real controls and their press/release semantics | Automatically choose the useful weapon at the obstacle |
| MM2's route doubles back and vertical coordinates wrap | Probe room/coordinate meaning and correct location/death decoding | Rank the known next room or recognize named stage hazards |
| Metroid reaches old places with more resources but cannot carry them along known routes | Measure retention, parent selection, route reuse, and preference starvation | Give missile targets to particular doors or encode the item route |
| Many temporary states swamp the archive | Measure slot count, pooling, retention, and culling | Drop states because a walkthrough says they are off-route |
| A milestone appears halfway through an action | Preserve frame evidence and a matching endpoint/tape | Treat aggregate best counters as one achievable state |

The examples diagnose failure classes. They are not requirements to copy all
SMB/MM2/Metroid fields or the experimental Metroid replay hooks into a new game.

## A small ablation that answers a real question

For an added field, keep the same genesis, seed set, compute limits, vocabulary,
and engine policy. Compare the old and new key on useful discovered states,
slot growth, retention/replacement, cost, and the specific aliasing evidence.
If it only makes the named obstacle easier without correcting an observation
or capability distinction, revisit the design. Preserve the old key identifier
in the evidence so later engine comparisons do not conflate both changes.

Avoid blanket fixed percentage acceptance rules. A search budget exhausted
without completion is a censored result, not an invented completion time.
Report distributions across the registered seeds and explain any tradeoff.
