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
Required fields are `config`, `seed`, `work_budget`, `broken`, and `verify`.
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
    "verify": True
}))
PY
```

`work_budget` accepts 1–20,000 transitions. A campaign uses one worker, one
reservation, an archive capacity of 4,096, and a 32 MiB logical memory budget.
The event stream has a checked 32 MB allocation limit. Execution work counts
restored-parent replay and suffix actions; restoring a snapshot preserves the
cumulative work counter. Environmental clocks belong to the restored world state.

The JSON report contains the request parameters, build-time source hashes,
objective result, execution work, archive statistics, and family diagnostics.
With `verify=true`, the runner replays the campaign stream, repeats execution with
the same runtime seed, checks any objective witness against world transitions,
and independently sums admitted job work. Tests check these invariants using
runtime-generated seeds; controlled transition and archive tests check the
mechanics directly.

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

Resource keys retain charge-first and health-first preferences. Deadline keys
retain the partial fast-route phase and prefer remaining time. Exact payment of
an obstacle's time cost succeeds. In `deadline_actions`, ineffective actions
also spend their configured duration. The action world's `observable` flag
controls whether regime identity appears in the key. Delayed progress uses
`sequence` or `wait` mode and exposes partial progress as context.

Diagnostics count admitted suffix observations. Arrival histograms count
transitions into a location; resource refill counts require a stock increase.
Delayed-progress histograms include distraction observations in their zero bin.
Exported archive entries include ancestry; `live_entries` reports active holders.
Continuation job counts and work come from the campaign stream.

## Configuration bounds

Every parameter is required. Bounds keep exhaustive enumeration below 100,000
states; requests exceeding the reachability limit are rejected.

| Family | Parameters |
| --- | --- |
| Resource | `initial_charge` 0–31; `initial_health` 1–31; `barrier_charge` 1–15; `route_cost`, `health_cost`, and `refill_health_cost` 0–15; `refill_amount` and `max_charge` 1–31; `corridor_len` 1–12; `initial_charge` ≤ `max_charge`. |
| Maze | `length` 1–8; `pattern` < `1 << length`; boolean `reverse_actions`. |
| Actions | `segment_len` 1–8; `water_action` 1–3; boolean `return_to_land` and `observable`. |
| Deadline | `length` 1–8; `initial_time` 1–64; `fast_ticks` 1–4; `slow_ticks` > twice `fast_ticks` and ≤ 16; `obstacle_ticks` 1–16. |
| Delayed | `horizon` 1–16; `distractions` 1–32; `mode` is `sequence` or `wait`. |
| Deadline/actions | `actions` uses the actions parameters; `initial_time` 1–64; four `action_ticks` values, each 1–16. |
| Route | `length` 2–16; `pattern` encodes two-bit actions per position; `attack` 0–3; boolean `shifted` and `upgrade_required`. |

## Scenario chains

A chain has one to sixteen `stages`, each containing a leaf `world` and
`refill_available`. Supported leaves are resource, maze, actions, deadline,
delayed, and deadline/actions. Completion enters the next stage in the same
action. The final stage's goal is the campaign objective. Snapshots contain the
active stage, local state, and carried charge. Health, history, and clocks reset
on stage entry; archived snapshots allow exploration from earlier stages.

With `carry_charge=false`, each stage starts from its declared initial state and
chain `initial_charge` is zero. Stage identity namespaces archive places. With
`carry_charge=true`, chain `initial_charge` initializes persistent charge;
resource stages consume and replenish it, while other stages preserve it.
Resource stages share a capacity and declare local `initial_charge=0`.
`refill_available=false` disables the resource refill action. Other leaves
require this flag to be true.

Chains use a uniform four-action alphabet and a 512-action path limit.
Standalone worlds use a 128-action limit. A chain job can overshoot its admission
budget by at most 511 transitions. Across stage namespaces, preferences compare
persistent stock; within a stage they also compare local resources. Whole-chain
reachability is checked independently of each stage's reachability.

Chain diagnostics include stage-entry work and charge distributions, suffix
actions per stage, and executed work grouped by parent stage and charge.
Pre-objective counters include the objective job and are checked against work to
the first objective, or total work for an unfinished campaign.

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

## Checks

```sh
cargo test --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml -- --test-threads=1
cargo fmt --manifest-path workloads/tiny-worlds/Cargo.toml -- --check
cargo clippy --locked --release --manifest-path workloads/tiny-worlds/Cargo.toml --all-targets -- -D warnings
```

Tests cover exhaustive reachability, snapshot restoration, archive retention in
both insertion orders, controlled objective witnesses, chain boundaries, route
alignment, trace validation, replay, and execution-work accounting.
