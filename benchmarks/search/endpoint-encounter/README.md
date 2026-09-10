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

## S02 result: partial damage can be extended; survival is separate

The [conditional-transition argument](return-extend-argument.md) states the
assumptions under which returning to partial results could save repeated work.
It does not treat observed 1/32 damage as a memoryless probability at every HP
level. [S02](s02-registration.json), frozen at `9e146804`, tests only the next
step from the first S01 damage endpoint, with unchanged resources, executable
and suffix list. This selected state and reused list are dependent diagnostics.

S02 completes all 64 exact root restores in 92.05 seconds, with 12,548 KiB peak
child RSS. Five ordinary trials produce further surviving damage observations.
The first saved endpoint follows five commands / 147 continuation frames and
has boss HP **135**, down from 139, with health still 79 tenths. Two complete
held-command replays agree on its live mechanical state, raw context and emulator
bytes. This witness includes the original encounter prefix and both searched
damage extensions. Only that first stored example receives two held replays;
the other four successes are observed trial endpoints.

All ordinary trials eventually die (minimum/median/maximum 8 / 34 / 1,512
continuation frames), with no defeat. All passive trials reach the 128-command
limit alive, at 4,187–6,762 frames, with health 79 and boss HP 139 unchanged.
Their different endpoints are horizons on one released-button trajectory.
The passive result rules out inevitable immediate death at this exact state,
without establishing that its resources are adequate for a fight. Damage and
survival are distinct outcomes; neither alone supplies a useful control policy.
Do not compare S01's 1/32 and S02's 5/32 as an independently confirmed gain.

The [analysis](s02-analysis.json) and losslessly compressed `s02-output/` preserve
all trials, identities, costs and the saved witness. The shared scorer now
accepts `--panel s02` and uses the registered 119,072-frame prefix/setup cost;
S01 still recomputes identically. Eleven offline evidence tests pass. The
service is terminal and successful. The block charges **539,906 auxiliary
frames** (119,072 prefix/setup + 182,396 continuation + 238,438 verification),
below its separate 2M ceiling. [The closed ledger](ledger-after-s02.json) totals
1,083,261,532 resumed admitted search and 50,017,208 known auxiliary frames;
prior physical-cost gaps and historical tranche ceilings remain separate.

This closes the registered two-stage diagnosis. It earns no third manually
selected root, boss-HP reward or policy promotion. A useful next discriminator
is the unchanged archive search's behavior from the **original** E01 encounter.
The shared engine already supports `CampaignOrigin::SnapshotRoot`, but the
current `nes-eval` command admits prefix origins only for MM2; it cannot be
assumed to run a Metroid snapshot challenge unchanged. A bounded adapter must
qualify root identity and complete-prefix replay, label supplied-state outcomes
explicitly, and account for setup before any such allocation. No new native
work follows merely from this implementation audit.

## AQ01: the existing archive engine is qualified for the supplied root

The standalone [challenge contract](archive-challenge-contract.md) uses the
existing `CampaignOrigin::SnapshotRoot` and leaves the engine, target, selectors
and legacy evaluators unchanged. Source `02798b44` is frozen and built on msr1
with the same motion/endpoint observation features as C01. The separately
[registered preparation](aq01-prepare-registration.json), at `be0111bb`, spends
237,608 direct frames in two held replays; Python independently verifies the
exported emulator bytes, raw context, decoded state, terminal markers and
117,875-frame route count against E01. Preparation takes 32.01 seconds.

The resulting snapshot digest is frozen in [two 16-job requests](aq01-run-registration.json)
at `65873148`. They differ only in one/two physical result-buffer slots and use
disjoint CPU sets. Both finish successfully in 21.01 / 28.01 seconds, with about
21 MiB reported peak child RSS. Each admits 1,954 frames, completes full campaign
replay, and verifies its one-action champion twice from the supplied root and
twice as a full prefix-plus-local input from genesis. This is an integration
fixture, not a capability result. Neither observes a defeat.

[The verifier](verify_archive_challenge.py) confirms identical streams, origin
and final checkpoints, reports, root snapshots and witness inputs between
buffer configurations. The root-local/full-prefix distinction is explicit; an
inherited root milestone or post-budget event cannot pass. Four evidence tests include changed emulator bytes, a duplicated prefix and a
missing campaign replay. All 15 endpoint evidence tests pass, as do the new
Rust command's three focused tests and strict all-feature Clippy. Repository
fast gates pass 1,159 tests with 23 skipped in 33.135 seconds.

All raw qualification records are losslessly compressed in `aq01-output/`.
The [analysis](aq01-analysis.json) totals **1,200,260 known auxiliary frames**:
237,608 preparation, 954,836 campaign helper frames, 3,908 campaign admitted
frames and 3,908 replay-admitted frames. This is below the separate 4M ceiling.
All services are terminal. [The closed ledger](ledger-after-aq01.json) records
1,083,261,532 resumed admitted search and 51,217,468 known auxiliary frames.
Engine setup, unadmitted work and reconstruction remain additional unknown
costs; this qualification makes no total-physical-work or throughput claim.

## AP01: no defeat at the fixed supplied-root horizon

The [single preregistered capability pilot](ap01-decision.md), frozen at
`a8fbbf6c`, uses the unchanged ordinary archive from the original E01 state.
It completes **2,548 jobs and 250,267 admitted frames**, including a 267-frame
bounded drain, in **38.02 seconds**. The 250k horizon is complete; there is no
Ridley-defeat observation. Full campaign replay and both root-local / full-prefix
witness pairs agree. The one-action champion is still near the origin and has
boss HP140; this selected witness cannot describe all generated or retained
branches. Reported peak child RSS is 52,760 KiB.

The final sidecar reports 1,882 classified encounter endpoints among 4,327
eligible live endpoints, 1,598 deaths and 1,032 cumulative admissions. It reports
557 active entries, **588 resident snapshots**, no snapshot eviction or entry
drop, and complete cached-active resource coverage with maximum health79. The
13-MiB checkpoint exports every cached snapshot, including inactive history:
`Archive::take_entry_reports_and_snapshots` does not filter for active selection.
Do not conflate those 588 resident snapshots with the 557 active entries, or
infer absence of damage from the champion's HP140.

[The scorer](score_archive_pilot.py) independently classifies the fixed horizon
from native process completion, checks report/campaign counters, origin and
witness composition, and preserves budget drain and replay costs. Its planted
checks reject origin-zero/post-budget success and execution/wall censoring as
completed negatives. All 18 endpoint evidence tests pass. The losslessly
compressed `ap01-output/` includes the full stream and final checkpoint for
inspection without repeating the campaign.

[The analysis](ap01-analysis.json) charges **977,952 known auxiliary frames**:
250,267 admitted challenge frames, 250,267 replay-admitted frames and 477,418
direct helper frames including 5,574 setup frames. This is below the separate
3M ceiling. The service is terminal and successful. [The ledger](ledger-after-ap01.json)
closes at 1,083,261,532 resumed admitted-search and 52,195,420 known auxiliary
frames. Engine setup, unadmitted work and reconstruction gaps remain explicit.
No performance-search allocation, matched-control gain or fresh victory is
claimed. This result earns inspection of the existing retained evidence before
a policy choice; no retry, larger sweep or another selected root follows.

## CI01: the ordinary archive retained and revisited lower-HP boss states

The [paused inspection](ci01-decision.md) finishes in **1.02 seconds** with
44.9 MiB peak child RSS. All 589 restores (588 cached entries plus the qualified
E01 control) preserve the entire snapshot and physical clock. Actual work is
**929 setup frames, zero gameplay actions**. The service is terminal. Native
metadata exactly matches the independent local typed inventory.

The source-backed active mapping identifies 557 representatives and 31 inactive
cached snapshots. Among 166 classified active endpoints, boss HP is 140 at 160,
139 at 3, and 131, 130, 129 at one each. All six endpoints below140 have unchanged
health 79 and zero missiles. The five classified inactive cached snapshots all
have HP 140. Missing historical snapshots and rejected endpoints remain unknown;
HP is a snapshot-local observation, not exact lifetime damage.

The [recomputable analysis](ci01-analysis.json) joins the six active IDs to the
recorded stream. They receive 27 later jobs; the HP 129 state receives four and
none produces a retention candidate. The chain 530→978→979→982 has recorded
parent links and HP 139→131→130→129. Full AP01 replay already passed; CI01 adds
no new search or witness trajectory. This establishes retained and revisited
lower-HP states, while leaving the cause of failed continuation unresolved.

[The scorer](score_checkpoint_inspection.py) refuses the active inference if
snapshots are missing, entries dropped, the input cap binds, or the fixed policy
preconditions change. Its tests also keep unclassified/HP255 states unavailable.
All 21 endpoint evidence tests pass; the new Rust decoder's malformed-format /
duplicate-ID check and strict all-feature Clippy pass. Compressed exact records
are in `ci01-output/`. [The ledger](ledger-after-ci01.json) closes at
1,083,261,532 resumed admitted search and **52,196,349 known auxiliary frames**;
earlier unknown physical-work gaps remain separate.

The [next candidate](next-mechanism.md) is a bounded local retry after an ordinary
terminal action. An exact finite model includes both a favorable aliasing case
and an alive-trap counterexample. It earns only a bounded source/fixture decision;
no new native panel or policy promotion is allocated by this census.
