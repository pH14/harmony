# Live endpoint encounter qualification

This narrowly scoped report addresses the missing generated-encounter evidence
identified after the failed no-cost depth, component-action and whole-repeat
confirmation screens. The [contract](../depth-transfer/endpoint-encounter-contract.md)
and [implementation](../../../workloads/nes/src/metroid/README.md#experimental-endpoint-encounters)
define endpoint eligibility, interpretation and costs. No new search policy or
performance gain is claimed.

The next native qualification uses the historical Metroid seed 2026090902,
2,000 executions, four workers, 8 GiB archive and fixed ordinary action law.
The no-cost selector is retained solely to reproduce that saved fixture; its
failed depth gate stays closed. msr1 runs a default build and an audited build;
ms02 runs the audited build with one and two physical result slots. Every cell
uses full campaign replay and the existing runner's time, process and output
bounds. Freeze exact source/build/core identities and resource ceilings before
emulation; compilation does not allocate game frames.

Require exact historical default stream/campaign/checkpoint hashes on msr1.
Require replay and exact audited buffering equality on ms02. Compare all job
scheduling, seeds, parents, work and retention decisions after removing only
version-dependent semantic result digests; compare checkpoint game state after
removing only the new observation marker and explicit identity fields. Verify
memory accounting separately rather than silently deleting memory differences.
Cross-host comparisons account for different core binary hashes. This reused
short fixture cannot qualify a positive boss episode or establish performance
neutrality at resource limits. A first prospective positive still requires
standalone observer verification before any local combat allocation.

## Q01 result

The [registered block](q01-registration.json), frozen at `198d32dd`, passes all
four cells using source `d2293b9c`. Each performs exactly 2,000 jobs and 284,283
admitted frames, followed by full replay and two witness replays. Default stream,
campaign and checkpoint bytes exactly match the saved historical fixture. All
five declared semantic projections agree across feature builds and hosts,
including all 1,139 retained emulator snapshots. Audited one/two-slot artifacts
are byte identical. Logical resident and snapshot memory are identical in this
fixture; the new field fits existing layout padding on these builds. This does
not establish zero resource overhead generally.

All audited runs count 6,784 admitted actions, 6,628 eligible live endpoints and
zero classified encounters. This is expected for the reused short fixture and
qualifies the negative/reporting path only. First-positive native input export
and its observer confirmation remain unqualified. The local planted positive
cases and reference classifier evidence remain separate.

Both services completed successfully within one minute of launch, with no
owned child remaining. [The score](q01-analysis.json) recomputes from the compact
raw records using `python3 score.py --out /tmp/endpoint-q01-recomputed.json`.
The standalone native verifier additionally checks the full retained artifacts
on each host. This block charges 2,281,896 known auxiliary frames against its
4M ceiling; full-campaign replay cost is inferred from admitted work. Setup,
reconstruction and unadmitted work remain unknown. Cumulative resumed totals
are 842,318,922 admitted performance-search and 47,622,115 known auxiliary frames
in [the ledger](ledger-after-q01.json). No new performance allocation was used.

## C01 and the first positive endpoint

The [development census](c01-decision.md) stopped at its 35-minute search cap
with 240,942,610 admitted frames. The planned 250M horizon was too high for that
cap at the measured throughput; it is **incomplete**, with no extension or
replacement. The evaluation process finished its witness checks successfully,
then the frozen panel runner rejected the unexpected wall censoring. The failed
panel and terminal service status are preserved, rather than relabeled passed.

The completed output nevertheless contains an existential positive: 4,973
classified endpoint observations among 5,061,181 eligible live endpoint
observations. These are observations, not unique states or independent trials.
The first event is execution 1,853,341, Ridley area, slot mask 1, at route action
endpoint 117,875. Its saved input passes two identical endpoint replays. Both
boss-area discovery events and the five earlier pickup/area events match the
chosen historical development run. This does not establish complete historical
identity or turn a reused seed into fresh validation.

[E01](e01-registration.json) prospectively registers a separate standalone
three-pass observation of that producing input on msr1. All passes agree on
endpoint and emulator state. Python checks 20,782 consecutive retained intervals
against native outputs and confirms the endpoint's classification. The tape
contains 52 classified Ridley frames, from 117,824 through 117,875, and no observed
boss HP decrease. Its endpoint has decoded health 79, zero missiles, missile
capacity 20, equipment 17 and boss HP 140. This describes the saved first tape;
damage and resources in the other campaign branches remain unmeasured. Fight
capability, actual retention/selection history and any useful global policy
change are still open questions.

The [C01 reader](analyze_census.py) preserves reported observations and measured
work while leaving the fixed-horizon gate incomplete. The separate
[E01 verifier](verify_e01.py) proves the positive episode without depending on a
complete negative census. Both machine services are terminal with no owned
process remaining. C01 charges 1,120,250 known replay frames; E01 adds 356,412
including setup. The [closed ledger](ledger-after-c01-e01.json) totals
1,083,261,532 admitted search frames and 49,098,777 known auxiliary frames since
resumption, with the old tranche and unknown costs kept separate. No further
fresh search, local combat or observer extension has been dispatched.

## S01 conditional control design

The [control contract](control-contract.md) defines one 32-pair ordinary/passive
diagnostic from E01's exact endpoint, with unchanged resources. The frozen draw
request selects 128 ordinary commands per seed before emulation. Planted checks
reject damage followed by death in the same command, stale/missing context and
inherited earlier-command progress. Native execution requires a separate
registration with source, build, generated suffix and resource identities.
No emulator work is allocated merely by this design or seed freeze.

## S01 result: reproducible damage, no sustained fight

[S01](s01-registration.json), frozen at `d94fb3d0`, completes all 32 ordinary /
passive pairs on msr1 in 53.04 seconds with a 10,376 KiB reported peak child RSS.
All 64 restores preserve the positive snapshot and raw context. The first state
has health 79 **in tenths** (7.9 game energy), zero missiles and boss HP 140.
The source uses `MetroidMechanicalState.health` in tenths throughout; the earlier
raw decoded value must not be read as 79 game energy.

One ordinary trial has one guarded HP decrease, at continuation frame 266. Its
ninth command ends at frame 268 with boss HP 139 and unchanged health 79; two
fresh held-command replays reproduce the live mechanical state, raw context
and emulator bytes. That trial dies at frame 285. Every ordinary trial dies
(minimum/median/maximum continuation: 207 / 311.5 / 1,506 frames), with no defeat
flag. Every passive trial dies at frame 316 without observed damage. Passive
trials are the same released-button physical trajectory under different command
boundaries, not 32 independent gameplay replications. No power or global
success-rate claim is made.

The [analysis](s01-analysis.json) preserves earlier surviving damage separately
from later death. It checks the full frozen order, draws, per-trial frame totals,
required witness set, exact searched-prefix/suffix composition and replay
charges. Four offline tests plant corrupted draws, changed witness bytes and a
missing required witness; the positive fixture also prevents later death from
erasing an earlier surviving endpoint. Recompute without emulation:

```sh
python3 benchmarks/search/endpoint-encounter/score_s01.py \
  --output benchmarks/search/endpoint-encounter/s01-output \
  --out /tmp/s01-recomputed.json
```

The raw records and witness are compressed losslessly in `s01-output/`. This
block uses 118,804 prefix/setup, 21,577 continuation and 238,144 verification
frames: **378,525 known auxiliary frames**, below its 2M ceiling. The service
exits successfully with no owned child. [The ledger](ledger-after-s01.json)
closes at 1,083,261,532 resumed admitted search and 49,477,302 known auxiliary
frames. No fresh performance-search frames were used.

This demonstrates one reproducible ordinary-input damage event from a weak
searched state. It does not establish sustained control, why all trials died,
actual loss of a useful archive state, or a better generic policy. The current
archive key does not encode boss HP, but the initial and damage endpoints occupy
different map/position cells; S01 therefore does not demonstrate an actual
same-cell replacement. The useful next discriminator is whether the saved
partial progress can itself be extended, under a separately registered bounded
diagnostic. Do not launch another discovery campaign from this observation.
