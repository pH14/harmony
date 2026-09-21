<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 exploration checkpoint — September 21, 2026

Work is paused at the user's request. **The game is unfinished.** The strongest
independently replayed and film-reviewed continuous power-on lineage has Flash
Man and Crash Man's weapons. No third weapon, Wily clear, or ending is verified.
PR [#363](https://github.com/pH14/harmony/pull/363) remains a draft, stacked on
#362; no merge is requested.

## Verified achievements and boundaries

- A 1,119-action power-on tape defeats Flash and Crash, including an ordinary
  death and retry, acquires inventory mask `0xA0`, and continues into live Quick
  Man stage gameplay. Tape SHA-256:
  `4bbec2f824520173b97d63e3afeba03f2f55a7c0e38f0d2d9f0162335030d29f`.
- A later 3,015-action, 125,821-frame witness reaches Quick's room 8 with both
  weapons, 25 HP and one life. Independent film review finds a large enemy on a
  raised block ahead; decoded boss HP and phase are zero, so this is not a boss
  encounter claim. Tape SHA-256:
  `efcb9ebe00ad52ce10b05d10e35e689b1e709eeeaa7692f7d1cb737bbca88ef1`.
- A 1,297-action, 52,978-frame witness reaches Metal's room 2 with both weapons
  and 4 HP. It enters the stage at 28 HP, then takes three eight-point hits
  during traversal among hanging hazards. Low health is incurred on this route,
  rather than inherited from the preceding boss. Tape SHA-256:
  `283ab3b52da8d9df203403b492adae5567d0472296ff827edfa45979371276a8`.
- A separate Metal-only lineage has a verified weapon award. It is not combined
  with the Flash/Crash lineage to claim three weapons.

Qualifying campaigns use power-on roots or entire autonomous archives. Selected
fight roots are diagnostics only. Recorded route tails execute from the actual
recipient state; no endpoint concatenation, RAM edits, hidden award-settling
frames, curated controller banks, or assisted ending tape qualifies as success.
Film was reviewed by a separate agent.

## Improvements made

### Generic searcher

1. **Horizon growth and retirement now work together.** A geometric retry
   window lets barren states reach a longer ordinary-action trial before
   returning to the uniform fallback. Feedback survives imports and selector
   resets, and zero-extension trials terminate correctly. The prior archive
   had 328,752 entries but only ten first-weapon states; retirement after three
   to six failed expansions starved the award transition. The corrected run
   produced a verified post-award traversal witness by 3,129,778 frames, below
   the control's 4,331,966-frame total, where the control had no healthy
   post-award gameplay candidate. See `68f427b1c` and `504b9b139`.
2. **Complete-archive restoration uses exact recorded input prefixes.** Shared
   snapshots reduce redundant bootstrap execution without selecting helpful
   roots. In a paired small archive, bootstrap work fell from 373,918 to 145,413
   frames while all subsequent 1,000 jobs, archive bytes and checkpoint bytes
   matched. Prefix ancestry also preserves observed routes across missing
   intermediate metadata. See `29375770e` and `d37c1fb83`.
3. **Observed-route reuse is explicit and replayable.** Compatible contexts can
   propose recorded exits across different inventory identities; retention
   identity remains separate, and actual execution determines acceptance.
   Donor liveness, compaction, accounting and exact attempt-cache bounds have
   tests. These are infrastructure improvements, not an established general
   completion speedup.
4. **Bounded progress follow-up remains experimental.** Half of draws can
   follow recent progress through a queue limited to 128 entries and 128
   attempts per entry. Four rooted Crash probes won 2/4 versus 0/4 controls.
   A full autonomous Flash lineage subsequently acquired Crash, but the
   corresponding earlier full-archive control was canceled before dispatch:
   this does not isolate an end-to-end causal benefit. Other lineages and fresh
   seed tests did not reproduce comparable gains. See `1a61c5310`.
5. **Discovery follow-up was tested at two depths.** The first trigger was
   inert in the matched campaign. The deeper trigger changed real behavior,
   but still found no third weapon at its frame limit. Neither is a default.
   See `9e88c382a` and `e4e0d9ce4`.
6. **Untried route alternatives are checkpointed, not yet campaign-tested.**
   Commit `50d5aa3bf` changes the opt-in deduplicated route policy to look past
   cached repetitions for another compatible observed tail. It uses the
   existing bounded exact-attempt cache and avoids a fixed shortlist that
   could hide older routes. No adapter code or default policy changes.

### Adapter and runner

Whole-game execution now exposes deaths, retries, awards, stage choices and the
ending through recorded inputs. Inventory identity, uniform resource bytes,
actionable encounter state and lifecycle observations replace misleading
aggregations or stale values. A simultaneous player death and boss defeat can
still produce an award, so dead states cannot simply be discarded globally.

Menu decoding no longer treats sprite scratch as a universal mode flag. Valid
register ranges and stage-selection portrait layout constrain observations,
but reused RAM still requires trace/film validation. These are mechanical
observations; generic selection, retention, horizons, mutation and route reuse
remain in the searcher.

Runner commits `78c83954a` and `4e1a9a160` release raw archive bytes before
checkpoint decoding and imported source ownership before final serialization.
Paired outputs remain identical. Full-size before/after peak-memory improvement
has not been established. Optional stage-order guidance in `ab1d7b73a` is
explicit supplied game knowledge, defaults off, and is not presented as a
generic search improvement.

## Matched experiments and negative results

All four runs below import the same entire archive, use seed 2, three workers,
a 16,000 MiB search budget, and a nominal 35M-frame limit. Each spends
10,072,079 frames on import; the table reports new exploration separately.

| Arm | Jobs | New exploration frames | Outcome |
|---|---:|---:|---|
| Unrestricted control | 31,880 | 24,935,279 | Two weapons; no ending |
| Explicit stage order | 34,477 | 24,928,685 | Two weapons; no ending |
| Two-coarsest-depth discovery | 31,880 | 24,935,279 | All behavior-bearing job fields match control |
| Deeper spatial discovery | 36,030 | 24,932,446 | Two weapons; no ending |

All four archive exports have completed manifest and compression verification.
The spatial archive and checkpoint finished verification at 13:13 UTC, after
search had stopped. Spatial discovery first differs from control at job 113.
Its final runner summary is still pending at this checkpoint; its detailed
census/allocation audit was canceled on pause.
More jobs or different selections alone are not evidence of better exploration.

Earlier exact-tail suppression removed 205 repeat attempts without increasing
the ten strict useful arrivals in its paired probe. Traversal and health results
were mixed. A later guided run spent 1,805,180 frames, 7.24% of new exploration,
on 1,779 exact repeats with identical result hashes and no retention. This is
measured waste, but it does not explain the entire stall.

## Diagnoses and remaining uncertainty

- **Composition is the current practical wall.** Existing capability states
  have useful local routes, yet the two-weapon lineage has not reached another
  unfinished boss. A one-weapon lineage reaches farther into Quick and Metal
  than the two-weapon lineage. Route transfer, usable resources at obstacles,
  and enough attempts from those states deserve direct measurement.
- **A discovery trigger can be completely saturated.** The coarse experiment
  matched all 31,880 control jobs because no new groups opened at its watched
  depths. Moving deeper activates the mechanism; it does not by itself prove
  useful progress. This is why telemetry counts need behavioral comparisons.
- **Allocation diagnoses must survive a control.** Guided search spent 4.89M
  identifiable frames on zero-weapon Crash versus 0.63M on two-weapon Metal.
  Unrestricted search spent only 0.065M and 1.79M respectively, still without a
  third weapon. Thus the guided concentration does not explain unrestricted
  failure. Missing parent attribution was 22.9% and 19.6%; neither audit is a
  complete accounting by location.
- **The prior deduplication fallback hid alternatives.** Lookup returned the
  highest-ranked compatible route, then rejected a duplicate and drew random
  input. Another known route was never considered in that dispatch. The new
  experimental core change addresses this specific limitation. Game-level
  benefit is unmeasured; compatibility does not guarantee successful transfer.
- **Memory and import cost limit experiment throughput.** Large imports take
  many minutes in JSON decoding, route-index propagation and restoration.
  Final reports expand shared input prefixes into full tapes, with observed
  footprints around 46–49 GiB in one export phase. Earlier runner ownership
  compounded this. The ownership fix and generic representation cost are
  separate issues; see #364 and #365. Avoid concurrent large imports.
- **Measurements can mislead.** Final archives include historical ancestors;
  a census is not a live-holder count. Stage/room maxima can be transition
  scratch. Current resources are not historical maxima. A boss-clear event is
  not necessarily a runnable post-award state. Imported follow-up tickets are
  reconstructed rather than preserving spent budgets.

## Validation and resume state

The route-alternative checkpoint passes 161 searcher unit tests, 19 integration
tests, all-target Clippy with warnings denied, release compilation, formatting,
comment checks and repository custom lints. Core integration tests exercise
route alternatives and concurrent cold/warm replay. Two 1,000-job real-emulator
warm qualifications replay exactly; the unchanged policy also matches the
previous archive, checkpoint and every job. **Both small real-game fixtures
had zero route tails**, so they validate fallback and equivalence, not real-game
alternative-route execution. The frozen binary SHA-256 is
`86f55aab403decbe426f5d58ff3d2333c27e3c8466c4bcc2062102818c160d45`.

The previously pushed head `3219d7e7b` passed all applicable CI, with exact guest
platform qualification skipped. Final checkpoint push/check results are recorded
in the task handoff rather than assumed here.

No new search should start until the user resumes. The prepared next route arm
has not launched. If resumed, first verify the completed spatial export and
audit its useful frontiers; then evaluate route alternatives from an entire
archive at comparable work. Fund a sustained continuation from measured useful
progress. Do not use the selected film witnesses as search roots.

Private evidence, frozen binaries, manifests, traces and detailed process state
are preserved in `/private/tmp/mm2-autonomous-20260919/`. Start with `PAUSED.json`,
`MM2-HANDOFF.md`, `v31-route-qualifications.json`, the matched-comparison JSON
files, and the `metal-entry-extract` / `quick-entry-extract` film-review records
under the completed control run. Private game assets are not checked in.
