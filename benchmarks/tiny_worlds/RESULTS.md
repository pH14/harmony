<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Synthetic tiny-world results

These results cover the registered synthetic workloads in `panel.json`. They
exercise the search engine, bounded-work accounting, and feature-retention
mechanisms without licensed game assets. They do not estimate game performance
or establish autonomous routes in a commercial game. Native local calibration
is reported separately in [CALIBRATION.md](CALIBRATION.md).

All three final panel jobs completed and verified. Development covered 14 cases
with three seeds and two arms (84 jobs); held-out validation covered 12 cases
with five seeds and two arms (120 jobs); the development-seed sweep covered 57
parameter/budget settings with three seeds and two arms (342 jobs). The sweep
budgets were 250, 1,000, and 4,000 logical transitions. Validation seeds were
2026092401, 2026092407, 2026092419, 2026092431, and 2026092443, in the work
lists below. Development and sweep used seeds 17, 29, and 43.

The held-out table reports each arm's objective-hit count and Wilson 95%
interval. The interval is descriptive and wide at five trials: 5/5 corresponds
to [0.566, 1.000], 3/5 to [0.231, 0.882], and 0/5 to [0, 0.434]. These small,
fixed-seed panels do not support broad probability claims. Listed work values
are only for successful seeds, in the seed order above; a dash means no success.
Every unsuccessful validation seed is right-censored at 4,000 work. No averages
include those censored trials.

| Held-out case | Normal hits (Wilson 95%) | Successful work | Broken-control hits (Wilson 95%) | Successful work |
|---|---:|---:|---:|---:|
| Resource route | 5/5 [0.566, 1.000] | 60, 36, 194, 38, 58 | 0/5 [0, 0.434] | — |
| Unnecessary refill | 5/5 [0.566, 1.000] | 13, 58, 6, 20, 28 | 5/5 [0.566, 1.000] | 18, 58, 6, 27, 12 |
| Reversed maze | 5/5 [0.566, 1.000] | 168, 324, 109, 105, 640 | 0/5 [0, 0.434] | — |
| Long maze | 3/5 [0.231, 0.882] | 1793, 618, 1007 | 0/5 [0, 0.434] | — |
| Permuted actions | 5/5 [0.566, 1.000] | 263, 299, 230, 168, 138 | 0/5 [0, 0.434] | — |
| Unsignaled actions | 0/5 [0, 0.434] | — | 0/5 [0, 0.434] | — |
| Tight deadline | 5/5 [0.566, 1.000] | 64, 49, 100, 67, 39 | 0/5 [0, 0.434] | — |
| Long deadline | 5/5 [0.566, 1.000] | 112, 116, 81, 52, 102 | 0/5 [0, 0.434] | — |
| Delayed sequence | 5/5 [0.566, 1.000] | 1637, 3023, 1119, 1691, 626 | 0/5 [0, 0.434] | — |
| Delayed wait | 5/5 [0.566, 1.000] | 1312, 656, 1311, 808, 1775 | 0/5 [0, 0.434] | — |
| Deadline/action interaction | 5/5 [0.566, 1.000] | 67, 71, 115, 93, 64 | 0/5 [0, 0.434] | — |
| Resource propagation | 5/5 [0.566, 1.000] | 81, 91, 132, 78, 65 | 0/5 [0, 0.434] | — |

The development panel was consistent with the registered feature controls on
most cases: normal/broken hits were 3/3 vs 0/3 for resource five-charge,
resource route, resource health trade-off, maze history, land/water actions,
return actions, tight deadline, delayed sequence, delayed wait,
deadline/action interaction, and resource propagation. The stress maze was
1/3 vs 0/3. The unsignaled action case was 0/3 in both arms. The small deadline
case was 3/3 in both arms, showing that this instance does not isolate the
deadline feature.

## Parameter sweep

Each cell is normal-arm objective hits out of three seeds at work budgets
250 / 1,000 / 4,000. Broken-control hits were 0/3 at every budget except the
two-step deadline case (2/3, 3/3, 3/3). The curves show sensitivity to the
registered parameters and budget; they are noisy three-seed observations, not
monotonic laws or a basis for choosing general search settings.

| Family and varied parameter | Normal hits at 250 / 1,000 / 4,000 | Broken hits at 250 / 1,000 / 4,000 |
|---|---:|---:|
| Resource corridor length 1 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Resource corridor length 3 | 2/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Resource corridor length 5 | 0/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Maze length 2 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Maze length 4 | 1/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Maze length 6 | 0/3, 0/3, 3/3 | 0/3, 0/3, 0/3 |
| Maze length 8 | 0/3, 0/3, 1/3 | 0/3, 0/3, 0/3 |
| Action segment length 1 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Action segment length 4 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Action segment length 8 | 0/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Deadline length 2 | 3/3, 3/3, 3/3 | 2/3, 3/3, 3/3 |
| Deadline length 4 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Deadline length 6 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Delayed horizon 3 | 1/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Delayed horizon 6 | 0/3, 2/3, 3/3 | 0/3, 0/3, 0/3 |
| Delayed horizon 9 | 0/3, 0/3, 3/3 | 0/3, 0/3, 0/3 |
| Deadline/action segment length 2 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Deadline/action segment length 3 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |
| Deadline/action segment length 4 | 3/3, 3/3, 3/3 | 0/3, 0/3, 0/3 |

The delayed-workload gains required larger budgets as the horizon grew: the
normal arm reached 0/3, 2/3, and 3/3 for horizon 6 at 250, 1,000, and 4,000;
horizon 9 remained 0/3 through 1,000 and reached 3/3 at 4,000. Deadline/action
lengths solved in all three seeds at every budget, while the two-step deadline
control also solved at larger budgets. That control result limits any claim that
the deadline-key feature is necessary for every deadline instance.

## Resource propagation evidence

The development resource-propagation configuration required five charges at the barrier after
a four-transition route consuming two charges per step and charged one health per route step. It refilled
two charges at a station, up to 14. The normal arm's first objective appeared
at work 173, 166, and 218 for seeds 17, 29, and 43; each run recorded 9, 9, and
11 continuation jobs/work units respectively. Corridor arrival-charge counts
were: seed 17, charge 0:1, 2:3, 4:3, 6:9; seed 29, 0:4, 2:2, 6:7; seed 43,
0:2, 2:1, 4:2, 6:4. The separate health-tradeoff case observed 18, 17, and 17
health recoveries, alongside 20, 21, and 19 refills; it reached its objective
in all three normal seeds at work 308, 352, and 339.

On the held-out resource-propagation configuration (five transitions, refill
of three charges, two health per route step), all five normal seeds reached the
objective at work 81, 91, 132, 78, and 65. The arrival histogram contained
charge-7 arrivals in all five runs (8, 9, 9, 7, and 11 such arrivals); observed
continuation jobs/work were 8, 0, 5, 0, and 1. This is evidence that charge
states reached later route transitions and that the continuation mechanism was
exercised in some runs; it is not an estimate of general resource-search
reliability. The broken arm reached 0/5.

## Runtime, resource use, and reproducibility

| Split | Jobs | Panel elapsed | Supervisor elapsed | Peak process-group RSS sampled |
|---|---:|---:|---:|---:|
| Development | 84 | 7.42 s | 7.98 s | 28,180,480 bytes (26.9 MiB) |
| Validation | 120 | 10.58 s | 10.94 s | 31,850,496 bytes (30.4 MiB) |
| Sweep | 342 | 14.39 s | 14.92 s | 34,619,392 bytes (33.0 MiB) |

Together the panels took 32.4 seconds of panel time and 33.8 seconds including
the supervisor wrappers on this host, below the 60-second targeted-run and
300-second broad-sweep limits. RSS is a sampled process-group peak, not a hard
memory cap. Each job used one reserved CPU slot, 1 GB memory reservation, and
0.1 GB artifact-disk reservation; these are bounded local synthetic tests, not
hardware-independent performance claims.

The following path-independent hashes identify the engine, workload, panel,
and compiled panel executable used in all three splits:

| Artifact | Identity |
|---|---|
| Engine baseline revision | `67f65bbce5ee84b57cd91aaceb49350325eb49b9` |
| Engine source SHA-256 | `98577556455bbca4109eb7ec7a93217dd71100e061f464de6f42ada785df7a70` |
| Workload source SHA-256 | `e38a72d947e4d5060f8bd8f3eb34ef88e9e6d4b6ea5ce701ef4dc8955531deef` |
| Registered panel SHA-256 | `175377809f4a7aa02d23b9775ae88fdb617e4679e4e29603d441ccc682be76c8` |
| Panel binary SHA-256 | `8e5f9bac6cdd706a7600d21bf85408066fa7ecf756493dc59cc112e7c75a24a0` |

The fixed seed counts, arrival observations, continuation work, and sweep rows
are reported as finite synthetic evidence. Search success is scored only when
the objective was observed within budget; unsolved trials remain censored at
their budget. The implementation and public report contain no licensed assets,
private asset paths, snapshots, or run traces.
