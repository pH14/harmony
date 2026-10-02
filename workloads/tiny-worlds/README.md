<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Tiny deterministic worlds

Tiny worlds exercise Dissonance's campaign engine with bounded state machines.
Each world supplies transitions, snapshots, archive keys, action sampling, and an
objective. Exhaustive reachability checks and direct action replay provide
independent checks of campaign results.

## Running a world

Build the executable with:

```sh
cargo build --release --locked --manifest-path workloads/tiny-worlds/Cargo.toml
```

The executable reads one JSON request from standard input, capped at 16 KiB.
Required fields are `config`, `seed`, `work_budget`, `broken`, `verify`, and
`keep`; `scale`, `workers`, and `search` are optional.
`config` contains `family` and `parameters`; nested objects reject unknown fields.
Supply a seed at runtime and retain it with the output when reproducing a run.
Keep generated requests and reports outside the repository.

For example, this command generates a request at runtime:

```sh
python3 - <<'PY' | workloads/tiny-worlds/target/release/tiny-worlds
import json, secrets
print(json.dumps({
    "config": {"family": "maze", "parameters": {
        "length": 4, "pattern": 10, "reverse_actions": False
    }},
    "seed": secrets.randbits(64),
    "work_budget": 1000,
    "broken": False,
    "verify": True,
    "keep": "portfolio"
}))
PY
```

`keep` selects slot retention. `portfolio` keeps one holder per slot under two
preferences: charge first and health first. `capacity_two` keeps two holders
per slot under the charge-first preference alone.

An optional `search` object selects existing campaign policies without changing
the world: `suffix` accepts `one_or_two`, `one_to_six`,
`one_to_six_within_3_max_action_cost_full_hold`, or
`one_doubling_while_in_place_up_to_64`; `mixture` accepts the identifiers
documented in the searcher README, including `energy_splice:6`; and
`stop_on_objective` overrides campaign stopping. Omitted fields retain the
world's normal settings. Ordinary runs stop at their first objective by default;
scaled runs normally continue to their work budget. Calibration can set
`stop_on_objective=true` in a scaled run and compare first-objective tries and
work without retaining a large campaign stream.

`work_budget` accepts 1–2,000,000 logical work units. A campaign uses `workers`
workers (1–64, default one) and an admission window of the same size, an
archive capacity of 4,096, and a 16 GiB logical memory budget,
and stops at its first objective. The event stream has a checked 1 GiB
allocation limit. Execution work counts
restored-parent replay and suffix actions; restoring a snapshot preserves the
cumulative work counter. Environmental clocks belong to the restored world state.

The JSON report contains the request parameters, build-time source hashes,
objective result, execution work, archive statistics, and family diagnostics.
With `verify=true`, the runner replays the campaign stream, repeats execution with
the same runtime seed, checks any objective witness against world transitions,
independently sums admitted job work, and checks that the last job started
before the work budget was spent. Tests check these invariants using
runtime-generated seeds; controlled transition and archive tests check the
mechanics directly.

## Scaled runs

An optional `scale` object replaces the fixed campaign settings, to measure
searcher throughput and memory at sizes the other runs cannot reach. Every
field is required:

| Field | Bounds | Meaning |
| --- | --- | --- |
| `workers` | 1–64 | Worker threads. |
| `window` | 1–512 | Reservations in flight. The stream depends on the window and not on the worker count. |
| `results_per_worker` | 1–2 | Finished results a worker may hold before admission: `ResultBuffering::OnePerWorker` or `TwoPerWorker`. |
| `memory_budget_mib` | 1–16,384 | The searcher's logical memory budget. |
| `archive_entries` | 1–4,194,304 | Archive entry limit. |
| `action_cost_ns` | 0–10,000,000 | Thread CPU time each transition spins on its worker thread, so a descheduled worker takes longer, as a real target would. |
| `action_sleep_ns` | 0–10,000,000 | Time each transition sleeps, so more workers than cores still run jobs of a fixed length. |
| `snapshot_bytes` | 0–1,048,576 | Pseudorandom payload stored in every snapshot and charged to the memory budget. |

The payload is derived from the world state, so it adds real resident memory
without changing any search decision, and job-result hashes leave it out. The
spin reads the thread's CPU clock only to wait. A scaled run accepts 1–10,000,000,000
logical work units and requires `verify=false` and `keep=portfolio`. It counts the
campaign stream's bytes instead of storing them, keeps no per-job evidence,
skips final archive entries, and writes the searcher's progress lines, one per
100 executions, to standard error. `HARMONY_COORDINATOR_PROFILE` adds the
coordinator's time profile to standard error. The report on standard output
holds the settings, the objective result, work, elapsed time, stream bytes,
logical resident memory, live entries, selector counters, the fraction of
worker time spent running jobs, and the fraction spent idle while finished
results waited for admission in reservation order.

## Families and controls

`broken=true` selects the control listed below. Controls expose how a particular
representation or action choice affects the same world transitions.

| Family | Mechanics | Control |
| --- | --- | --- |
| `resource` | Refill charge, preserve health, and spend charge along a corridor and at its barrier. | Hide charge from the archive key. |
| `maze` | A sequence of choices determines whether the endpoint opens; a reset permits another attempt. | Omit choice history from the archive key. |
| `actions` | Traverse land and water stages with different effective actions. | Sample only the land action. |
| `deadline` | Choose slow single-action or fast two-action travel before paying a final time cost. | Hide remaining time from the archive key. |
| `delayed` | Complete a repeated sequence or wait while distraction lanes compete for exploration. | Omit partial progress from the archive key. |
| `deadline_actions` | Traverse changing-action stages while every action spends environmental time. | Sample only the land action. |
| `chain` | Complete an ordered sequence of leaf worlds in one campaign. | Omit stage identity, or carried stock when charge carries between stages. |
| `route` | Scout a route, return for an upgrade, and traverse it again. | Uses the same representation in both settings. |
| `backtrack` | Walk a line whose barriers each need one more item; the next item is taken at the hub after reaching the next barrier. | Hide items from the key. |
| `trap` | Follow a corridor to the goal, or enter a side corridor to an item whose rooms never reach the goal. | Hide the item from the progress tier. |
| `map` | Enter a branch of a grid of rooms, take the item at its far end, leave through the same door, and cross the rest of the map to the goal. | Hide the item from the progress tier. |
| `graph` | Walk a long line of nodes, with hashed jumps, to its last node. | Hide the progress tier. |
| `crossing` | Reach a short exit while a large pool of flat-tier places competes for attempts. | Hide partial exit progress. |
| `passive_clock` | Every sampled hold advances a clock; only specified event boundaries change the archive key. | Hide all intermediate events. |

Resource keys retain charge-first and health-first preferences. Deadline keys
retain the partial fast-route phase and prefer remaining time. Exact payment of
an obstacle's time cost succeeds. In `deadline_actions`, ineffective actions
also spend their configured duration. The action world's `observable` flag
controls whether regime identity appears in the key.

## Passive clock

`passive_clock` supplies `events`, a strictly increasing list of 1–64 positive
clock readings, and `holds`, 1–16 weighted duration bands. Each band has
`minimum`, `maximum` (positive byte values), and a positive `weight`. The sampler
chooses a band in proportion to its weight, then uniformly draws a duration
within it. Bands may overlap; their probabilities add. Every hold advances the
clock regardless of any control symbol, and restoring a snapshot restores that
clock. The final event is the objective. Reachability follows from positive
holds and the finite final reading, without enumerating action sequences.

The key is constant between events, with no resource preference for time spent
waiting. Each event creates a new place; `broken=true` hides intermediate events
but preserves the objective. This isolates passive time that is lost when a
later equal-preference state competes with an earlier, cheaper holder. It differs
from `delayed`, where particular actions advance progress and other actions can
reset it. Passive clocks are standalone worlds, because chain actions and work
costs use the four-symbol transition alphabet.

For this family, an action is its held duration and both declared cost units are
`clock_ticks`; other families retain unit-cost `transitions`. A hold is charged
in full even if its endpoint passes the final clock reading. Ordinary and scaled
reports give first-objective executions beside work and the resolved suffix,
mixture and stopping policies. `passive_clock` evidence
counts admitted hold durations, jobs by the parent's actual event phase, the
maximum selected clock in each phase, and the first work/execution for each
event. These bounded aggregates remain available in scaled calibration runs.
They describe observed native campaign work, not an independent probability
forecast. Duration-only inputs abstract otherwise ineffective control symbols.
Before any retention, this preserves the ordinary duration marginal; after
retention it can change duplicate and empirical-tail rates, which need separate
calibration. A new configuration needs unchanged-loss and rate calibration plus
prospective game predictions before it is called predictive.

The comparison panel includes hidden-gap and visible-progress controls around the recorded 1,095-frame automatic wait. The 960–1,230 range tests sensitivity; it is not a native-rate calibration or a forecast. Duration-only inputs omit control-symbol entropy, the populated game archive, and the later menu decision.

The panel gives each clock run 200,000 held ticks, not 200,000 executions.
Long holds consume that budget in relatively few suffixes. It detects changes
that cross the hidden gap within this short horizon; an improvement that still
needs hundreds of thousands of executions can remain censored in both arms.
Equal misses and an equal budget-capped total-work ratio do not establish equal
performance. The gaps exceed the ordinary 720-tick suffix bound, but a miss at
this budget alone is not a rate estimate or evidence of predictive accuracy.
Use longer scaled runs and prospective game forecasts to qualify such gains.

When both arms miss, the panel cannot estimate the `wait entry to goal` leg.
That undecided leg keeps adaptive sampling running to its 256-layout limit,
even when both arms have only censored results. Hidden-gap comparisons can
therefore pay the maximum sampling cost while providing no completion-rate
estimate. A final `plausible` status means no regression was detected at the
sampled horizon; it does not qualify the hidden wait or a game prediction.

## Archive key

The key's progress is the pair (goal, tier). The searcher ranks slots by
progress, so a higher tier draws most of the selection weight. Leaf families
set tier 0 except where an option below raises it.

Delayed progress uses `sequence` or `wait` mode. `placement` puts partial
progress in the key: `identity` as context, `place` as part of the place,
`engaged` in the place with tier 1 once progress is nonzero, and `tier` as the
tier itself. With `sticky_credit=true`, leaving the fight lane keeps the last
reading in the key of every distraction lane; it requires a placement other
than `identity`. With `ammo` nonzero, every fight action spends one ammo, a
miss keeps progress, and a hit needs ammo. Distraction action 3 refills one
ammo up to the maximum. Leaving the fight lane resets progress. Ammo appears
as charge, which preferences favour.

The trap world's item raises the tier to 1 and makes the goal unreachable;
reachability checks every item room. Route `ranked_upgrade=true` puts the
upgraded phase in tier 1.

The backtrack world places barriers every `segment` positions along a line of
`(barriers + 1) * segment` positions. Passing barrier k needs k items. Reaching the
next barrier marks it scouted; action 3 at the hub then takes one item. A wrong
route action returns to the hub and keeps items. Each stage therefore walks
out, returns, and crosses the earlier ground again. `placement` puts items in
the `tier`, in the slot `identity`, or only in a retention `preference` through
charge. The report's `backtrack_first_items` gives the first work at which
each item count was held.

The map world is a `width` by `height` grid of rooms. Doors form a spanning
tree drawn by a randomized depth-first walk from room 0, seeded by `layout`,
so the map has long corridors, branches, and dead ends. The inner region is
the tree branch of about `inner` rooms that hangs from one door; the campaign
starts in room 0 in the outer region, and that door is the only way in or
out. Up to `loops` extra doors join rooms within one region. The item is the
inner room farthest from the door, and the goal is the outer room farthest
from the door; reaching the goal counts only while holding the item, so a
campaign enters the inner region, takes the item, leaves through the same
door, and crosses the outer region again. Actions 0–3 are up, right, down, and
left. Crossing a horizontal door takes `corridor` presses in its direction and
crossing a vertical door takes `shaft` presses; the opposite action steps back
and the side actions do nothing. The archive place is the room and the
position inside its doorway, and the tier is 1 once the item is held. Map
reports include the `layout` (doors per room, regions, item, key, door, goal,
region entry, farms, ordered items, and room distances; item and key are null
when absent), `evidence.map_first` (first work entering the inner region,
holding the item, leaving it with the item, and at the goal),
`evidence.map_first_tier` (first work at each tier), `evidence.map_first_stocked`
(first work arriving in a boss room holding the item and enough stock, and
enough health when the boss hits back), and `parent_timeline`, which lists
each job's start work, its parent's tier, and its parent place in order. A room
is its place divided by 16; boss hit counts are places 1,024 plus the hits.
As a known limit, the map spreads draws faster than Metroid. After the item
it leaves the item room at once and settles with 0.8–0.9 of top-tier draws
outside the inner region, where Metroid stays on 6–12 map cells for about 0.4
of the first trip and settles at 0.3–0.5.

Optional map fields add Metroid behaviours; each defaults to off. The search
key puts `stock` in the stock field and `health` in the health field, so both
feed the slot preferences and neither changes the tier.

| Field | World | Metroid behaviour |
| --- | --- | --- |
| `farms`, `farm_cap` | `farms` farm rooms; a layout whose region has too few rooms is rejected. Entering an even-numbered farm adds one health and an odd-numbered farm one stock, up to `farm_cap`. Without a boss the farms are inner rooms other than the item room. | Refilling energy and missiles in rooms that do not lead to the next item: a stock gain wins the slot preference and draws the search back to the farm. |
| `items` above 1 | The items are outer rooms, taken in order, each the room farthest from the start and the earlier items. The door to the inner region opens once all are held, and the goal is the inner room farthest from the door; the tier is the item count. | Each item gain sends a new tier back across the whole map to the one region it opens. |
| `boss_stock` | A boss in the goal room, which is the outer room farthest from the door among rooms with at most three doors, so a wall remains to press. Needs `farms` of at least 2 and `boss_stock` at most `farm_cap`. Stock farms fill only while holding the item and lie in the outer half farthest from the goal. Each wall press in the goal room with the item spends one stock and adds a hit; any step into a doorway resets hits. The goal needs `boss_stock` hits. Hit counts are their own places. | Kraid and Ridley: the item tier must farm missiles far away, then carry them to the boss room. |
| `boss_hits_back` | Needs `boss_stock` 1–8. Each hit also spends one health and needs health left, each stock farm entry spends one health, and health stops at `boss_stock` plus 2, or with `approach_drain` at `boss_stock` plus 2 plus a third of the rooms from the farthest farm to the goal. The goal needs `boss_stock` stock and `boss_stock` health on one arrival. | Arriving at a boss with enough missiles or enough energy but not both: the preference that keeps the most stock keeps a holder low on health. |
| `boss_by_door` | Needs `boss_stock`. The goal is the outer room nearest the inner region's door that has a wall to fire at, so with a small `inner` the item sits a few rooms from the boss and the farms are far. | Ridley's statue next to Tourian: the tier rises beside the end, and the stock must come from across the map. |
| `tail_slots` | Needs `gauntlet`, `approach_drain`, `late_item` or `shield`. The archive keeps separate holders at each place by the last three actions. | Metroid's positions within a room: many holders share a room, so the best holders near the end get few draws. |
| `late_item` | Needs `boss_hits_back`, without `approach_drain` or `boss_by_door`. The goal is the inner room farthest from the entry that has a wall to fire at, and the item is the inner room before it, so the tier rises beside the boss. Stock farms fill without the item. Every arrival in an inner room other than the goal costs one health and one stock, and an arrival at 0 health kills the run. Arrivals in outer rooms other than the farms lose one health on a third of the arrivals and one stock on another third, each a fixed function of the room and the last three actions. Health stops at `boss_stock` plus 2 plus the rooms from the door to the item, so a holder that reaches the item low cannot walk back out to the farms. | Ridley's statue beside Tourian: one drained state raises the statue and founds the top tier, every holder in that tier descends from it, and the stocked lower-tier holders get few draws once it opens. |
| `item_at_entry` | Needs `late_item`. The item is the inner region's entry room, and the goal is the inner room with a wall closest to four rooms past the entry, so every inner room's fixed cost falls after the tier rises. Arrivals in outer rooms lose stock on a third of the arrivals and never lose health. Health stops at `boss_stock` plus 2 plus the rooms from the door to the goal. | Tourian: drained states raise the statue, and the rooms past it drain missiles and energy with nothing to restock them, so a kill needs stock carried in from before the statue. |
| `tanks` | 0–8; needs `boss_hits_back`. Tank rooms are the outer rooms, other than farms, the door room, the goal, the key and items, that add the most to a walk from the start to the door. The first arrival in each tank room raises the health limit by 4 and refills health to the new limit. | Metroid's energy tanks off the route: a holder that collects them ranks first on health while holding little stock, and collecting them costs route time. |
| `shield` and `shield_odds` | `shield` 1–32 needs `boss_stock` and `shield_odds` 1 or more. While the shield is below `shield`, a wall press in the goal room with the item lands one time in `shield_odds`, spending one stock and adding one to the shield; hits start once the shield is full. A 16-bit aim value that each action changes decides the landing, and the key does not see it, so a replay from the same state lands differently. Each shield level at the goal is its own place. | Mother Brain behind the Zebetites: many missiles go to a barrier before any damage, and the chance of a landed shot keeps the search firing from the same states. |
| `hit_tier` | Needs `boss_stock`, one item and no lock. The first hit on the boss raises the tier by one. | Metroid's boss damage in the progress order: the first hit founds a new top tier from whatever stock the hitting state held, and every later hit descends from it. |
| `approach_drain` | Needs `boss_hits_back`. While holding the item, every arrival outside the farms and the goal room loses one health on half the arrivals and one stock on another half, each a fixed function of the room and the last three actions; a health loss at 0 health kills the run. | Tourian after a long walk from the refills: the most-missiles holder near the end has little energy and the most-energy holder has no missiles, and no holder keeps enough of both. |
| `item_optional` | The inner region is the shallowest side branch whose size is within two rooms of `inner`, or the branch closest in size when none is, and the item is its room farthest from the entry. The goal is the outer room farthest from the start that is not on the way to that branch. The goal counts without the item, which still raises the tier. | Varia and other pickups off the main path: the new tier walks back over ground the lower tier already reached. |
| `locked` | The inner region is chosen as for `item_optional`. Its door opens only while holding a key in the outer room farthest from the start other than the door room; the goal is the outer room farthest from the door other than the key room and needs the item. The key raises the tier to 1 and the item to 2. | Varia in Brinstar after the lower tier reached the Tourian shaft: the lower tier's frontier is far from the pickup, and the new tier must walk back across the map. |
| `timing` | 0, or a period of 2–16. Every action is rotated by a hidden phase that advances by one each step modulo `timing`, so a recorded suffix replayed from a holder with another phase takes different actions. At 5, about one replayed suffix in five lands, and at 10 about one in ten. A period that is a multiple of 4 lands one in four, because rotations by phases four apart are equal. | Enemy positions and frame timing that the key does not see. |
| `gauntlet` | Needs `items` of 2 or more and farms. The last item is the outer room nearest the door, and the farms are the outer rooms farthest from it. Every farm gives health, but only on one arrival in eight, and health stops at the entry-to-goal room count plus 2. One outer arrival in four loses one health. An inner arrival loses one health unless the room and the last three actions fall in one combination of 64, and a hit at one health or less kills the run, which then ignores every action. Each of these draws is a fixed function of the room and the last three actions. Searches build health after the last item is first picked up, by farming while holding both items. When the last item is first picked up, no holder with fewer items has health to carry there. | Tourian: the last item arrives at the entry low on energy after a long walk from the refills, and only an arrival near full energy crosses a region with no refills. |

The graph world is sized for scaled runs. It has `nodes` states in a line,
each also carrying stock and health from 0 to 15. Action 0 moves to the next
node, so the goal at the last node is reachable from every state; the
reachability check holds by this construction instead of enumeration. Actions
1–3 jump to a node drawn from a hash of `layout`, the node, and the action, and
add amounts from the same hash to stock and health modulo 16. The place splits
the nodes into `places` equal ranges and the identity is the node's position in
its range, so a large graph gives hundreds of thousands of slots in about as
many cells as `places`. Stock and health feed the two slot preferences, and the
tier splits the nodes into `levels` equal ranges. Graphs cannot be chain
stages.

Diagnostics count admitted suffix observations. Arrival histograms count
transitions into a location; resource refill counts require a stock increase.
Delayed-progress histograms include distraction observations in their zero bin.
Exported archive entries include ancestry; `live_entries` reports active holders.
Continuation job counts and work come from the campaign stream.
Ordinary reports also count `executions`, one restore plus one executed suffix,
and give `first_objective_execution` beside `first_objective_work`. Both are
independently read from the stream and checked against the campaign counters;
skipped selector draws are not executions. `parent_timeline` covers every
ordinary world and aligns with `evidence.job_parents` in admission order. Its
job start work locates an action-work discovery in a search execution: use the
last job whose start work is strictly less than the discovery's work. A
milestone present at genesis has execution zero. This keeps a discovery on a
job's last action in that job instead of the next one. These fields let
forecasts compare search attempts without treating action work as attempts.
`parent_draws` counts jobs by (before objective, selector path, tier rank,
parent tier, parent place). `skipped_draws` counts draws whose parent and
suffix were already executed; they produce no job.

## Configuration bounds

Every parameter is required except the optional map fields. Bounds keep
exhaustive enumeration below 200,000 states; requests exceeding the reachability limit are rejected. The graph
family's and passive clock's reachability hold by construction.

| Family | Parameters |
| --- | --- |
| Resource | `initial_charge` 0–31; `initial_health` 1–31; `barrier_charge` 1–15; `route_cost`, `health_cost`, and `refill_health_cost` 0–15; `refill_amount` and `max_charge` 1–31; `corridor_len` 1–12; `initial_charge` ≤ `max_charge`. |
| Maze | `length` 1–8; `pattern` < `1 << length`; boolean `reverse_actions`. |
| Actions | `segment_len` 1–8; `water_action` 1–3; boolean `return_to_land` and `observable`. |
| Deadline | `length` 1–8; `initial_time` 1–64; `fast_ticks` 1–4; `slow_ticks` > twice `fast_ticks` and ≤ 16; `obstacle_ticks` 1–16. |
| Delayed | `horizon` 1–16; `distractions` 1–32; `mode` is `sequence` or `wait`; `placement` is `identity`, `place`, `engaged`, or `tier`; boolean `sticky_credit`; `ammo` 0 or `horizon`–31. |
| Deadline/actions | `actions` uses the actions parameters; `initial_time` 1–64; four `action_ticks` values, each 1–16. |
| Route | `length` 2–16; `pattern` encodes two-bit actions per position; `attack` 0–3; boolean `shifted`, `upgrade_required`, and `ranked_upgrade`. |
| Backtrack | `barriers` 1–4; `segment` 1–8; `(barriers + 1) * segment` ≤ 32; `pattern` encodes two-bit actions per position and its first action differs from 3; `placement` is `tier`, `identity`, or `preference`. |
| Trap | `length` 1–16; `pattern` encodes two-bit actions per position and its first action differs from 3; `trap_len` 1–8; `rooms` 1–16. |
| Map | `width` and `height` 2–8; any `layout`; `loops` 0–16; `corridor` and `shaft` 1–4; `inner` from 2 to two fewer than the room count; `items` 1–9, and above 1 only with two outer rooms to spare and without farms unless `gauntlet`; `farms` 0–8 with `farm_cap` 1–63, and `farm_cap` only with farms; `boss_stock` 0–`farm_cap`, 1–8 with `boss_hits_back`, and not in chain stages; `approach_drain` needs `boss_hits_back`; `boss_by_door` needs `boss_stock`; `late_item` needs `boss_hits_back` and neither `approach_drain` nor `boss_by_door`; `item_at_entry` needs `late_item`; `tanks` 0–8 and only with `boss_hits_back`; `shield` 0–32, above 0 only with `boss_stock` and `shield_odds` above 0, and `shield_odds` only with a shield; `boss_stock` plus `shield` at most `farm_cap`; `hit_tier` needs `boss_stock`, one item and no lock; `tail_slots` needs `gauntlet`, `approach_drain`, `late_item` or `shield`; `timing` 0 or 2–16; `item_optional` needs one item and no boss; `locked` needs one item, no optional item, no boss, and an outer room for the key besides the start and the door room; `gauntlet` needs `items` 2 or more and farms. |
| Graph | `nodes` 16–4,194,304; `places` 1–`nodes` with at most 65,536 nodes per place; `levels` 1–16; any `layout`. |
| Passive clock | `events` has 1–64 positive, strictly increasing `u16` readings; `holds` has 1–16 bands, each with `minimum` and `maximum` 1–255, `minimum` ≤ `maximum`, and `weight` 1–65,535. |

## Scenario chains

A chain has one to sixteen `stages`, each containing a leaf `world` and
`refill_available`. Every family except `chain`, `graph`, `crossing`, and `passive_clock` is a supported leaf. Completion enters the next stage in the same
action. The final stage's goal is the campaign objective. Snapshots contain the
active stage, local state, and carried charge. Health, history, and clocks reset
on stage entry; archived snapshots allow exploration from earlier stages.

With `carry_charge=false`, each stage starts from its declared initial state and
chain `initial_charge` is zero. Stage identity namespaces archive places in
blocks of 1,024. With `ranked=true`, a stage's tier is its leaf tier plus 32
times the stage index, so a later stage outranks every earlier one; the control
that omits stage identity keeps only the leaf tier. With
`ranked=false`, every chain key has tier 0. With
`carry_charge=true`, chain `initial_charge` initializes persistent charge;
resource stages consume and replenish it, while other stages preserve it.
Resource stages share a capacity and declare local `initial_charge=0`.
`refill_available=false` disables the resource refill action. Other leaves
require this flag to be true.

Chains use a uniform four-action alphabet. Across stage namespaces,
preferences compare persistent stock; within a stage they also compare local
resources. Whole-chain reachability is checked independently of each stage's
reachability.

Chain diagnostics include stage-entry work and charge distributions, suffix
actions per stage, and executed work grouped by parent stage and charge.
Pre-objective counters include the objective job and are checked against work to
the first objective, or total work for an unfinished campaign.

## Held-button world

`held-world` compares controller chord draws on a side-scrolling course of
ground and pits. It reads one JSON request with `course` (`ground_px` 16–1,024
and one to 32 `pits_px`, each 1–120 pixels), `seed`, `work_budget` in frames
(up to 400 million, or 20 million with `verify`), `workers`, `suffix`,
`mixture`, and the optional `chords` and `verify`.

B raises the top speed from walking to running. Holding A lengthens a jump, and
a new jump needs a new press. A running jump clears pits that a walking jump
falls into, and a fall returns the runner to the start of its section. The
progress tier is the 128-pixel band. The place is the 16-pixel column, the
height and the 1,024-frame time bucket, and fewer frames spent is the
preference.

`chords` is `change_one_control`, the SMB draw and the default, or `fresh`,
which draws whole new chords as the control. Holds are 2–12 or 96–120 frames,
as in SMB. On 24 pits (16–112 pixels three times, 128-pixel ground), four
workers and eight seeds, change-one-control reaches the goal at a median of
0.34 million frames with 1,087 live entries. Fresh chords take 0.91 million
frames with 1,862 live entries.

## Upgrade route

The route world requires scouting a blocked endpoint before `attack` returns to
the origin and acquires an upgrade. Wrong route actions return to the origin
while preserving the upgrade. With `shifted=true`, acquisition changes lanes;
`(attack + 1) % 4` aligns the player before route movement succeeds. Alignment
must differ from the first route action. With `upgrade_required=false`, the
first traversal completes the objective.

Position and lane identify archive places; phase is a retention preference.
Route reports join executed observations to admitted campaign jobs. They record
first upgraded position/lane arrivals, acquisition, alignment, and continuation
transfers that advance an upgraded state. Donor and leaf states come from
historical admissions. A `non_upgraded_donor` label describes their recorded
phase. Job-end work includes replay overhead. The action mixture draws random
one- or two-action suffixes and supports continuation dispatch.

## Searcher panel

The panel runs the worlds under settings whose outcomes match recorded Metroid
search behaviour, then checks each outcome's direction:

```sh
uv run workloads/tiny-worlds/panel.py
```

It builds the executable, draws every seed at runtime, runs six seeds per arm
(twelve for the engaged and level-as-tier tight-ammunition boss arms) in six
processes, and prints one PASS or FAIL line per rule. It writes no files. A run
takes under a minute on a ten-core laptop with `--jobs 10`. `--seeds` and
`--jobs` change the sample and parallelism; `--binary` uses a prebuilt
executable instead of the fresh build.

| Rule | Setting |
| --- | --- |
| Boss damage | Delayed boss with tight or ample ammunition; partial progress in the place, as an engaged tier, or as the tier itself. |
| Credit kept after leaving | Engaged boss with and without `sticky_credit`. |
| Unwinnable top rank | Trap world with the item ranked against the same world with the item hidden, on the same seed. The item raises the tier and makes the goal unreachable, so the search must leave the top tier to reach the goal. The ranked run takes under 4x the hidden run's median work, and the hidden run's median stays under 300 work. Over 300 seeds the current searcher measures 1.8x and 172 work; six-seed medians exceed 4x in 0.03% of 20,000 resamples and exceed 300 work in 0.01%. Drawing the top tier at its full rank weight whatever it yields measures 7.9x, and fails the first rule in 99.4% of six-seed resamples. |
| Retention | Two-stage chain under `portfolio` and `capacity_two`; the rule expects equal work. |
| Backtracking | Backtrack world with items ranked, split by identity, kept as a preference, or hidden. |
| Chains | Sixteen-stage chains: flat, ranked stages, ranked stages with ranked upgrades, and kept boss credit. |
| Growing gaps | Eight-by-eight map with the item ranked or hidden, one layout per seed; the return trip is shorter than the first trip, and per room of path the walk from the door to the goal takes longer than the return trip. |
| Farm loop | Eight-by-eight map with `inner` 20 and 4 farms against the same layout and seed without farms, 200,000 work each. The median ratio of the trip from leaving the item region to the goal stays under 10x, which six-seed medians on the current searcher stayed below in 20,000 resamples of 300 pairs. The current searcher measures 5.87x. Resetting a cell's draw count only on its 1st, 2nd, 4th, 8th and later carried-in preference win measures 2.50x, with six-seed medians up to 4.47x in 20,000 resamples. That change is clearly bad on the gauntlet worlds: net extra goal misses are 9.0% of layouts, and 12.9% with hidden timing. |
| Boss damage as a tier | Twelve layouts of the boss behind a shield, each with `hit_tier` on and off on the same seed, 600,000 work each. Damage as a tier loses more kills than it gains. On the current searcher it loses 41% of 32 layouts and gains none, the loss seen in Metroid, where every engaged Mother Brain state descended from one drained first hit. |

Timing rules compare medians in which an unsolved run counts as the work
budget on the side that must be slower and as unbounded on the side that must
be faster, so a rule passes only when solved runs establish it. A milestone
reached after the budget counts as unreached. Count
allowances scale with the seed count and round up. Each rule records current
searcher behaviour. A searcher change that flips a rule predicts the same
change on Metroid. The backtrack ranked-items rule and the farm-loop rule set
their thresholds above the current searcher's spread, so they fail only when a
change makes that cost worse. Six-seed medians on the current searcher put
ranked items at 1.41x the preference work, at most 2.29x in 20,000 resamples
of 300 seeds, against a 2.5x threshold. With six seeds per arm, a rule close to its threshold can
flip between runs of an unchanged searcher, so rerun a FAIL before attributing
it to a change. The panel is not a CI check.

### Comparing a searcher change

`--compare BASELINE` also runs the game-mechanism worlds on the baseline executable and
on `--binary` or the fresh build, with the same layouts and runtime seeds on both:

```sh
uv run workloads/tiny-worlds/panel.py --jobs 10 --binary candidate --compare baseline
```

`--workers N` uses `N` workers and an admission window of `N` when `N` is
greater than one, to check the window sizes a multi-worker search uses. The
default rare crossing requests use one worker and their explicit window of two.
`--world NAME`, repeatable, limits the comparison to the named worlds on fresh
layouts.

| World | Settings | Game behaviour | Measures |
| --- | --- | --- | --- |
| Flat archive crossing | 1,024 places, four exit steps, randomized layout and action pattern | Mega Man 2: an easy local crossing competes with a populated flat-tier archive | To crossing entry |
| Fresh crossing | Same transitions, rooted at the exit entrance | Control for crossing difficulty without the competing archive | To crossing entry (zero) |
| Rare flat archive crossing | 256 places, eight exit steps, useful-action denominator 1,024 | Mega Man 2: difficult local retries compete with a populated flat-tier archive | To crossing entry, in tries |
| Rare fresh crossing | Same transitions, rooted at the exit entrance | Control for retry difficulty without the competing archive | To crossing entry, in tries (zero) |
| Passive clock hidden gap | 960–1,230 hidden clock ticks after two visible events; weighted holds, existing full-six suffix | Automatic equipment messages can advance without a retained cell change | To wait entry |
| Passive clock visible progress | Same hold distribution and gap range, with an event every 40 ticks | Cause-removal control exposing the waiting progress | As above |
| Farm loop | `inner` 20, 4 farms | Refills away from the next item draw the search back | To the item, out of the item region |
| Whole-map re-walk | `inner` 4, 9 items | Each item sends a new tier back across the map | To the last item |
| Boss needing far stock | `inner` 20, 4 farms, `boss_stock` 24 | Kraid and Ridley need missiles farmed far from the boss | To the item, to the stocked arrival |
| Boss needing stock and health | `inner` 20, 4 farms, `boss_stock` 6, `boss_hits_back` | Arriving with enough missiles or enough energy but not both | As above |
| Boss after a draining approach | `inner` 20, 4 farms with `farm_cap` 14, `boss_stock` 8, `boss_hits_back`, `approach_drain` | Tourian's delivered state low on both, with the best holders near the end split between the two | As above |
| Off-path item | `inner` 6, `item_optional` | A new tier from an off-path pickup walks back over reached ground | To the pickup |
| Off-path item with farms | as above, 2 farms | The same with refills | To the pickup |
| Locked item | `inner` 4, `locked` | The new tier's lower-tier frontier is far from the pickup | To the key, to the item |
| Locked item with hidden timing | as above, `timing` 5 | Replayed inputs land about one time in five | As above |
| Gauntlet | `inner` 20, 2 items, 2 farms, `farm_cap` 1, `gauntlet` | Tourian: the last item comes at the entry low on energy, and the refills are far away | To the last item, to the full-health arrival |
| Gauntlet with hidden timing | as above, `timing` 5 | The same with replayed inputs landing about one time in five | As above |
| Boss by the door | `inner` 2, 4 farms with `farm_cap` 14, `boss_stock` 8, `boss_hits_back`, `approach_drain`, `boss_by_door`, `tail_slots`; 600,000 work | Tourian after the statues: the best holders near the end are drawn one or two times each, and none holds enough of both | To the item, to the stocked arrival |
| Boss beside a late item | `inner` 20, 4 farms with `farm_cap` 63, `boss_stock` 8, `boss_hits_back`, `late_item`; 600,000 work | The 9-item tier founded by one drained state at Ridley's statue, while stocked 8-item holders lose their draws | As above |
| Boss past an item at the entry | `inner` 20, 4 farms with `farm_cap` 63, `boss_stock` 8, `boss_hits_back`, `late_item`, `item_at_entry`, `tail_slots`; 600,000 work | Tourian: drained states raise the statue, and a kill needs stock carried past rooms that drain it | As above |
| Boss behind a shield with damage as a tier | `inner` 20, 4 farms with `farm_cap` 63, `boss_stock` 24, `shield` 8, `shield_odds` 4, `hit_tier`, `tail_slots`; 600,000 work | Mother Brain: the first hit raises the tier from a state that spent its missiles on the Zebetites, and the stocked states below that tier stop getting draws | To the item, to the stocked arrival, to the first hit |

The passive-clock worlds use `one_to_six` and `energy_splice:6`. The rare crossing worlds use `one_to_six_within_3_max_action_cost_full_hold`, `energy_splice:6`, and scaled low-memory reports; the other worlds retain their default settings. Clocks count `clock_ticks`, whereas crossing work counts actions. The rare crossing entry legs count executions. Compare arms within a world, not absolute work across families. Older binaries without these families or options cannot run them: use a baseline built with the same world implementation and the unchanged engine, and record that source explicitly.

Each measure is the work from the start of the campaign to a milestone. Every
world also measures work to the goal, counting a missed goal as its budget, and
goal misses. The panel also prints the legs between consecutive milestones as
diagnostics. A leg between two milestones charges a candidate that reaches the
first milestone sooner, so the verdict uses only the measures from the start. A
slower diagnostic is a watch.
A campaign stops at the goal or at its budget: 2,000,000 actions for the rare
crossings, 600,000 work on the worlds the table marks with 600,000 work, and
200,000 work for the others. A milestone after the budget counts as unreached.
Comparison runs skip replay verification; the rule panel
verifies every run. The off-path and locked worlds start with four times
`--world-scale` layouts (default 16) and the others start with `--world-scale`.
A stocked arrival is the first arrival in the boss room holding the item and at
least `boss_stock` stock, and at least `boss_stock` health when the boss hits
back. With a shield it needs stock plus shield of at least `boss_stock` plus
`shield` and no hits. A full-health arrival is the first arrival in the gauntlet's entry room
holding both items and health at least the entry-to-goal room count. For each measure and diagnostic the panel prints the median over layouts of the
candidate-to-baseline ratio of work plus one, a 99% bootstrap interval, and how
many layouts reached the milestone in each run; only layouts where both runs
reached it enter the ratio, and one with fewer than three such layouts
prints nan. The off-path pickup measure covers only layouts
where the pickup came before the goal, and a change in that count changes which
layouts it compares. A measure is slower when the interval lies above 1.25,
faster when it lies below 0.8, inside the band when it lies within
[0.8, 1.25], and undecided when it crosses 0.8 or 1.25. Goal misses are
judged on the layouts where only one of the two runs missed the goal. The
candidate has more misses when its net extra misses reach 5% of layouts and an
exact one-sided sign test on those layouts gives p < 0.01, and fewer misses in
the mirror case. Misses are inside the band when the 99% bootstrap interval of
the net extra-miss rate lies within ±5%, and undecided otherwise. A world with
an undecided measure or undecided misses doubles its layouts until nothing is
undecided or it reaches 256 layouts (1,024 for the off-path and locked worlds).
A world is clearly bad when any measure is slower or the candidate has more
misses. A measure still undecided at the limit with its upper bound above 1.25 makes
the world undecided when its interval's lower bound is above 1.0, and is a
watch otherwise. Misses still undecided at the limit make the world
undecided when the net extra misses reach 5% of layouts and an exact one-sided
sign test gives p < 0.05, and are a watch otherwise. A watch passes and is named in the world's line so the
matching Metroid leg gets measured in the game slices. A candidate is
plausible when no world is clearly bad or undecided. The exit status is nonzero when a rule fails or a world is clearly
bad or undecided. Two unchanged
searchers with different runtime seeds, in 300 simulated comparisons per world,
failed the farm loop, re-walk, off-path, locked and first two boss worlds in
none, the gauntlet worlds in 0.3% (no timing) and 2.3% (hidden timing), the
boss after a draining approach in 1.7%, the boss by the door in 3.0%, the
boss beside a late item in 2.3%, the boss past an item at the entry in 0.3%, and the boss behind a shield with damage as a tier in 5.0%. The shield world's fails all come from the goal-miss rule, since two seeds on the same layout disagree on the kill in 24% of pairs; its measures fail in none. The simulated comparisons
for a world draw from one pool of 512 layouts (2,048 for the off-path and
locked worlds; 343 layouts with three seeds each for the five newest boss
worlds), so they share layouts and these rates are rough. At these rates an
unchanged build fails at least one of the 15 worlds in about 14% of comparisons.
When a candidate fails exactly one world and the failing measure lies inside the
1st to 99th percentile of the same measure in that world's unchanged
comparisons, rerun that world alone with `--world` before dropping the
candidate, and judge it by the rerun. A fail outside that range, or fails on two
or more worlds, stand. Each world also counts the layouts whose event streams are identical on
both executables, which happens where the change never acts. A comparison that
takes every world to its limit takes about 25 minutes with `--jobs 10`.

The map world is the calibrated world for the return trip. In recorded
Metroid campaigns, climbing out of Kraid's hideout after the kill takes
0.55–0.72 of the first trip from the hideout entry to Kraid's room. The map
rule asserts only that the return trip is shorter than the first trip.

## Scale measurements

`scale.py` runs scaled graph campaigns and prints measurements. It is not a CI
check, writes no files, and each mode runs for minutes to hours:

```sh
uv run workloads/tiny-worlds/scale.py slowdown --binary before --binary after
uv run workloads/tiny-worlds/scale.py cores --cost-ns 4000000 --work 6000 --repeats 3
uv run workloads/tiny-worlds/scale.py memory --workers 2 --work 1000000
```

Every run uses a 4,194,304-entry archive limit, a window of two reservations per worker
and one result per worker unless `cores` sets them.
`slowdown` and `cores` use one runtime seed and one graph layout for all their
runs, so every build and worker count explores the same graph. A try on the
graph averages about 1.3 transitions, so `--cost-ns 4000000` gives about 5.2 ms
of worker CPU time per try.
The script samples the coordinator thread's CPU time, from `ps -M` on macOS
and from `/proc` schedstat on Linux, against the executions on the latest
progress line.

| Mode | Runs | Prints |
| --- | --- | --- |
| `slowdown` | One long campaign per `--binary` on a 4,194,304-node graph with 1,024 places, a 16 GiB budget, and no snapshot payload. | Executions per second and coordinator CPU milliseconds per 1,000 executions, as medians over sampling windows grouped into `--bin` active entries. The final window is dropped because it includes the final report. |
| `cores` | The same graph at each of `--workers-list`, with `--work` transitions per worker, or in total with `--total-work`, and with `--reservations` and `--results` per worker. `--sleep-ns` makes each transition sleep instead of spin, so worker counts above the core count still measure scheduling. | Median executions per second over `--repeats`, timed between the first and last progress lines of the search so setup and the final report are excluded, speedup over the first count, coordinator busy share, transitions per try, worker milliseconds per try, the share of worker time running jobs, and the share idle while results waited for admission. |
| `memory` | `--seeds` campaigns on a 65,536-node graph under `--budget-mib`, plus one control at 16 GiB, with `--snapshot-bytes` payloads. | Whether each run reached the goal, peak logical memory, progress lines over the budget, peak RSS, snapshot evictions, history compactions, and dropped entries. |

## Checks

```sh
cargo test --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml -- --test-threads=1
cargo fmt --manifest-path workloads/tiny-worlds/Cargo.toml -- --check
cargo clippy --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml --all-targets -- -D warnings
```

Tests cover exhaustive reachability, dead trap items, snapshot restoration, archive retention in
both insertion orders, controlled objective witnesses, chain boundaries, route
alignment, trace validation, replay, and execution-work accounting.

## Archive competition diagnostic

The `crossing` family separates a pool of flat-tier places from a short exit
path. Required parameters are `cells` (2–60,000), `length` (1–8), `pattern`
(two bits per exit step), `layout` (pool jump seed), and `rooted`.
Pool action 0 advances one place; actions 1 and 2 jump deterministically to
another pool place; action 3 waits. Only advancing from the last pool place
enters the exit. Correct exit actions advance; incorrect actions reset to the
exit entrance.
The exit has no direct transition back to the remote pool. All resources
and tiers stay constant. The control hides partial exit progress from the key.

Optional `action_denominator` (4–65,535) controls the frequency of useful
inputs. Each draw picks a uniform slot in that denominator: slots 0–3 keep
their transitions, and every other slot emits the same neutral action 4. That
action leaves the state unchanged while spending one action of work. Omitting
the option retains the uniform-four policy and its RNG draw sequence. An
explicit denominator is recorded in the policy identifier and must match on
replay. This separates local retry frequency from competing pool places. It
does not model health, death, enemy movement or held-action duration.

Diagnostics include both first exit-entry work and first exit-entry execution.
Subtract the latter from first-objective execution for tries after entry;
report total approach tries separately. Rooted entry counters are zero. Scaled
reports expose these fields under `crossing`, so longer calibrations need not retain the full stream. Never
use pool-start total tries as a checkpoint-slice leg.

A recorded Mega Man 2 boss-arena handoff motivated separate controls for
local retry difficulty and competition with a populated flat-tier archive.
The existing uniform-four crossing makes local retries much easier than the
recorded game leg. Increasing the denominator permits calibration to its
retry rate before an allocation forecast. A pool gap alone does not qualify
that calibration or show that action sampling caused the native gap.
Hiding four consecutive steps with a unit-cost, three-action bounded suffix
prevents reaching the exit; censored equality in that control is not an
allocation null result.

The comparison panel includes `rare flat archive crossing` and `rare fresh
crossing`: 256 pool places, eight visible exit steps, denominator 1,024, the
bounded one-to-six suffix and `energy_splice:6`. They use a 2,000,000-action
budget, stop at the goal, and run through the scaled low-memory reporting path.
The fresh world isolates local retries. The flat world measures the same exit
with a populated flat-tier pool. Its entry-to-goal leg counts executions;
approach tries and total action work are separate legs. A missing goal or one
recorded after the work horizon leaves the retry leg unobserved and contributes to the panel's goal-miss comparison;
do not interpret equal censoring as equal retry performance.

The parameter choice followed observation of the calibration runs. It is
not a prospective prediction, and the world omits health and several native
archive differences. Unchanged-engine controls used 512 shared layouts per
world with independent seeds: both full cohorts were plausible, with the
flat world's entry-to-goal retry leg a watch leg. Among 300 adaptive comparisons
resampled from each finite layout pool, the actual panel returned a false
failure in 5/300 fresh comparisons (1.67%: one clearly bad and four undecided)
and 40/300 flat comparisons (13.33%: all undecided). Watch legs remain
plausible; undecided worlds fail the panel. These are approximate pooled
rates, not 300 independently generated cohorts. Final verdicts were computed
from cached reports at the original adaptive stopping lengths. The full
512-layout cohorts exceed the panel's normal 256-layout limit, which was
reached in 279 fresh comparisons and all 300 flat comparisons. In particular,
the flat world's uncertainty can make an unchanged engine fail a short panel;
do not relax the acceptance rule or treat an undecided comparison as efficacy.
At that limit, the two added worlds can cost 1,024 binary runs of up to
2,000,000 actions each; short or identical comparisons establish no searcher
speedup.

`rooted=true` starts at the exit entrance with the exact same transitions and
resources. Compare its objective work with entry-to-objective work from a
pool start, reporting approach censoring separately. Diagnostics record first
exit-entry work and actions in the pool and exit. Each pool position is its own
place, unlike maze histories that compete as identities within a few places.
Graphs and crossings cannot be chain stages. The normal 4,096-entry campaign
limit still applies; choosing more places does not guarantee their retention.

This diagnostic is motivated by Mega Man 2's Metal room 18→19 contrast between
a fresh archive and a populated archive. Calibration reproduces an easy fresh
crossing and a much slower identical crossing under flat-tier competition.
This is a simplified local retry model: the exit cannot return to the remote
pool, whereas a game can eventually revisit earlier areas. Tiny work counts
actions, so a forecast of game execution counts still needs a scorecard.

The recorded crossing rejection rates below used the former 32 MiB default. Requalify them under the current budget before using them as current rates.

For the crossing comparison worlds, 300 simulated comparisons of unchanged
searchers with independent runtime seeds rejected 3.7% of populated-pool
comparisons (all undecided) and 3.3% of fresh-crossing comparisons (3.0%
undecided and 0.3% clearly bad). These use the default 16 initial layouts,
the existing adaptive doubling and 256-layout limit, and 2,000 bootstrap draws.
Each world draws from one pool of 512 shared layouts, so these are approximate
rates, not 300 independent experiments. All underlying runs reached the goal;
rejections came from work intervals. This sampling noise limits forecasts
from small cohorts and does not override the panel's rejection rule.
