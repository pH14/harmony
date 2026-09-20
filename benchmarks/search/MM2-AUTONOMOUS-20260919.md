<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Autonomous Mega Man 2 campaign

Deadline: September 20, 2026 at 08:00 America/New_York (12:00 UTC).

Success means a campaign beginning from a recorded power-on boot prelude,
choosing its own stages and inputs, and reaching the ending. Its final tape
must replay continuously from power-on without intermediate restores. The
prior assisted completion is diagnostic evidence, not an autonomous result.

## Ownership and evidence

The adapter owns truthful RAM decoding, controller semantics, scene identity,
mechanical capabilities, game outcomes, and documented evaluation contracts.
The generic searcher owns selection, retention, rollout horizons, mutation,
continuation discovery, recording, and replay. No chosen obstacle weapon,
winning controller bank, manually selected intermediate root, imported solution
tape, or automatic gameplay transition is permitted in a qualifying campaign.
Rooted diagnostic probes are labelled separately and cannot establish success.

One PR will contain semantic commits separating adapter work, core searcher
work, and experiment evidence. Existing assisted evidence and PR 362 are kept.

## Milestones

1. Provide whole-game execution with recorded actions through stage selection,
   deaths, Continue, stage awards, and the ending. Remove obstacle-specific
   ammunition identity from the qualifying key and distinguish actual inventory
   identities. Check snapshot restoration and continuous frame accounting.
2. Reproduce the generic same-key waiting failure. Test a bounded generic
   remedy, including replay and negative cases, before using it in a game run.
3. Run fresh power-on campaigns on msr1 and at most eight Mac cores. Inspect
   archive support, actual continuation outcomes, and subagent-reviewed film;
   change mechanisms only in response to measured failures.
4. Verify a discovered ending on both hosts with one uninterrupted controller
   tape, publish hashes and evidence, and report any unresolved limitation.

## Initial findings

The prior target ends at the first boss and automatically supplies award
settling frames. It cannot represent the requested experiment. Its optional
coordinate-consistency rejection is an unproven diagnostic, not mechanical
truth. The Crash-only ammunition key is obstacle advice and must not be part
of the autonomous policy.

Generic archive admission retains the cheaper equal-preference state in a full
slot. This rejects a strict same-key waiting continuation; ordinary random
suffixes contain at most six actions. A constant-key seven-step fixture is the
initial searcher counterexample. Unconditional replacement by later descendants
would discard the cheap representative and is not accepted without evaluating
cycle and destructive-drift behavior.

## First qualification

A fresh seed-1 whole-game run on msr1 completed 2,000 executions. Full campaign
replay reproduced its report and snapshots exactly; video replay matched its
decoded endpoint. It selected and entered a stage using searched inputs. No boss
or ending was reached. The report watermark includes raw stage-selection labels
and is not a trustworthy deepest-stage metric; inspect retained contexts and film.

Baseline uses the unchanged searcher from ea4d3116 and the new adapter. Two fresh
100,000-execution controls use seeds 1 and 2, five workers each, and 12,000 MiB
memory budgets. Their files live under `/root/mm2-autonomous-20260919/runs/`.
The adaptive-horizon branch is evaluated separately; equal seeds do not imply
equal actions after its policies diverge.

Qualification stream SHA-256:
`b547be50d2c046c1ec64b2c2a6066d7c92926f5a47f2484986a5c9c3d7527abf`.
Qualification checkpoint SHA-256:
`8b3b2dab1e685e66e55904db9c7ded5136c99314b5b820ab902c1e057cb6c9b3`.

## Observation audit after the first controls

Both fresh 100K controls reached multiple screens in all eight ordinary stages,
with no boss entry or kill. Seed 1 used 14,672,002 emulated frames and retained
49,371 active representatives; seed 2 used 14,746,809 frames and retained 46,795.
These counts alone are not progress evidence.

Seed 1 contained 18,206 retained endpoints whose bank-`0x0d` `$fd/$fe` values
were outside the source-defined weapon-menu cursor range. Across all bank-`0x0d`
entries there were 2,209 distinct raw pairs. The experimental raw tuple in v25
therefore split reused drawing/animation scratch into excessive local identities.
V26 removes that tuple from the key, bounds candidate menu selections by the
source's page/row ranges, and preserves raw bytes only as diagnostic observations.
Valid range alone is not a scene detector: Game Over reuses the same registers.

The generic horizon prototype passed its constant-action fixture but failed
real two-worker replay at job 2 (196 measured work units versus 97 recorded).
The live choice used reservation-time barren feedback, while replay initially
recomputed from admission-time feedback. That prototype is unqualified. The fix
records the chosen extension with the job and validates replay bounds; it needs
multiworker variable-action and real-game replay checks before long runs.

The preserved assisted tape passed the new target oracle: all 6,800 actions,
exact supplied-frame work, snapshot restoration, and ending at action 6697.
The six source-backed castle transitions were observed exactly once each.
The boot-only film independently confirms the fixed 17-action, 1,512-frame
prelude ends on the stage-selection grid. Boot tape SHA-256:
`d19eda0b7d33793a968db230a00746f5518892ffe4d1efa231b2113418cea96`.
None of these oracle inputs seed a qualifying campaign.

## Repaired replay qualification

The fixed core records its reservation-time horizon extension on job and skip
records (generic schema 6) and keeps extension policy private to the engine.
Default variable-length/variable-action multiworker tests now cover reservation
lag. A fresh 2,000-execution real-game run with the unchanged v25 adapter passed
full report/snapshot replay on msr1. Stream SHA-256:
`338e0dd06f412e6c48843420c51a2c757a0282bd1ea7374434e600c882de6ba3`.

The integrated v26 adapter (bounded menu identities and confirmed castle clears)
also passed a fresh 2,000-execution replay on the Mac, with stream SHA-256:
`153111dbb1a80a62ab6d09296420f78a11ef45388e320ce2c3a51a9b77aae108`.
MM2 stream/checkpoint format is v8. Its 45 active MM2 library tests and seven
CLI tests passed; the external-ROM oracle is separately opt-in. Targeted Clippy
passed. These are qualification checks, not game-completion claims.

At 23:02 UTC, the paired v25 seed-1 horizon arm and the integrated v26 seed-1
power-on campaign began fresh 100K budgets with five workers each. The longer
unchanged seed-1 control continues from its complete autonomous 100K archive;
archive import is a warm start, not an exact continuation of the random schedule.

## First paired horizon results and current barrier

The v25 100K arms completed with no boss entry or kill. Control seeds 1/2
used 14.67/14.75M frames and reached screen 9. The resettable-horizon arms
used 16.56/16.68M frames and reached screens 11/12. Screen maxima across
stages are only scouting indicators; these results do not establish a gain
per unit of emulator work. The control warm start added 400K jobs, reaching
screen 20 without a boss encounter.

The integrated v26 fresh seed-1 run used 16,568,558 frames over 100K jobs.
It retained 38,874 active representatives, including stage 5 screen 17 at
health 2. A separate fresh seed-2 500K campaign is running. Films of the
v25 controls show actual low-health frontiers: Quick Man screen 9 at health
2; Metal Man screen 8 at health 4 after autonomous deaths, Continue, and
stage reselection. These are live gameplay endpoints, not completion.

The actual Wily 6 intro diagnostic did not cross its wait with either
unchanged or resettable-horizon core (2K jobs, one worker, same root and seed).
The latter spent 423,458 versus 297,960 frames, but extensions never exceeded
5 extra actions: selector resets were clearing horizon feedback. Commit
4947531d introduces a separate counter driven only by actual unproductive
admissions and retained descendants. A 20-step constant-cell wait fixture
with default retirement now passes, including replay and archive import;
a real-game probe is still required.

A separate route-replay experiment freezes commit 44d3b92f with the same
v26 adapter and resettable core. The only experimental source edit changes
the CLI's default mixture from AlphabetOnly to AlphabetContinuation; no
route or preferred action is supplied. Both binaries are stored under
`/root/mm2-autonomous-20260919/experiment-bins/`. The arm passed a fresh
2K-job, two-worker full report/snapshot replay before its 100K comparison.
Readouts must include surviving improved arrivals, frontier health, boss
encounters, and work; continuation job counts alone are not success.

The v26 seed-1 archive was produced before manifest output existed. Its
manifest was backfilled from its own recorded genesis header and exact
archive/snapshot hashes, with no external inputs. Archive SHA-256:
`858f7126d02fa676db0f3082f891535397c3bc7797e93e7724e1382ccb9e2090`.
Snapshot SHA-256:
`8568970b7d53f84059933ec541108e54342764120965a33809bc714acd27dc7e`.
The new CLI must validate this manifest before any warm start.

The first v26 route-replay arm completed 100K jobs using 15,516,113 frames.
It had 8,891 continuation jobs (538,147 frames), 7,165 retained admissions,
and 3,144 of those IDs present in the export. Export presence alone does not
prove an entry is active: historical ancestors can remain in the report.
A conservative same-destination check found 43 health-improving admissions
whose original route leaf was still comparable. Of these, 37 were the unique
maximum-health entry in their exact slot across the final export; with zero
population drops, they establish surviving improved representatives. This
undercounts outcomes whose original leaf disappeared and makes no claim for
the others. The arm reached Metal Man screen 9 at health 10 (control screen 5,
health 4), but only stage 5 screen 4 (control screen 17). Neither found a boss.
A second paired seed is required before choosing the mechanism.

The v26 seed-1 control reproduced exactly on Mac and msr1: 16,568,558 frames,
38,874 active entries, and the same progress. The emulator binary hashes
remain platform-specific and manifest validation keeps those identities exact.

## First autonomous boss encounter

The fresh v26 seed-2 500K run completed 85,261,808 emulated frames, retaining
140,599 active entries. First boss encounter was recorded at job 332542;
no boss clear yet. Film verifies Crash Man, a death, an ordinary retry from
full health, and 16 damage on the second attempt. The final export contains
positive-health combat entries only in stage 7 screen 19, with health up to
20 and boss-damage bucket up to 9 (18 damage). Other stage aliases carrying
boss damage have zero player health: damage RAM survives death through menus.
This is an evaluation-lifecycle issue to check before trusting pooled damage
priority. Per-context census maxima are independent; max health and max damage
in one row need not describe the same entry.

The copied champion diagnostic replay used the exact boot plus discovered
input (473 actions, 22,558 frames), SHA-256
`733f607f4904c01b10f1d991846e403a1083606a51844f770e2fe439a8050b2c`.
Film: `/private/tmp/mm2-autonomous-20260919/v26-s2-boss-witness-tail.mp4`.

The completed v26 seed-2 bundle's manifest was backfilled from its own
recorded genesis header and exact producer outputs. Archive SHA-256:
`49eff797a2ebf71f658807e8332b3123cb642234c94bcdf47a32e7f6943306a8`.
Snapshot SHA-256:
`545ad6701f15b811c30a23e314cc17eb2684d88d2ed6455e81a8ff1eb285be05`.

The persistent-horizon real intro probe now crosses the wait and reaches
Alien phase 2, dealing 4 damage, where its control remains phase 1. Full
report/snapshot replay passes. These rooted inputs diagnose the generic
mechanism only; they remain excluded from the autonomous campaign.


## Overnight evidence, 00:15 UTC September 20

The persistent-horizon warm campaign added 500K jobs to the complete seed-2
v26 archive: 84,689,265 frames, 232,493 active entries, first defeat marker at
job 413797. It still has no acquired boss weapon. Independent film verification confirms a healthy Flash Man kill at raw
frame 13,576 (HP2, lives2). The witness tape SHA-256 is
`e38f75e6168071c48e2293121177cb1546187446479035f085d755dfb026bf41`.
A diagnostic neutral tail reaches the Time Stopper award about 720 frames later;
that tail is excluded from the qualifying archive.

V27 corrects a separate observation defect: partial encounter damage was
surviving player death and appearing in stage-selection contexts. Live combat
now requires nonzero health and a non-death player state; intro health filling
is still excluded from damage. Defeated phases remain separately authoritative.
The adapter format/key identities were advanced, so old v26 archives cannot
silently seed a v27 campaign. The preserved ending oracle still passes.

The v27 control, with the persistent horizon and unchanged selector, passed a
fresh 2K-job, two-worker full report/snapshot replay. Stream SHA-256:
`0d9eed3ce0895dfe63653e41953d535236b84ef5548a344735b5997ca3965009`.
A fresh seed-1 100K-job run follows with five workers and 12 GiB. A separately
versioned pooled-recency core experiment is being qualified against this same
adapter, rather than comparing across representation changes.

Long tapes previously materialized every RGB frame on disk. The CLI now streams
frames to a single-thread encoder, then muxes recorded PCM from a temporary
directory. An external-media regression exactly matches the old renderer's
frame metadata and PCM hash, checks the encoded frame count, and checks cleanup.
It passed twice; targeted Clippy and real qualification passed. This is artifact
handling, not action generation or hidden gameplay execution.

A retention diagnostic replayed 128 identical random six-action suffixes from
each member of three same-cell historical pairs (health 4/8, 4/8, and 8/12).
The lower-health state produced greater maximum live damage in 2, 9, and 2
suffixes; the higher-health state did so in 9, 9, and 26. Both members reached
the same best damage per pair. These observations show aliased futures, but do
not yet justify a general extra-holder policy or establish improved campaign
reachability. Inputs, root hashes, and outcomes are preserved in the local
`retention-pairs` bundle. None seed a qualifying campaign.


The following complete-archive warm start uses the same v26 persistent-horizon
binary, seed 2, two workers, and a further 500K-job budget. It independently
records the Flash weapon bit (`0x20`) by progress sample 4500, after 940,729
additional emulated frames. Its copied champion passed independent uninterrupted replay from power-on: 303
actions, 14,373 frames, weapon bit `0x20`, HP2, lives2. Complete tape SHA-256:
`6a38060323d6b5300be378ece5c6714bb29d9948c0f27d151aa48e61198f65aa`.
No hand-authored waiting input or diagnostic root was imported.

The long lab continuation comparison completed 900K additional jobs from each
arm's own 100K archive. Control: 156,022,615 frames, first encounter at 373120,
first defeat marker at 564502. Continuation arm: 154,407,487 frames, first
encounter at 877469, no defeat. Together with the short probes, this does not
support enabling the continuation mixture by default. The existing control
binary continues on the lab from its entire autonomous archive; no newer source
or binaries are uploaded.


The isolated recency arm (`6255df86` plus the streaming-media CLI) passed fresh
2K-job/two-worker and own-bundle warm-resume qualification. Stream SHA-256:
`82ec38cf557e82536f486667d5a23a3182476f755a0b415ed3dfd2132cf4fa48`.
Binary SHA-256:
`cf925a20e1a03d7259b911161037d6a0d8864445e5f9ee33bad3a3a6e4ca90fb`.
Its seed-1 100K-job, five-worker, 12-GiB comparison began at 00:26 UTC. The
final-cell novelty policy is unchanged; the bounded new multiplier applies to
equal-progress ties at the pooled depths. Persistent discovery age also survives
compaction and archive import. This remains an experiment until outcome data.
