# SMB completion lab log

## 2026-08-10 — setup

- The original checkout was rejected for experiment use because its HEAD was `6bf71bb64408bb1ed18b4e2670a0fb9bf0139111`, not the pinned base.
- Created dedicated worktree `/Users/phemberger/workspace/harmony-smb-completion` and branch `codex/smb-completion` directly from `8f2b522c26c6f192f2db45a430bec03ed447cad7`.
- Verified the worktree was clean and recorded `BASE_COMMIT=8f2b522c26c6f192f2db45a430bec03ed447cad7`.
- Read `AGENTS.md`, `dissonance-v2/CLAUDE.md`, all three governing Dissonance documents, and `dissonance-v2/NOTES.md` as present at the base commit.
- Preregistered the frozen-baseline reproduction, seed split, acceptance rule, and H1 before running a campaign.

## Baseline reproduction

- External ROM SHA-256 matched the required `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- The frozen six-seed M5 runner completed at 500 executions per seed. Because the checked-in base includes M10's 16-pixel metric, the raw ratchet maxima were `[37, 45, 20, 29, 36, 37]` and the random-walk maxima were `[12, 10, 10, 19, 19, 11]`; the ratchet upper median was 37 versus 12. The preregistered historical M5 values use the pre-M10 64-pixel observer and cannot be emitted by the checked-in M12 code. This is a metric-granularity deviation, not a search rerun failure.
- The best autonomous source corpus was seed `0x5eed_d701`: 25 retained inputs and max-x 45.
- Reconstructed the frozen M12 restart control from that corpus at seed `0x5eed_dc00` for 500 executions. It retained 50 inputs and reproduced the recorded M12 frontier max-x 62 without a detector, generated mutator, model process, supplied input, or intermediate state.
- A second complete campaign was structurally identical to the first. Replaying the starting champion twice produced an identical complete observation trace.
- Starting champion input SHA-256: `f5ea861ef624b4033684a13e077e6faa66ebfdef0681d276153424bb63413aee`.
- Starting champion observation-trace SHA-256: `d00f3b0760b1cf58fdb86827b25faa59e50b2010695195da2583ff11080e3095`.
- Raw evidence: `dissonance-v2/target/smb-completion/baseline-m5/`, `dissonance-v2/target/smb-completion/baseline-champion-film/`, and `dissonance-v2/target/smb-completion/m12-reproduction/`.

## Hypothesis ledger

| ID | Status | Falsifiable claim | Smallest test | Development result | Held-out result |
|---|---|---|---|---|---|
| H1 | accepted | Global novelty discards reusable local frontier states, causing repeated long-prefix reconstruction. | Bounded deterministic snapshot-backed quality-diversity archive plus short seeded suffix search. | Controls `[104, 88, 108, 103, 105, 105]`; challengers `[146, 147, 173, 86, 152, 82]`; 4/6 paired wins; upper median 105→147. | Controls `[105, 115, 88, 97, 97, 104]`; challengers `[146, 154, 82, 147, 177, 140]`; 5/6 paired wins; upper median 104→147. |

## H1 — accepted snapshot archive

- Instrumentation smoke: seed `0x5eed_e000`, 100 executions, 342 retained entries, four death transitions, and an exact full-report replay.
- Development challenger checkpoint-curve areas (100-execution samples): `[6116, 6047, 6350, 3961, 5505, 4080]`.
- Development final-maximum arrival executions: `[3300, 4100, 5000, 2100, 4900, 300]`. Low seeds and flat curves were retained without filtering or extra budget.
- Held-out champion: seed `0x5eed_e104`, 5,000 executions, max-x 177, 5,930 lineage entries, 312 death transitions, input SHA-256 `92c879eea15988b8818f5a1d5b02a834a6761829506261ffe2acecfbf7af1b83`, observation SHA-256 `3c61407dab5eb6cff52bb5438617adf704a5e132c744eb5bf89d9450fc5f63f3`.
- The complete held-out champion campaign reproduced exactly in an independent no-model rerun (`replay_verified=true`).
- Its film ends on the final 1-1 staircase. Its curve rose from max-x 170 at execution 4,400 to 174 at 4,500 and 177 at 4,800, so scaling this accepted search is permitted before adding machinery.
- Raw evidence: `dissonance-v2/target/smb-completion/h1-dev/`, `dissonance-v2/target/smb-completion/h1-held/`, and `dissonance-v2/target/smb-completion/h1-promotion-replay-e104/`.

### Rejected cache calibration C1

Hypothesis: a larger frozen prefix cache plus per-action prefix snapshots would materially reduce control wall time without changing target-execution results. Two 5,000-execution e000 calibration runs were preserved under `target/smb-completion/cache-equivalence/`. Neither materially outpaced the completed frozen control because state-save overhead offset replay savings. Both were stopped, and the cache changes were removed. Do not repeat C1 without a different measured cache design.

## Accepted-H1 scale checkpoint S1 — preregistered

- Rationale: the held-out H1 champion's max-x curve was still rising at 4,800 of 5,000 executions, so the frozen compute rule permits scaling the accepted mechanism without a new search change.
- Fixed configuration: unchanged H1 archive, original autonomous M12 initial corpus, seed `0x5eed_e104`, 20,000 target executions, no model process, no replay during the live run.
- Decision rule: continue unchanged only if the curve crosses the 1-1 flag/level milestone or its final 5,000-execution window improves max-x. Otherwise declare a plateau, preserve and inspect the archive/lineage/deaths/film, and formulate exactly one H2 before further compute.
- Replay rule: an independently repeated full campaign is required only if S1 produces a new promoted champion or level milestone.
- Raw destination: `dissonance-v2/target/smb-completion/h1-scale-e104-20000/`.

### S1 result — promoted milestone

- The unchanged archive reached the 1-1 flag at target execution 5,758 and finished at max-x bucket 195. It retained 11,378 lineage entries, rejected 13,239 candidates, observed 478 deaths, and did not reach 1-2.
- An independent full 20,000-execution repeat produced a byte-identical archive report. Both report files have SHA-256 `359db709fd50ec0e3eefb980383d2d2c33810d787281aa9b66eaa0fe945c0136`. Champion input SHA-256 is `c6038d8984efed41e5f5cf5dcf8695ae327c5e7fb8295787df6b6873a30cb481`; observation SHA-256 is `68728a461e5c905534270b405662710dc7236710661861c2d5f98cca05a8307c`.
- The final 5,000-execution window was flat at bucket 195. The first flag input used 91 of the 96 permitted chords; 719 retained entries used at least 90, 31 reached the cap, and every chord-count band from 90 through 96 topped out at the same flag state. Blind scaling stops here.
- Raw live/replay evidence: `target/smb-completion/h1-scale-e104-20000/` and `target/smb-completion/h1-scale-e104-20000-replay/`.

### Required model evidence and generated-mutator validation — preregistered

- Luna low triage sees only the bounded flag-frontier evidence and supplies scheduler labels. Luna xhigh instrumentation sees the same operator view plus the generic `SmbMacro` interface; it may not emit a route, trajectory, supplied input, or state-specific rule.
- Every attempt is immutable. Generated source must compile exactly as emitted, then pass paired pure-mutator fixtures on development seeds `0x5eed_e000..=0x5eed_e005` and held-out seeds `0x5eed_e100..=0x5eed_e105`: same input/seed equality, unchanged original prefix, bounded action count and durations, and no panic on empty/at-cap inputs.
- A generated mutator is not retained unless it additionally changes the measured horizon bottleneck; merely filling unused slots up to the existing 96-chord ceiling is rejected before campaign integration because H1 already appends generic chords and the terminal cap remains identical.
- Raw operator view, transcripts, attempted sources, compiler/fixture records: `target/smb-completion/h2-model-evidence/`.

### Model evidence result

- Luna low returned `Boost` for retained testcase 6,816, tagged it `flag-reached`, `nonterminal`, and `progress-plateau`, and highlighted the five remaining chords.
- Luna xhigh attempt 1 emitted a generic seeded tail-extension mutator but used nonexistent `SmbInput.chords`; exact isolated compilation failed with five `E0609` errors. The source and failure were preserved before retry.
- Attempt 2 corrected the field to `actions`, compiled, and passed deterministic/bounds/prefix fixtures on all 12 preregistered seeds. It was rejected before campaign integration: it only fills unused slots up to the unchanged 96-chord cap and therefore cannot change the measured horizon; H1 already supplies generic append mutations. No model output was installed.

## H2 — preregistered completion horizon

- Falsifiable claim: after the flag, search is blocked by the generic 96-chord representation ceiling rather than archive saturation or lack of a retained flag state. A larger bounded completion-only ceiling, continuing from the autonomously produced clean-reset flag input, will reach 1-2.
- Smallest test: parameterize only Phase 4c's expansion limit while preserving Phase 4b's frozen 96-action surface and H1 replay behavior. Add an archive-source CLI mode that extracts the prior autonomous champion input and reconstructs all snapshots from clean gameplay genesis; no saved emulator state is injected. Challenger limit is 512 chords; control remains 96. Mutation vocabulary, RNG, archive keys, selection, cell bounds, and suffix distribution are unchanged.
- Development and held-out seeds remain `0x5eed_e000..=0x5eed_e005` and `0x5eed_e100..=0x5eed_e105`. Paired budget is 5,000 target executions from the exact S1 archive report in both arms.
- Acceptance: challenger reaches 1-2 on at least four of six development seeds with no flag regression; repeat unchanged on held-out seeds and require the same threshold. If it fails, inspect continuation-state curves and films before any further change. Any promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h2-dev/` and, only after development acceptance, `target/smb-completion/h2-held/`.

### H2 result — rejected

- Controls reached 1-2 on 0/6 development seeds and all retained the unchanged S1 champion. Challengers reached 1-2 on only 1/6 (`0x5eed_e004`), below the 4/6 threshold, so no held-out run was performed.
- The five failing challengers did expand beyond the former ceiling (maximum lengths `[98, 99, 99, 102, 99]`) but stayed at the flag. The lone success reached 1-2 at execution 1,975 with only 95 chords, so the original 96-chord cap was not the causal blocker.
- The successful clean-reset input appended four chords to the S1 champion, all with holds in 105..=115 frames and totaling 436 frames. Its film visibly reaches the `WORLD 1-2` transition screen. The legacy sampler selects a hold above 100 frames on only 20/120 of its one-quarter long-sampling branch, about 4.17% per chord.
- H2's parameterization and resume path remain useful experiment plumbing but the 512 ceiling is not promoted as a search improvement. Raw evidence: `target/smb-completion/h2-dev/`; film: `target/smb-completion/h2-dev/challenger-e004/level-1-2-film/`.

## H3 — preregistered temporal-horizon coverage

- Falsifiable claim: continuation past mechanically nonterminal animations is rare because the legacy duration sampler overwhelmingly emits 2..=12-frame holds and gives only about 4.17% probability to holds above 100 frames. Stratified generic duration sampling will make the successful multi-hundred-frame continuation reproducible across seeds without any state-specific trigger.
- Smallest test: preserve the H2 512-action bound, button-mask vocabulary, archive keys, parent selection, suffix length, and all milestone logic. Change only duration sampling in a challenger policy: half of chords uniformly sample 2..=12 frames and half uniformly sample 96..=120 frames. The policy is global and cannot inspect world, level, progress, engine state, coordinates, or prior actions.
- Controls are the completed H2 development challengers. Challenger development seeds remain `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, starting from the exact S1 clean-reset champion. Acceptance is 1-2 on at least 4/6 with no flag regression. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h3-dev/` and, only after development acceptance, `target/smb-completion/h3-held/`.

### H3 result — accepted

- Development reached 1-2 on 6/6 seeds. First-hit executions were `[324, 338, 527, 248, 448, 279]`, compared with H2's 1/6 and frozen-cap control's 0/6. All six retained the flag milestone.
- Held-out reached 1-2 on 6/6 seeds. First-hit executions were `[413, 443, 273, 456, 578, 299]`. The unchanged policy therefore exceeded the 4/6 threshold on both panels.
- Promoted campaign: held-out seed `0x5eed_e102`, first 1-2 at execution 273, 5,000 executions total, 4,202 lineage entries, 881 deaths. Champion input SHA-256 `c866fc96ad7aa3ab4a2711cb52ee54a66ff961fa410d0d12f69cbdb23c446af3`; observation SHA-256 `70f9c3952c06f92da6d4e2f05373a263963c55687ff681eb7e03c1ed07e67310`.
- An independent complete no-model campaign repeat was byte-identical; both archive reports have SHA-256 `6b4e401946ba8bf49911a5f7fba7e2ba22bb30d53ee01e7730f47e1a64b5adc1`.
- Cost: the 50% long-hold policy materially increases emulator frames and terminal deaths per fixed target execution. Later efficiency work must preserve the 12/12 milestone result rather than weakening the accepted temporal coverage.
- Raw evidence: `target/smb-completion/h3-dev/`, `target/smb-completion/h3-held/`, and `target/smb-completion/h3-promotion-replay-e102/`.

## Accepted-H3 scale checkpoint S2 — preregistered

- Rationale: the promoted H3 report contains 3,191 retained 1-2 entries. Its mechanically furthest entry reached 1-2 progress bucket 76 at execution 4,997 with 143 chords, so the within-level frontier was still rising at budget. The old milestone-only `champion_input` remains the first 1-2 transition at 98 chords and is not a faithful resume point.
- Source selection is experiment plumbing, not a route rule: choose the retained, nonterminal archive entry with the greatest lexicographic `(world, level, progress)` mechanical tuple, then prefer the shorter clean-reset input and earlier archive id on exact ties. Reconstruct every snapshot by replaying that input from gameplay genesis; do not load a saved emulator snapshot.
- Fixed configuration: promoted H3 temporal policy, held-out campaign source `h3-held/challenger-e102/archive-live.json`, seed `0x5eed_e102`, 20,000 target executions, 512-chord bound, no model process, no replay during the live run.
- Decision rule: continue unchanged only if S2 reaches `reached_onward` (1-3 or later), or if its furthest `(world, level, progress)` tuple improves during the final 5,000 executions. Otherwise stop and diagnose the plateau before adding machinery.
- Replay rule: independently repeat the complete campaign if S2 reaches a new level milestone or becomes the next promoted frontier.
- Raw destination: `target/smb-completion/h3-scale-e102-20000/`.

### S2 result — plateau

- S2 completed all 20,000 executions without reaching onward. It retained 7,158 entries, rejected 17,033 candidates, observed 367 terminal deaths, and did not approach the 32,768-entry archive bound.
- The furthest tuple remained exactly `(world 0, level 1, progress 76)` at every checkpoint from execution 15,000 through 20,000; the selected entry was a clean-reset bootstrap prefix, so no further scaling is permitted.
- The mechanically furthest film ends at a live 1-2 platform edge. The accepted temporal policy executes 96..=120-frame chords atomically for archive purposes: intermediate frames are observed, but only the final emulator state can be snapshotted and retained. A chord that passes through a useful live state and later dies contributes a death but no reusable intermediate snapshot.
- Raw evidence and film: `target/smb-completion/h3-scale-e102-20000/`.

## H4 — preregistered intermediate snapshot retention

- Falsifiable claim: endpoint-only retention censors useful intermediate states inside long chords, especially when the chord later terminates in death. Retaining deterministic action-boundary snapshots every 12 frames along the identical controller stream will make progress beyond the S2 plateau reproducible across seeds.
- Smallest test: preserve the accepted 50/50 short/long duration distribution, button masks, parent selection, archive keys, suffix count, 512-action bound, and target-execution accounting. In the challenger only, split each sampled chord into adjacent same-button chunks of at most 12 frames whose durations sum exactly to the sampled duration; snapshot and apply normal bounded archive retention after each live chunk. The controller value on every emulated frame is unchanged. No state field controls chunking.
- Source for both arms: the mechanically furthest clean-reset entry in `h3-scale-e102-20000/archive-live.json`. Development seeds `0x5eed_e000..=0x5eed_e005`, held-out seeds `0x5eed_e100..=0x5eed_e105`, 5,000 target executions, 512-action bound.
- Acceptance: challenger reaches onward on at least 4/6 development seeds with no 1-2 regression, then repeats unchanged on held-out seeds and meets the same threshold. It must not exhaust the archive before the milestone, and a promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h4-dev/` and, only after development acceptance, `target/smb-completion/h4-held/`.

### H4 result — rejected

- Endpoint-only controls reached onward on 0/6 development seeds. Their retained-entry counts were `[3674, 4515, 4445, 4678, 3468, 3696]`.
- Checkpointed challengers also reached onward on 0/6. Their retained-entry counts were `[8120, 11601, 8662, 8857, 7362, 8322]`; every run preserved 1-2, and none approached the 32,768-entry archive bound.
- Every challenger remained at exactly `(world 0, level 1, progress 76)`. More intermediate snapshots therefore did not make the S2 plateau traversable.
- The source archive contains 3,975 entries at progress 76, spanning every vertical bucket, but the resume plumbing selected only the shortest one. Its clean-reset film ends at the same gap as S2. H4 multiplied descendants of that single bootstrap state rather than testing the retained frontier diversity.
- No held-out run was performed. The checkpoint-retention mechanism is rejected and will be removed before the next challenger. Raw evidence and diagnosis film: `target/smb-completion/h4-dev/`.

## H5 — preregistered frontier-diversity resume

- Falsifiable claim: continuation is blocked because clean-reset resume collapses a mechanically diverse frontier to one shortest bootstrap input. Seeding the unchanged accepted H3 search from a bounded set of mechanically distinct entries at the best `(world, level, progress)` tuple will reach onward reproducibly.
- Smallest test: remove H4 checkpoint retention and restore endpoint-only H3 execution. From the exact S2 report, find the greatest `(world, level, progress)` tuple, then retain the shortest/earliest complete input for each distinct mechanical `(player_y_bucket, player_engine_state)` pair, bounded at 64 inputs. Replay every seed input from gameplay genesis to reconstruct snapshots. The grouping and bound are fixed before execution and do not inspect route semantics, buttons, images, or outcomes.
- Control is the completed H4 endpoint-only panel, which used the same source report and accepted H3 policy but one shortest frontier input. Challenger seeds are `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, 512-action bound. Acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion; if accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold.
- A promoted campaign must replay exactly without a model. Raw destination: `target/smb-completion/h5-dev/` and, only after development acceptance, `target/smb-completion/h5-held/`.

### H5 instrumentation calibration

- An unscored 128-input smoke was cancelled before completing bootstrap or writing a campaign report: reconstructing and indexing every long overlapping prefix was too expensive. The preregistration was narrowed before any scored execution to one shortest input per already-recorded coarse mechanical `(player_y_bucket, player_engine_state)` pair (18 source pairs, hard bound 64). The experiment claim and source frontier are unchanged.
- The calibrated 10-execution smoke reconstructed all 18 inputs, retained 173 entries, observed two deaths, and reproduced a byte-identical complete report in an independent rerun.

### H5 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[3085, 3124, 3094, 3267, 3188, 3105]`; deaths were `[84, 63, 64, 203, 73, 81]`. No held-out panel was run.
- Every run remained at `(world 0, level 1, progress 76)`. Frontier diversity at bootstrap was therefore not sufficient.
- The scheduler's nominal 128-entry frontier is sorted by the entire archive key after milestones. At equal world/level/progress, larger `player_y_bucket`, `player_engine_state`, and random fingerprint rank later. In H5 seed e000, all 128 entries in that exact scheduler window had `player_y_bucket=15` and `player_engine_state=8`. The generic progress scheduler had collapsed into a deterministic offscreen-state tie-break.
- Raw evidence: `target/smb-completion/h5-dev/`.

## H6 — preregistered progress-equivalent frontier scheduling

- Falsifiable claim: search cannot cross the 1-2 gap because the nominal frontier scheduler ranks route-agnostic diversity fields as quality and consequently schedules only the largest vertical/state/fingerprint tie-breaks. Treating all active entries with the strongest `(milestones, world, level, progress)` tuple as progress-equivalent will reach onward reproducibly.
- Smallest test: keep H5's exact 18-input source set, accepted stratified duration policy, endpoint snapshots, archive keys, retention, suffix count, 512-action bound, and 5,000-execution budget. Change only `choose_parent`: on its existing 3/4 frontier branch, find the maximum `(milestone_key, world, level, progress)` tuple and uniformly select from every active entry with that tuple. Vertical bucket, engine state, fingerprint, input length, and insertion id remain diversity/retention fields but are not quality tie-breaks.
- Controls are the completed H5 development reports. Challenger seeds are `0x5eed_e000..=0x5eed_e005`; acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h6-dev/` and, only after development acceptance, `target/smb-completion/h6-held/`.

### H6 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[3283, 3239, 3147, 3229, 3213, 3175]`; deaths were `[41, 63, 52, 45, 54, 59]`. No held-out panel was run.
- Every run remained at `(world 0, level 1, progress 76)`. Removing the vertical/state/fingerprint quality tie-break substantially changed scheduling and reduced some death counts, but equal-progress diversity alone did not cross the gap.
- The H6 exact-replay smoke retained 175 entries with zero deaths and reproduced its full report. Raw evidence: `target/smb-completion/h6-smoke/` and `target/smb-completion/h6-dev/`.

## H7 — preregistered progress-band scheduling

- Falsifiable claim: strict maximum-progress scheduling creates a deceptive local optimum at the 1-2 gap by preferentially expanding states at or past the edge, while successful traversal requires mutations beginning from an earlier run-up state. A bounded near-frontier progress band will reach onward reproducibly.
- Smallest test: keep H6's exact source set, stratified durations, endpoint snapshots, archive/retention keys, suffix count, 512-action bound, and 5,000-execution budget. On the existing 3/4 frontier branch, select uniformly from active entries that match the strongest `(milestone_key, world, level)` and lie within eight 16-pixel buckets of its maximum progress. The inclusive eight-bucket band is fixed before execution and applies identically to every world/level without inspecting route, images, buttons, enemies, or coordinates beyond the existing mechanical progress bucket.
- Controls are the completed H6 development reports. Challenger seeds are `0x5eed_e000..=0x5eed_e005`; acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h7-dev/` and, only after development acceptance, `target/smb-completion/h7-held/`.

### H7 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[3292, 3452, 3373, 3429, 3452, 3277]`; deaths were `[63, 32, 51, 44, 37, 45]`. No held-out panel was run.
- Every run remained at `(world 0, level 1, progress 76)`. A near-frontier run-up band is therefore insufficient with the existing controller representation.
- The H7 exact-replay smoke retained 175 entries with zero deaths and reproduced its full report. Raw evidence: `target/smb-completion/h7-smoke/` and `target/smb-completion/h7-dev/`.
- Exact source-film diagnosis: the archive vocabulary contains right (`0x01`), B (`0x40`), A (`0x80`), and A+right (`0x81`), but not B+right (`0x41`) or A+B+right (`0xc1`). It cannot express a running jump as one held controller chord. The existing `0x83` chord is A plus both left and right, not A+B+right.

## H8 — preregistered controller-chord closure

- Falsifiable claim: the 1-2 gap is untraversable because the mutation vocabulary cannot hold run and jump with a horizontal direction simultaneously. Closing the existing horizontal × A/B chord product will reach onward reproducibly.
- Smallest test: keep H7's source set, progress-band scheduler, duration policy, snapshots, archive/retention, suffix count, 512-action bound, and 5,000-execution budget. Add exactly the five missing symmetric A/B combinations to the existing vocabulary: B+right (`0x41`), B+left (`0x42`), A+B (`0xc0`), A+B+right (`0xc1`), and A+B+left (`0xc2`). Existing masks remain unchanged. The completion-only vocabulary is global, state-blind, and contains no route trigger.
- Controls are the completed H7 development reports. Challenger seeds are `0x5eed_e000..=0x5eed_e005`; acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h8-dev/` and, only after development acceptance, `target/smb-completion/h8-held/`.

### H8 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[3298, 3419, 3297, 3456, 3390, 3278]`; deaths were `[49, 47, 46, 52, 47, 44]`. No held-out panel was run.
- The new vocabulary was not dormant: seed e000 retained 1,376 near-frontier entries whose final two chords included a new combination, including 335 with A+B+right (`0xc1`). None exceeded progress 76.
- A jump requires an A-release edge before a later A press; useful traversal can therefore require release, run-up, and jump chords. Phase 4c emits at most two chords per execution, while its shortest-input cell replacement rejects longer intermediate inputs that return to the same mechanical cell. The required temporal composition cannot reliably accumulate one neutral step at a time.
- Raw evidence: `target/smb-completion/h8-dev/`.

## H9 — preregistered bounded temporal bursts

- Falsifiable claim: the plateau persists because the two-chord suffix horizon cannot compose a release/run/jump sequence across archive cells that prefer shorter equivalent inputs. A bounded state-blind suffix burst of up to four chords will reach onward reproducibly.
- Smallest test: preserve H8's source set, progress-band scheduler, closed controller vocabulary, stratified duration policy, snapshots, archive/retention, 512-action bound, and 5,000-execution budget. Add an explicit completion suffix policy. Legacy remains 75% one chord and 25% two; the challenger samples one, two, three, or four chords with probabilities 1/2, 1/4, 1/8, and 1/8. The count is sampled once from the seeded RNG before executing the suffix and cannot inspect state or outcomes.
- Controls are the completed H8 development reports. Challenger seeds are `0x5eed_e000..=0x5eed_e005`; acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h9-dev/` and, only after development acceptance, `target/smb-completion/h9-held/`.

### H9 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[4384, 4446, 4379, 4519, 4559, 4395]`; deaths were `[156, 149, 146, 146, 144, 156]`. No held-out panel was run.
- The burst materially increased retained/rejected candidates and terminal deaths but left every run at progress 76. Temporal suffix depth from the same ancestry is therefore insufficient.
- Raw evidence: `target/smb-completion/h9-dev/`.

## H10 — preregistered approach-band ancestry

- Falsifiable claim: H5–H9 share a collapsed ancestry because they reconstruct only max-progress source inputs; mutations cannot revise the earlier approach trajectory when shortest-input cell retention discards longer equivalent detours. Seeding from distinct source trajectories across the near-frontier progress band will reach onward reproducibly.
- Smallest test: keep H9's progress-band scheduler, closed controller vocabulary, stratified durations, burst suffix, endpoint snapshots, archive/retention, 512-action bound, and 5,000-execution budget. In the exact S2 source report, select entries matching the best `(world, level)` and lying within the fixed eight-bucket progress band. For each populated progress bucket, take the eight shortest distinct complete inputs by `(action count, archive id)`, with a hard total bound of 64, then reconstruct all snapshots from clean gameplay genesis. S2 has five populated endpoint buckets in this band (`71, 73, 74, 75, 76`).
- Controls are the completed H9 development reports. Challenger seeds are `0x5eed_e000..=0x5eed_e005`; acceptance is onward on at least 4/6 with no 1-2 regression and no archive exhaustion. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted campaign must replay exactly without a model.
- Raw destination: `target/smb-completion/h10-dev/` and, only after development acceptance, `target/smb-completion/h10-held/`.

### H10 result — rejected

- All six development seeds preserved 1-2 but reached onward on 0/6. Retained-entry counts were `[6788, 5542, 6238, 5265, 4793, 4884]`; deaths were `[71, 136, 131, 143, 142, 113]`. No held-out panel was run.
- The first reading that all runs remained at 1-2 progress 76 was wrong: it confused the S2 source boundary with each H10 report's 1-2 frontier. Directly decoding the recorded entry keys gives per-seed maxima `[115, 115, 122, 114, 83, 83]`. Broader clean-reset ancestry therefore did advance the within-level frontier, but it did not reach onward, so the registered H10 rejection and no-promotion/no-replay decision are unchanged.
- Raw evidence: `target/smb-completion/h10-dev/`.

## Integrator steer after H10

- Before any further plateau-motivated search redesign, validate the progress-76 decoded fields against raw WRAM and independent film evidence. From now on, milestone crossings record raw WRAM next to decoded observations.
- Implement next-free milestone M13: generated rankings delivered through the existing instrumentor decision schema, validated on recorded observations, installed/recorded for exact no-model replay, and consulted only for replacement inside a full archive cell. Fewer actions remains the final tie-breaker; rankings cannot use progress measures.
- Add execution-count retirement for ineffective rankings and a fixed-window detector retained-execution rate ceiling if the M12 validation path does not already provide one.
- Ranking precedes another SMB search hypothesis: the repeated plateau with normal archive growth is consistent with losing a within-cell state attribute needed later, which is exactly the bounded mechanism rankings address.

### Plateau decode diagnosis — pass

- The film manifest now records the complete 2 KiB raw WRAM beside the independently decoded mechanical state and milestone flags at every observer event, plus action-boundary records. Bucket transitions, first death, and action endpoints are therefore captured with a byte-for-field audit trail, including milestone rung crossings.
- The S2/H4–H9 boundary was replayed from an exact recorded `(world 0, level 1, progress 76)` entry. Its raw scroll bytes were page `4` and x `204`, independently yielding `4 * 16 + floor(204 / 16) = 76`. The corresponding film frame visibly places Mario at the 1-2 gap. Result: **pass**; progress 76 was not a decode fault.
- H10 seed `0x5eed_e002` was separately replayed at its actual frontier `(world 0, level 1, progress 122)`. Raw page `7` and x `161` independently yield `7 * 16 + floor(161 / 16) = 122`; the film visibly shows late 1-2 terrain. Result: **pass**. The differing H10 maxima are genuine search outcomes, not one field being scored as regression.
- Diagnostic manifests and PNG strips are ignored raw evidence under `target/smb-completion/plateau-diagnosis/`; the mechanism that emits raw-plus-decoded boundaries is tracked in `smb-film`.

## M13 — preregistered generated ranking

- Mechanism claim: a model-generated deterministic score over one state's recorded observations can preserve a within-cell representative better prepared for descendant novelty without changing novelty, keys, scheduling, or parent choice.
- Source evidence is H10 development seed `0x5eed_e002`, the strongest actual H10 1-2 frontier at progress 122, its raw-WRAM film manifest/video, and eight insertion-order-spaced recorded state traces. Luna xhigh receives only that operator view and the neutral ranking contract. It gets at most three compile/fixture attempts under the existing single-decision schema.
- Validation is fixed at seed `0x5eed_ef00` for 256 target executions. It requires deterministic score output on every recorded observation fixture, isolation to the replacement arm, and an exact full archive-report replay with the model absent.
- The paired development panel uses seeds `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, a 512-action bound, the accepted H3 parent scheduler/controller vocabulary/stratified durations/one-or-two suffix, and one shortest input at the source archive's mechanically furthest `(world, level, progress)` tuple. Arms differ only by the installed ranking.
- Ranking search promotion requires onward on at least 4/6 seeds with no 1-2 regression. A null search result does not reject the validated M13 capability; it rejects only this generated ranking's SMB promotion. Two pre-model evidence-build directories preserve the superseded interface preflights; the scored raw destination is `target/smb-completion/m13-luna-20260811-ranked/`.

### M13 result — capability accepted, SMB ranking not promoted

- Luna xhigh attempt 1 emitted a pure bounded WRAM-richness ranking and passed source validation, recorded-observation determinism fixtures, and the fixed seed `0x5eed_ef00` 256-execution pilot. The pilot made 3 full-cell replacements with 2 descendant-novelty cells; its complete candidate archive replayed exactly with no model.
- Control 1-2 frontier maxima were `[139, 137, 149, 138, 152, 153]`; ranked maxima were `[138, 137, 139, 195, 196, 148]`. Controls reached onward on 0/6. Rankings reached onward on 2/6, seeds e003 and e004, first entering 1-3 at execution 2,656 on e003.
- Ranked replacement counts were `[711, 764, 619, 547, 474, 741]`; descendant-novelty counts were `[130, 204, 102, 274, 349, 672]`. Every ranking remained active, so the mechanical retirement rule correctly did not discard a ranking that kept producing descendant novelty.
- The full 5,000-execution e000 ranking report reproduced byte-for-byte from the recorded generated source with no model, and `m13-report.json` records `replay_verified=true`.
- Decision: M13 capability is accepted and lands. This generated ranking is not promoted as the SMB search configuration because 2/6 misses the registered 4/6 threshold; no held-out panel is due. The two reproducible onward gains remain evidence for ordering the next hypothesis.

### Post-M13 frontier diagnosis — pass

- M13 ranked seed e004 retained 700 states in 1-3 and reached a mechanically furthest entry at `(world 0, level 2, progress 195)`. Its observer-event manifest contains 583 raw-plus-decoded events for the 222-action clean-reset input.
- At the frontier event, raw page `12` and x `55` independently yield `12 * 16 + floor(55 / 16) = 195`; the decoded flag task is active and the film visibly shows Mario on the 1-3 flagpole. Result: **pass**. This is a genuine level-end state, not a progress decode fault. Raw evidence: `target/smb-completion/m13-frontier-film/`.

## H11 — preregistered within-level frontier continuation

- Falsifiable claim: after the first newly reached level, milestone-only `champion_input` resume throws away the later level-end state needed for the next transition. Resuming from the exact mechanically furthest state in the same archive will reach 1-4 reproducibly, while resuming from the first-onward champion will not.
- Source is the exact M13 ranked e004 report. Control selects its recorded `champion_input`; challenger selects the shortest recorded input at the maximum `(world, level, progress)` tuple. Both reconstruct from clean gameplay genesis and then run the accepted H3 archive search: frozen H3 parent scheduler and controller vocabulary, stratified durations, one-or-two action suffixes, 512-action bound, no ranking, and 5,000 target executions.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires recorded `(world 0, level 3)` entries on at least 4/6 challenger seeds, strictly more paired successes than control, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; exact replay is due only for a promoted result.
- Raw destination: `target/smb-completion/h11-dev/` and, only after development acceptance, `target/smb-completion/h11-held/`.

### H11 result — rejected

- Controls and challengers reached 1-4 on 0/6 development seeds. No held-out panel or promotion replay is due.
- Challenger frontiers were uniformly `(world 0, level 2, progress 195)`. Control frontiers were `[195, 195, 20, 19, 20, 19]` in 1-3, so mechanical-frontier resume preserved the flagpole state more consistently but never completed the transition.
- Challenger retained-entry counts were `[3779, 3653, 3770, 3844, 3727, 3920]`; deaths were `[43, 38, 50, 37, 37, 32]`. The archive continued to grow normally while every challenger stalled at the same independently validated numeric boundary.
- Decision: selecting the later clean-reset bootstrap state is insufficient. The repeated full-cell search at a genuine flag-active boundary is evidence that later transition preparation is being lost among representatives inside existing cells, not evidence for another progress-key or scheduler change. Raw evidence: `target/smb-completion/h11-dev/`.

## H12 — preregistered ranked flag-transition continuation

- Falsifiable claim: the validated M13 generated ranking preserves within-cell state that is better prepared to finish the 1-3 flag transition, so installing that recorded ranking during continuation from the exact 1-3 frontier will reach 1-4 reproducibly.
- Smallest test: source remains the exact M13 ranked e004 report and bootstrap selection remains its shortest input at maximum `(world, level, progress)`. Controls are the completed H11 mechanical-frontier arms. Challengers differ only by installing the exact model-generated M13 ranking source already validated and recorded for replay. The accepted H3 scheduler, controller vocabulary, stratified durations, one-or-two suffix, 512-action bound, and 5,000-execution budget remain frozen.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires recorded `(world 0, level 3)` entries on at least 4/6 challenger seeds, strictly more successes than the 0/6 controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; exact no-model replay is due only for a promoted result.
- Raw destination: `target/smb-completion/h12-dev/` and, only after development acceptance, `target/smb-completion/h12-held/`.

### H12 result — rejected

- Challengers reached 1-4 on 0/6 development seeds, matching the 0/6 H11 controls. Every challenger remained at `(world 0, level 2, progress 195)`, so no held-out panel or promotion replay is due.
- The generated ranking was materially active: replacement counts were `[855, 602, 612, 875, 746, 829]` and descendant-novelty counts were `[273, 247, 210, 289, 139, 197]`. It remained installed and active in every arm under the mechanical retirement rule.
- Retained-entry counts were `[4297, 3881, 3984, 4255, 4158, 4286]`; deaths were `[39, 34, 33, 34, 32, 31]`. Ranking-driven replacement and normal archive growth therefore did not make this H10-evidence ranking useful at the independently verified 1-3 transition boundary. Raw evidence: `target/smb-completion/h12-dev/`.

## H13 — preregistered plateau-evidence generated ranking

- Falsifiable claim: M13's first ranking was generated from the 1-2 approach corpus and is too generic for the later 1-3 transition. A fresh ranking chosen by the instrumentor from the exact recorded 1-3 plateau corpus will preserve within-cell preparation that reaches 1-4 reproducibly.
- Source is H12 seed e003, selected mechanically because it produced the most descendant novelty among the tied progress-195 runs. The operator view contains only neutral campaign counts, the independently validated raw-plus-decoded frontier film, and eight insertion-order-spaced recorded observation traces. It contains the ranking contract but no suggested score terms, fields, or goals. Luna xhigh has at most three compile/fixture attempts through the existing instrumentor decision schema.
- Validation remains fixed at seed `0x5eed_ef00` for 256 target executions and requires deterministic fixture scores, replacement-policy isolation, and exact archive-report replay with no model. The paired development panel uses seeds `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, the H12 source frontier, and the frozen H3 scheduler/controller/duration/suffix/action-bound configuration. Arms differ only by the newly recorded generated ranking.
- Acceptance requires recorded `(world 0, level 3)` entries on at least 4/6 ranking arms, strictly more successes than controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; a promoted result must replay exactly with the recorded generated files and no model.
- Raw destination: `target/smb-completion/h13-luna-20260811/` and, only after development acceptance, `target/smb-completion/h13-held/`.

### H13 result — rejected

- Luna xhigh attempt 1 emitted a bounded liveness/engine-state/WRAM-activity ranking without using a progress measure. It passed source validation, deterministic observation fixtures, the fixed seed `0x5eed_ef00` 256-execution isolation pilot, and exact pilot replay with no model.
- Controls and ranking arms reached 1-4 on 0/6 development seeds. Every report remained at `(world 0, level 2, progress 195)`, so no held-out panel or promotion replay is due.
- Ranking replacement counts were `[818, 747, 802, 860, 666, 797]`; descendant-novelty counts were `[126, 1188, 95, 176, 99, 209]`. The ranking remained active in every run. Fresh plateau-specific ranking evidence therefore changed within-cell selection substantially without crossing the transition.
- Decision: a second active generated ranking null makes another ranking iteration lower priority than temporal-horizon diagnosis. Raw evidence and generated files: `target/smb-completion/h13-luna-20260811/`.

## H14 — preregistered frozen transition burst

- Falsifiable claim: the automatic 1-3 flag sequence lasts longer than the accepted H3 execution's maximum two 120-frame chords, and useful intermediate animation states are not retained even under two active generated rankings. Extending only the state-blind suffix horizon to at most four chords will observe the 1-4 transition within one target execution reproducibly.
- Source is unchanged H12 seed e003 and bootstrap selection remains its shortest maximum-`(world, level, progress)` input. Controls are the completed H13 controls. Challengers use the frozen H3 parent scheduler, nine-mask controller vocabulary, stratified duration policy, archive keys/retention, no ranking, 512-action bound, and 5,000-execution budget. The sole change is the previously fixed burst distribution: one, two, three, or four chords with probabilities 1/2, 1/4, 1/8, and 1/8.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires recorded `(world 0, level 3)` entries on at least 4/6 challengers, strictly more successes than the 0/6 controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; a promoted result must replay exactly without a model.
- Raw destination: `target/smb-completion/h14-dev/` and, only after development acceptance, `target/smb-completion/h14-held/`.

### H14 registered result — rejected under the recorded decoder

- Recorded keys reached nominal 1-4 on 0/6 challengers and all reported `(world 0, level 2, progress 195)`, matching the registered 0/6 decision rule. Retained-entry counts were `[5367, 5266, 5121, 5217, 4859, 4839]`; deaths were `[39, 63, 37, 49, 45, 46]`. No held-out panel is due under that preregistration.
- The result immediately triggered the standing plateau-instrumentation check rather than another temporal redesign. Raw evidence: `target/smb-completion/h14-dev/`.

### Post-H14 level decode diagnosis — fail

- The supposed M13/H11–H14 1-3 flag frame visibly reads `WORLD 1-2` in the HUD. Its raw WRAM has `LevelNumber=$02` and `StarFlagTaskControl=$05`, while the old decoder reported zero-based level 2. Exact H14 flag descendant 362 then shows the black transition frame with the task cleared and the same raw level number.
- A mechanically deepest retained descendant of that exact flag lineage, archive id 5279, visibly plays `WORLD 1-3` while raw `LevelNumber` remains `$02`; the old decoder still reports level 2. This proves that a real 1-2 -> 1-3 transition was scored as no level gain.
- The SMB disassembly independently identifies `$075c` as `LevelNumber` and shows that the `RdyNextA` path increments it exactly when `StarFlagTaskControl == $05`, before the new area loads. The corrected current-level decode therefore uses `LevelNumber - 1` only during task 5, and the raw level number otherwise. The earlier 1-1 flag fixture at task 2 remains level 0.
- Consequence: M13's reported 2/6 `reached_onward` values were false positives at the late 1-2 flag task, and H11–H14's nominal 1-4 tests were actually exploring the 1-2 -> 1-3 transition. Their raw reports remain immutable evidence, but their semantic level labels are superseded by this failed diagnosis. Diagnostic films: `target/smb-completion/h14-transition-diagnosis/`, `h14-transition-lineage-diagnosis/`, and `level-decode-1-1-flag-diagnosis/`.

## H15 — preregistered transition-aware level decode

- Falsifiable claim: correcting only the phase-timed level decode will let the existing frozen H3 scheduler recognize and extend the already observed 1-2 -> 1-3 transition reproducibly, without another ranking or search-policy change.
- Source remains the exact recorded H12 e003 archive and its same shortest old-key frontier input. Development seeds remain `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, frozen H3 scheduler and nine-mask vocabulary, stratified durations, one-or-two suffix, 512-action bound, and no ranking. The sole behavioral change is the corrected current-level observation used by milestones and archive keys.
- Acceptance requires corrected `(world 0, level 2)` entries on at least 4/6 seeds and no regression below visible 1-2. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted result must replay exactly without a model.
- Raw destination: `target/smb-completion/h15-dev/` and, only after development acceptance, `target/smb-completion/h15-held/`.

### H15 result — accepted

- Development retained genuine corrected 1-3 entries on 6/6 seeds, with within-level frontiers `[19, 19, 20, 20, 19, 19]`. Retained-entry counts were `[3738, 3929, 3855, 4253, 3938, 4134]`; deaths were `[51, 31, 35, 29, 33, 33]`.
- Held-out retained genuine corrected 1-3 entries on 6/6 seeds, with frontiers `[20, 20, 20, 19, 20, 20]`. Retained-entry counts were `[4034, 4053, 4145, 4184, 4107, 4081]`; deaths were `[23, 49, 37, 36, 45, 31]`.
- Promoted campaign is held-out seed `0x5eed_e102`: 5,000 executions, 4,145 retained entries, 37 deaths, corrected 1-3 progress 20. Champion input SHA-256 is `acdf519be359d21a27cc91354cfe1bfd0445d9c2242c3a3eaa15349f6e4efe81`; observation SHA-256 is `5c9353565906478822f4449a07a3499cf0d8a25c2a93c184acb19eb1ee10827c`.
- The independent complete no-model replay is byte-identical; both archive reports have SHA-256 `dd3c9a40988434d5fa3188ce54ccc5c28d1fe94fddceef6c5e3cadf8d48f24db` and the summary records `replay_verified=true`.
- Decision: promote the transition-aware current-level decode. The 12/12 result and exact replay demonstrate that the prior hard boundary was an instrumentation fault, not a need for ranking, burst, or scheduler machinery. Raw evidence: `target/smb-completion/h15-dev/`, `h15-held/`, and `h15-promotion-replay-e102/`.

## H16 — preregistered genuine 1-3 continuation

- Falsifiable claim: H15 ended while its corrected 1-3 archive was still gaining cells at progress 20, so an unchanged continuation from the mechanically furthest genuine 1-3 state will reach genuine 1-4 more reproducibly than continuation from the first 1-3 champion.
- Source is the exact byte-replayed H15 promotion archive from held-out seed `0x5eed_e102`. Controls select its recorded `champion_input`; challengers select the shortest recorded input at the maximum corrected `(world, level, progress)` tuple. Both reconstruct from clean gameplay genesis and run the frozen accepted H3 scheduler, nine-mask controller vocabulary, stratified durations, one-or-two suffix, 512-action bound, no generated ranking, and 5,000 target executions. The source is not treated as a plateau: progress-20 cells were still being inserted at execution 4,996.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires genuine corrected `(world 0, level 3)` entries on at least 4/6 challenger seeds, strictly more successes than controls, and no regression below genuine 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; a promoted result must replay exactly without a model.
- Raw destination: `target/smb-completion/h16-dev/` and, only after development acceptance, `target/smb-completion/h16-held/`.

### H16 result — rejected

- Controls and challengers reached genuine corrected 1-4 on 0/6 development seeds, so no held-out panel or promotion replay is due. Control 1-3 frontiers were `[17, 18, 19, 19, 34, 19]`; every mechanically furthest challenger stopped at progress 20.
- Control retained-entry counts were `[4274, 3786, 4353, 3941, 4149, 3528]` with deaths `[43, 38, 33, 38, 46, 32]`. Challenger retained-entry counts were `[3242, 3483, 3245, 3513, 3817, 3346]` with deaths `[40, 30, 47, 38, 33, 44]`.
- Decision: reject mechanical-frontier continuation. Its 0/6 exact progress-20 boundary with thousands of retained states is the standing signature for a possible instrumentation fault. Before any search redesign, independently validate the decoded 1-3 fields at that boundary against raw WRAM and film evidence. Raw evidence: `target/smb-completion/h16-dev/`.

### Post-H16 plateau decode diagnosis — pass

- Exact challenger archive id 224 independently decodes raw screen page `$01` and x `$4c` as `1 * 16 + floor(76 / 16) = 20`; raw world/level are `0/2`, flag task is zero, and the film visibly shows the early 1-3 mushroom platforms. Exact control archive id 3564 independently decodes page `$02` and x `$22` as progress 34, with the same raw level and no flag transition; its film visibly shows later 1-3 platforms.
- Result: **pass**. Both numeric boundaries are genuine observations of live 1-3 terrain. The uniform challenger boundary is therefore a search/representative-quality failure, not a level or progress decode fault. Raw-plus-decoded manifests and film strips: `target/smb-completion/h16-plateau-diagnosis/`.

## H17 — preregistered current-plateau generated ranking

- Falsifiable claim: the H16 frontier source preserves a mechanically furthest progress-20 state but repeatedly loses an unkeyed liveness/preparation attribute inside later full cells. A fresh ranking chosen by the instrumentor from this corrected 1-3 corpus will preserve representatives that produce descendant progress beyond 20 more reproducibly than the identical unranked search.
- Source is H16 frontier seed `0x5eed_e004`, selected mechanically for the largest retained archive among the six tied progress-20 challengers. The operator view contains only neutral campaign counts, its independently validated raw-plus-decoded progress-20 film, and eight insertion-order-spaced recorded observation traces. It states the ranking contract but suggests no score terms, fields, or goals. Luna xhigh has at most three compile/fixture attempts through the existing decision schema.
- Validation remains fixed at seed `0x5eed_ef00` for 256 target executions and requires deterministic recorded-observation scores, replacement-policy isolation, and exact archive replay with the model absent. The paired development panel uses seeds `0x5eed_e000..=0x5eed_e005`, 5,000 executions, the frozen H3 scheduler/controller/duration/one-or-two suffix/action-bound configuration, and arms differing only by the installed ranking.
- Search acceptance requires progress greater than 20 in at least 4/6 ranking arms, strictly more paired successes than controls, and no regression below genuine 1-3. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. A promoted ranking campaign must replay exactly with recorded generated files and no model.
- Raw destination: `target/smb-completion/h17-luna-20260811/` and, only after development acceptance, `target/smb-completion/h17-held/`.

### H17 result — rejected

- Luna xhigh attempt 1 emitted a bounded live-state-evolution/changed-byte-coverage ranking without using a progress measure. It passed deterministic recorded-observation fixtures and the fixed seed `0x5eed_ef00` 256-execution isolation pilot; the pilot made 2 replacements and produced 1 descendant-novelty cell.
- Controls and ranking arms exceeded genuine 1-3 progress 20 on 0/6 development seeds. Every report remained at corrected `(world 0, level 2, progress 20)`, so no held-out panel is due.
- The ranking was materially active: replacements were `[652, 849, 659, 748, 757, 658]`, descendant-novelty counts were `[113, 207, 597, 232, 131, 162]`, and it remained active in every arm. Seed e000's complete 5,000-execution ranking report and independent model-free replay are byte-identical, both SHA-256 `ab6a548cadfcacbddee770ba5a45d61db514477d89ae7a94168adbb01beb0f67`.
- Decision: reject this ranking for SMB promotion. A third active ranking null at a film-validated boundary makes another score iteration lower priority than changing the single-state bootstrap assumption. Raw evidence and recorded generated files: `target/smb-completion/h17-luna-20260811/`.

## H18 — preregistered frozen frontier-state plurality

- Falsifiable claim: a single mechanically furthest input is a brittle bootstrap because one state can be poorly prepared even when another retained state at the same genuine frontier is viable. Seeding the unchanged frozen search with one shortest input per distinct `(player y bucket, player engine state)` at the exact frontier will cross H16 control's progress-34 boundary reproducibly.
- Source is H16 control seed `0x5eed_e004`, the only development archive to advance beyond progress 20 and the mechanically strongest genuine 1-3 source. Controls select its single shortest input at maximum `(world, level, progress)`; challengers select at most 64 exact-frontier inputs, deterministically taking the first shortest entry for each distinct `(player y bucket, player engine state)`. Both use the frozen accepted H3 scheduler, nine-mask vocabulary, stratified durations, one-or-two suffix, 512-action bound, no ranking, and 5,000 executions. No progress band or experimental scheduler is enabled.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires genuine 1-3 progress greater than 34 on at least 4/6 challengers, strictly more successes than controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; an accepted promotion must replay exactly without a model.
- Raw destination: `target/smb-completion/h18-dev/` and, only after development acceptance, `target/smb-completion/h18-held/`.

### H18 result — rejected

- Single-state controls and 16-state challengers exceeded genuine 1-3 progress 34 on 0/6 development seeds. All twelve reports stopped at corrected `(world 0, level 2, progress 34)`, so no held-out panel or promotion replay is due.
- Control retained-entry counts were `[3552, 3347, 3391, 3347, 3416, 3275]` with deaths `[43, 34, 43, 33, 43, 40]`. Plural retained-entry counts were `[2985, 2809, 2955, 3069, 2932, 3074]` with deaths `[30, 37, 32, 47, 35, 35]`.
- The zero-execution selector validation deterministically reconstructed 16 distinct exact-frontier inputs into 255 initial archive entries and replayed byte-for-byte. The null therefore rejects frontier-state plurality, not selector installation. The post-H16 raw/film diagnosis already independently validated this exact progress-34 boundary, so no new decode fault is indicated. Raw evidence: `target/smb-completion/h18-source-validation/` and `target/smb-completion/h18-dev/`.

## H19 — preregistered genuine 1-3 temporal burst

- Falsifiable claim: the early 1-3 mushroom-platform obstacle needs a longer coordinated action sequence than the accepted H3 execution's maximum two chords. Extending only the suffix horizon to the already-fixed up-to-four distribution will cross the independently validated progress-34 boundary reproducibly.
- Source is unchanged H16 control seed `0x5eed_e004`; both arms select its same single shortest input at maximum corrected `(world, level, progress)`. Controls are the completed H18 single-frontier arms. Challengers retain the frozen H3 parent scheduler, nine-mask controller vocabulary, stratified durations, archive keys/retention, no ranking, 512-action bound, and 5,000-execution budget. The sole change is the fixed suffix distribution: one, two, three, or four chords with probabilities 1/2, 1/4, 1/8, and 1/8.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires genuine 1-3 progress greater than 34 on at least 4/6 challengers, strictly more successes than the 0/6 controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; an accepted promotion must replay exactly without a model.
- Raw destination: `target/smb-completion/h19-dev/` and, only after development acceptance, `target/smb-completion/h19-held/`.

### H19 result — rejected

- Challengers exceeded genuine 1-3 progress 34 on 2/6 development seeds, reaching frontiers `[34, 34, 39, 34, 36, 34]` versus the controls' uniform 34. This misses the registered 4/6 threshold, so no held-out panel or promotion replay is due.
- Retained-entry counts were `[4423, 4447, 4832, 4671, 4972, 4360]`; deaths were `[36, 61, 45, 48, 40, 44]`. The archive did not exhaust, and the nonuniform 34/36/39 frontier is not the standing signature of a new decode fault.
- Decision: reject up-to-four burst as a reproducible promotion from the progress-34 source. The two paired gains are evidence that the longer horizon can cross this platform boundary occasionally, so the mechanically strongest immutable result is eligible as evidence/source for the next registered continuation. Raw evidence: `target/smb-completion/h19-dev/`.

## H20 — preregistered ratcheted burst continuation

- Falsifiable claim: H19's sporadic progress was limited by its starting boundary rather than by an exhausted burst policy. Resuming from its mechanically strongest genuine 1-3 archive and retaining the same up-to-four suffix distribution will advance beyond progress 39 reproducibly.
- Source is H19 seed `0x5eed_e002`, selected mechanically for the maximum corrected `(world, level, progress)` tuple of 1-3/39. Both arms select its same single shortest exact-frontier input and use the frozen H3 parent scheduler, nine-mask vocabulary, stratified durations, archive keys/retention, no ranking, 512-action bound, and 5,000 executions. Controls use one-or-two suffixes; challengers use the already-fixed up-to-four distribution. This is a continuation test, not retroactive H19 promotion.
- Development seeds remain `0x5eed_e000..=0x5eed_e005`. Acceptance requires genuine 1-3 progress greater than 39 on at least 4/6 challengers, strictly more successes than controls, and no regression below 1-3. Only then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; an accepted promotion must replay exactly without a model.
- Raw destination: `target/smb-completion/h20-dev/` and, only after development acceptance, `target/smb-completion/h20-held/`.

### H20 result — rejected

- Controls and challengers exceeded genuine 1-3 progress 39 on 0/6 development seeds. All twelve reports stopped at corrected `(world 0, level 2, progress 39)`, so no held-out panel or promotion replay is due.
- Control retained-entry counts were `[3547, 3737, 3590, 3581, 3742, 3829]` with deaths `[41, 29, 35, 28, 42, 36]`. Burst retained-entry counts were `[4646, 4569, 4484, 4767, 4623, 4632]` with deaths `[60, 44, 38, 42, 35, 38]`.
- Decision: reject ratcheted up-to-four continuation. The exact progress-39 boundary on 12/12 arms alongside normal archive growth is the standing signature of a possible observation fault; independently validate it against raw WRAM and film before another search redesign. Raw evidence: `target/smb-completion/h20-dev/`.

### Post-H20 plateau decode diagnosis — pass

- Exact control archive id 587 independently decodes raw screen page `$02` and x `$7b` as `2 * 16 + floor(123 / 16) = 39`; raw world/level are `0/2`, flag task is zero, and the film visibly shows the large early-1-3 mushroom-platform gap.
- Result: **pass**. Progress 39 is a genuine observation of live 1-3 terrain, not a level, transition, or scroll decode fault. Raw-plus-decoded manifest and film strip: `target/smb-completion/h20-plateau-diagnosis/`.

## M14 — preregistered generated archive mutator

- Mechanism claim: the existing `install_mutator` instrumentor decision can safely provide a bounded semantic suffix to archive search, allowing corpus-evidence-derived coordinated actions where blind one-to-four chord sampling is unreliable. This extends the existing mutator artifact into the archive path; it adds no protocol or decision kind.
- The live artifact remains one pure deterministic `SmbInput, seed -> SmbInput` function generated by Luna xhigh. Archive validation uses recorded progress-39 inputs, fixed seeds including zero and `u64::MAX`, requires prefix preservation, at least one bounded change, at most 512 actions, durations in `1..=120`, no panic on empty/at-cap inputs, and exact repeated output. Hand-written implementations are permitted only as scripted deterministic test fixtures.
- The archive gives the generated mutator one fixed choice in five; the other four choices retain the frozen H3 one-or-two suffix generator. Generated attempts, retained offspring, consecutive nonretained offspring, retirement execution, and active state are recorded. Retirement is mechanical after 128 consecutive emitted offspring fail to enter the archive; skipped/no-change outputs do not count. The mutator cannot change novelty, cell keys, parent scheduling, or retention.
- Source evidence is H20's exact progress-39 archive/film plus eight insertion-order-spaced observation traces and neutral campaign counts. The operator view states only the archive-mutator interface and supplies no action suggestion, route, field meaning, or goal. At most three compile/fixture attempts are allowed. A fixed seed `0x5eed_ef14`, 256-execution control/candidate pilot must isolate the mutator and replay exactly without a model before a full panel.
- Search utility uses the same H20 progress-39 source, seeds `0x5eed_e000..=0x5eed_e005`, 5,000 executions, frozen H3 scheduler/vocabulary/durations/one-or-two suffix/action bound, and arms differing only by the installed generated mutator. Acceptance requires progress greater than 39 on at least 4/6 mutator arms, strictly more successes than controls, and no regression below 1-3. If accepted, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`; promotion must replay exactly from recorded generated files with no model.
- Raw destination: `target/smb-completion/m14-luna-20260811/` and, only after development acceptance, `target/smb-completion/m14-held/`.

### M14 result — capability accepted, generated mutator not promoted

- Luna xhigh attempt 1 emitted a seed-parameterized, corpus-derived semantic suffix mutator. It passed schema/source validation plus deterministic empty, exact-frontier, and at-cap prefix/bounds fixtures on seeds zero, `0x5eed_ef14`, and `u64::MAX`.
- The fixed 256-execution isolation pilot invoked the mutator 48 times, emitted 48 bounded offspring, retained 45, stayed active, and reproduced the complete candidate archive exactly with the model absent. The control recorded no installed mutator. Decision: accept the generated archive-mutator mechanism.
- Development controls and mutator arms exceeded genuine 1-3 progress 39 on 0/6 seeds, so this generated mutator is not promoted and no held-out panel is due. Control retained-entry counts were `[3658, 3691, 3977, 3682, 3635, 3576]`; mutator retained-entry counts were `[9362, 9862, 8424, 9860, 8445, 8876]`.
- Full-arm mutator attempts were `[968, 986, 982, 988, 945, 1020]`, all emitted bounded offspring; retained-offspring counts were `[652, 686, 616, 673, 584, 583]`. Every mutator remained active with final nonretained streaks `[1, 1, 2, 4, 2, 1]`. The null is therefore search utility, not inactivity, invalid output, retirement, or replay failure.
- The panel obeyed the executor-rework steer's six-arm concurrency cap by running controls and candidates in separate waves. Raw evidence and recorded generated files: `target/smb-completion/m14-luna-20260811/`.

## M15 — preregistered scheduling-path executor Phase 1

- Authority and frozen plan are the integrator specification
  `/Users/phemberger/workspace/steers/EXECUTOR-REWORK.md`. Registration is
  recorded before inspecting any identity-gate result. M14 is complete, and no
  new campaign of 20,000 executions or more has started on the legacy executor.
- Phase 1 is a semantics-preserving transform. Retained scheduling-path
  testcases pin their end snapshots, decoded observations, milestones, base
  features, and detector features. Mutants resume at the deepest unchanged
  retained prefix. A separate bounded transient cache may bridge execution and
  corpus admission. Stored null-detector traces retain raw WRAM only at frozen
  milestone crossings; compatibility detectors may explicitly require the old
  complete-raw trace. Synchronous triage timing is unchanged.
- Every campaign report records executor mode, total emulated/evaluated work,
  and snapshot restores. These are deterministic execution counters and must
  replay exactly. Corpus reports include insertion order, lineage, and
  host-owned producer tags.
- Identity gate seed is `0x5eed_ee01`. For maze, adventure toy, and SMB, run one
  legacy and one snapshot-resume campaign with a 5,000-execution ceiling.
  Acceptance requires bit-identical semantic reports after removing only the
  intentionally different executor-mode/work fields. The SMB new executor must
  emulate at most one tenth as many frames as legacy. Record wall time as
  non-semantic benchmark evidence and report both ratios.
- A Criterion bench compares the two SMB executors at a frozen seed and fixed
  50-execution budget without a model or network. Raw identity evidence:
  `target/smb-completion/executor-phase1-identity/`; benchmark evidence:
  `target/criterion/`.

### M15 result — accepted by integrator ruling

- The fixed 5,000-execution gate is semantically exact on all three scheduling
  targets after normalizing only executor mode and work counters. Semantic
  SHA-256 values are maze
  `6e1500f1f0baa2a479f5828158e68995deef395d2ac5b6eeb45d858f5c6b7844`,
  adventure
  `8debc5e902d71df10c18ab868df375a4b84478338686cc2bdff8f97f42a62153`,
  and SMB
  `085c22ee1c3f8a57f93362c2d74fd38d2ab5810da42df99d655126ea230ba2a4`.
  Corpus insertion order, lineage, producer tags, milestones, and reports match.
- Replay-stable work was maze `648 -> 354` decisions with 178 restores;
  adventure `7,515 -> 4,485` actions with 4,999 restores; and SMB
  `730,736 -> 460,921` emulated frames with `4,966 -> 5,149` restores. The
  shallow SMB frame ratio is 1.59x and observed wall ratio is 1.90x.
- The frozen local M15 registration's tenfold condition is false. Integrator
  ruling on 2026-08-12 confirms that EXECUTOR-REWORK amendment 3 supersedes
  that registered threshold because it originated as an integrator analysis
  error. Independent exact 20,000-execution deep-regime runs on macOS and Linux
  measured `4,058,377 -> 2,027,227` frames (2.00x) with bit-identical semantic
  reports. The frozen registration remains above unchanged; this ruling beside
  it withdraws only its erroneous performance threshold.
- Criterion's fixed seed, 50-execution comparison recorded legacy
  `[1.3313, 1.3730]` seconds and snapshot resume `[1.7523, 1.7829]` seconds per
  campaign. The shallow serial workload regresses because action-boundary state
  serialization costs more than it saves; this is recorded rather than hidden.
- M15 acceptance is therefore the bit-identical identity gate, replay-exact
  deterministic work counters, and honest measured ratios. All three hold, so
  the integrator accepts M15. Executor selection remains fixed and recorded per
  campaign: use legacy for shallow workloads where it is faster and
  snapshot-resume where depth pays, never mixing modes within one campaign.
- Phase 2 is explicitly routed to separate executor-performance work and does
  not begin here. Completion work returns to the independently verified genuine
  1-3 progress-39 frontier.
- Raw evidence is under
  `target/smb-completion/executor-phase1-identity/` and
  `/private/tmp/harmony-smb-dissonance-target/criterion/`.

## H21 — preregistered current-plateau controller closure

- Falsifiable claim: the independently filmed progress-39 obstacle is a large
  1-3 gap, but the frozen nine-chord controller cannot hold run+right (`0x41`)
  or run+jump+right (`0xc1`). Closing the existing horizontal × A/B chord
  product, without changing the scheduler or suffix horizon, will advance
  beyond corrected 1-3 progress 39 reproducibly.
- Source remains H20 seed `0x5eed_e002`, selected mechanically for its maximum
  corrected `(world, level, progress)` tuple. Controls are the completed M14
  control arms, which use this same source, seeds `0x5eed_e000..=0x5eed_e005`,
  5,000 executions, frozen H3 parent scheduler, stratified durations,
  one-or-two suffixes, archive keys/retention, no ranking or generated mutator,
  and a 512-action bound. Challengers differ only by adding the five symmetric
  chord combinations already fixed in H8: B+right (`0x41`), B+left (`0x42`),
  A+B (`0xc0`), A+B+right (`0xc1`), and A+B+left (`0xc2`).
- H20 states at progress 39 use only 234–246 of 512 actions, so this is not an
  action-cap test. M14's generated suffixes never emitted `0x41` or `0xc1`, so
  its null does not cover the missing running-jump representation. H8 tested
  the same global, state-blind closure at an earlier 1-2 boundary; H21 tests its
  utility at the distinct film-validated 1-3 gap without carrying H8's
  experimental progress-band scheduler.
- Acceptance requires progress greater than 39 on at least 4/6 challenger
  seeds, strictly more successes than the 0/6 controls, and no regression below
  1-3. If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; a promotion must replay exactly without a model.
  The snapshot-backed archive executor is fixed for every arm and recorded as
  snapshot-resume; executor modes are never mixed inside a campaign. The six
  challenger arms run in one wave, respecting the concurrency cap.
- Raw destination: `target/smb-completion/h21-dev/` and, only after development
  acceptance, `target/smb-completion/h21-held/`.

### H21 result — rejected

- Closed-vocabulary challengers exceeded corrected 1-3 progress 39 on 0/6
  development seeds, matching the completed controls' 0/6 result. Every report
  stopped at `(world 0, level 2, progress 39)`, so no held-out panel or
  promotion replay is due.
- Retained-entry counts were `[3720, 3808, 3867, 3668, 3525, 3411]`; deaths were
  `[37, 27, 27, 39, 35, 33]`. Retained inputs contained respectively
  `[19481, 19956, 21218, 19161, 17261, 17059]` occurrences of B+right or
  A+B+right, proving the closed controller representation was active rather
  than dormant.
- Decision: reject controller closure alone at the progress-39 boundary. The
  independently validated H20 raw/film diagnosis already covers this exact
  uniform boundary, so the result is a search failure rather than a new decode
  fault. Raw evidence: `target/smb-completion/h21-dev/`.

## H22 — preregistered current-plateau progress-equivalent scheduling

- Falsifiable claim: the nominal frozen 128-entry frontier ranks vertical
  bucket, engine state, and state fingerprint after progress, collapsing
  parent selection to a nearly uniform offscreen-state slice. Treating every
  active entry at the strongest `(milestones, world, level, progress)` tuple as
  progress-equivalent will advance beyond corrected 1-3 progress 39
  reproducibly.
- Direct audit of completed H21 seed e000 reconstructs the active archive and
  frozen sort exactly: all 128 selected parents are progress 39, 118 have
  `player_y_bucket=15`, the remaining 10 have bucket 14, and all 128 have
  `player_engine_state=8`. Their inputs use only 236–244 of 512 actions. This
  is the same generic tie-break failure signature diagnosed at H5, now at a
  distinct film-validated obstacle.
- Source remains H20/H21's exact H19 seed `0x5eed_e002` progress-39 report.
  Controls are the completed M14 control arms. Challengers keep the frozen
  nine-mask controller, stratified durations, one-or-two suffix, archive
  keys/retention, no ranking or generated mutator, 512-action bound, and 5,000
  executions. The sole change is the existing 3/4 frontier branch: uniformly
  select from all active entries tied at maximum
  `(milestone_key, world, level, progress)`, ignoring vertical/state/fingerprint
  only for parent choice. The 1/4 global archive branch is unchanged.
- Acceptance requires progress greater than 39 on at least 4/6 development
  seeds, strictly more successes than the 0/6 controls, and no regression below
  1-3. If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; a promotion must replay exactly without a model.
  The snapshot-backed archive executor is fixed and recorded for every arm;
  executor modes are never mixed. Run at most six arms concurrently.
- Raw destination: `target/smb-completion/h22-dev/` and, only after development
  acceptance, `target/smb-completion/h22-held/`.

### H22 result — rejected

- Progress-equivalent challengers exceeded corrected 1-3 progress 39 on 0/6
  development seeds, matching the completed controls. Every report stopped at
  `(world 0, level 2, progress 39)`, so no held-out panel or promotion replay is
  due.
- Retained-entry counts were `[3241, 3345, 3089, 3419, 3156, 2955]`; deaths were
  `[37, 27, 29, 37, 44, 39]`. The changed corpus counts confirm that removing
  the vertical/state/fingerprint tie-break affected scheduling, but it did not
  traverse the boundary.
- Decision: reject progress-equivalent scheduling alone. The result shifts the
  next test from diversity among states at the lip to momentum preparation from
  earlier approach states. Raw evidence: `target/smb-completion/h22-dev/`.

## H23 — preregistered current-plateau approach-band scheduling

- Falsifiable claim: selecting only progress-39 parents begins too late to
  build the momentum required by the filmed large gap. Uniformly selecting on
  the frontier branch from a fixed eight-bucket approach band will advance
  beyond corrected 1-3 progress 39 reproducibly.
- The exact H19/e002 source has 1,934 active representatives in corrected 1-3
  progress buckets 32 through 39, with populated buckets
  `[32, 33, 34, 35, 36, 37, 38, 39]` and all sixteen vertical buckets. The
  approach evidence therefore exists in the retained source and does not
  require new keys, bootstraps, ranking, or route knowledge.
- Source, controls, seeds, 5,000-execution budget, frozen nine-mask controller,
  stratified durations, one-or-two suffix, archive keys/retention, no generated
  artifacts, and 512-action bound remain exactly H22's. The sole change is the
  existing 3/4 frontier branch: select uniformly from all active entries tied
  at maximum `(milestone_key, world, level)` and whose progress is within the
  inclusive eight-bucket band ending at the maximum. The 1/4 global branch is
  unchanged.
- Acceptance requires progress greater than 39 on at least 4/6 development
  seeds, strictly more successes than the 0/6 controls, and no regression below
  1-3. If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; a promotion must replay exactly without a model.
  The snapshot-backed archive executor is fixed and recorded; modes never mix.
  Run at most six arms concurrently.
- Raw destination: `target/smb-completion/h23-dev/` and, only after development
  acceptance, `target/smb-completion/h23-held/`.

### H23 result — rejected

- Approach-band challengers exceeded corrected 1-3 progress 39 on 0/6
  development seeds. Every report stopped at `(world 0, level 2, progress 39)`,
  so no held-out panel or promotion replay is due.
- Retained-entry counts were `[3647, 3858, 4028, 3871, 3716, 3452]`; deaths were
  `[30, 27, 30, 40, 44, 29]`. The populated source band and changed campaign
  corpora show the scheduler path was active, but earlier approach selection
  with the frozen controller was insufficient.
- Decision: reject approach-band scheduling alone. Raw evidence:
  `target/smb-completion/h23-dev/`.

## H24 — preregistered run-up controller interaction

- Falsifiable claim: the large filmed gap requires an interaction that neither
  isolated arm can express: choosing an earlier approach state and then holding
  run+right or run+jump+right. Combining H23's eight-bucket approach band with
  H21's symmetric A/B controller closure will advance beyond corrected 1-3
  progress 39 reproducibly.
- Source remains the exact H19/e002 progress-39 report. Controls are the
  completed H23 arms. Seeds `0x5eed_e000..=0x5eed_e005`, 5,000 executions,
  stratified durations, one-or-two suffix, archive keys/retention, no ranking
  or generated mutator, and the 512-action bound remain fixed. Challengers add
  only the five symmetric masks fixed before H8/H21 (`0x41`, `0x42`, `0xc0`,
  `0xc1`, `0xc2`) to H23's already-tested approach-band scheduler.
- This is a preregistered interaction test, not retroactive promotion of either
  0/6 main effect. Acceptance requires progress greater than 39 on at least 4/6
  challengers, strictly more successes than the 0/6 H23 controls, and no
  regression below 1-3. If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; promotion requires exact no-model replay.
  Snapshot-resume archive execution is fixed and recorded; modes never mix.
  Run at most six arms concurrently.
- Raw destination: `target/smb-completion/h24-dev/` and, only after development
  acceptance, `target/smb-completion/h24-held/`.

### H24 result — rejected

- Run-up/controller interaction challengers exceeded corrected 1-3 progress 39
  on 0/6 development seeds. Every report stopped at
  `(world 0, level 2, progress 39)`, so no held-out panel or promotion replay is
  due.
- Retained-entry counts were `[3637, 3826, 3628, 3809, 3739, 3725]`; deaths were
  `[26, 27, 36, 41, 40, 36]`. Combining the two active but individually null
  mechanisms therefore did not traverse the boundary. Raw evidence:
  `target/smb-completion/h24-dev/`.

## H25 — preregistered duration-stratum closure

- Falsifiable claim: the accepted stratified duration policy has a blind band:
  every sampled chord lasts either 2–12 or 96–120 frames. Adding a bounded
  middle hold stratum will advance beyond corrected 1-3 progress 39
  reproducibly by allowing control edges between a tap and an approximately
  two-second hold.
- Source remains the exact H19/e002 progress-39 report. Controls are the
  completed M14 control arms. Seeds `0x5eed_e000..=0x5eed_e005`, 5,000
  executions, frozen parent scheduler, frozen nine-mask controller, one-or-two
  suffix, archive keys/retention, no ranking or generated mutator, and the
  512-action bound remain fixed. The sole change is duration sampling: choose
  short 2–12, middle 32–64, or long 96–120 with equal seeded probability, then
  sample uniformly inside the selected inclusive stratum.
- Acceptance requires progress greater than 39 on at least 4/6 challengers,
  strictly more successes than the 0/6 controls, and no regression below 1-3.
  If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; promotion requires exact no-model replay.
  Snapshot-resume archive execution is fixed and recorded; modes never mix.
  Run at most six arms concurrently.
- Raw destination: `target/smb-completion/h25-dev/` and, only after development
  acceptance, `target/smb-completion/h25-held/`.

### H25 result — rejected

- Three-stratum challengers exceeded corrected 1-3 progress 39 on 0/6
  development seeds. Every report stopped at
  `(world 0, level 2, progress 39)`, so no held-out panel or promotion replay is
  due.
- Retained-entry counts were `[3780, 3783, 3465, 3395, 3538, 3618]`; deaths were
  `[38, 27, 30, 40, 43, 21]`. Filling the duration blind band changed campaign
  corpora without traversing the boundary. Raw evidence:
  `target/smb-completion/h25-dev/`.

## H26 — preregistered current-boundary checkpoint retention

- Falsifiable claim: a long chord can pass through a viable post-gap state and
  then fall before its endpoint, while Phase 4c retains snapshots only at chord
  endpoints. Splitting each newly sampled chord into adjacent identical-button
  segments of at most 12 frames will preserve an intermediate state that
  advances beyond corrected 1-3 progress 39 reproducibly.
- Source remains the exact H19/e002 progress-39 report. Controls are the
  completed M14 control arms. Seeds `0x5eed_e000..=0x5eed_e005`, 5,000 target
  executions, frozen parent scheduler, frozen nine-mask vocabulary, accepted
  two-stratum durations, one-or-two sampled chords, archive keys/retention, no
  ranking or generated mutator, and the 512-action bound remain fixed. The sole
  change is deterministic representation: each sampled chord is encoded and
  executed as consecutive same-button segments of at most 12 frames, admitting
  the existing action-boundary snapshots after each segment. The concatenated
  controller-frame stream is unchanged.
- H4 tested the same generic checkpoint size at the earlier 1-2 boundary, where
  a single-state bootstrap dominated; H26 tests the distinct, raw/film-validated
  1-3 gap after plural source evidence and current-boundary rankings, bursts,
  controllers, schedulers, and durations have all been measured null.
- Acceptance requires progress greater than 39 on at least 4/6 challengers,
  strictly more successes than the 0/6 controls, and no regression below 1-3.
  If accepted, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105`; promotion requires exact no-model replay.
  Snapshot-resume archive execution is fixed and recorded; modes never mix.
  Run at most six arms concurrently.
- Raw destination: `target/smb-completion/h26-dev/` and, only after development
  acceptance, `target/smb-completion/h26-held/`.

### H26 interrupted before result — superseded by frontier-viability steer

- The integrator paused new mechanism panels after the six H26 development
  processes had started but before any arm wrote a campaign report. All six
  were terminated; completed-report count is zero. No partial process state or
  outcome was inspected, scored, or used. H26 remains registered above but has
  no decision.
- The required next work is one no-search-change frontier-viability diagnostic
  on the exact progress-39 frontier and H23 approach band, plus a per-frame
  maximum-progress watermark. Raw empty/interrupted process logs remain under
  `target/smb-completion/h26-dev/` as an audit trail.

### H26 resumed result — rejected

- After D27 rejected the unrecoverable-frontier diagnosis, H26 resumed without
  changing its registered mechanism, seeds, budget, source, controls, or
  acceptance rule. Checkpoint challengers exceeded corrected 1-3 progress 39
  on 0/6 development seeds. Every retained frontier and every per-frame
  maximum-progress watermark stopped at `(world 0, level 2, progress 39)`, so
  no held-out panel or promotion replay is due.
- Retained-entry counts were `[9045, 9365, 9716, 7941, 9220, 8477]`; deaths
  were `[10, 16, 23, 31, 22, 52]`. This substantial corpus expansion proves
  that the 12-frame checkpoint representation was active, while the unchanged
  watermarks prove no crossing was merely lost between retained endpoints.
- Decision: reject current-boundary checkpoint retention. The unpromoted
  mechanism is removed from the live path; its focused deterministic
  frame-stream and exact-replay fixture passed before the panel. The six
  completed reports are under
  `target/smb-completion/h26-dev/rerun-e000-checkpoints/` through
  `rerun-e005-checkpoints/`; the earlier empty interrupted logs remain beside
  them.

## D27 — preregistered frontier-viability diagnostic

- Diagnostic claim: retained evaluation-end states at the progress-39 boundary
  may already be below the playable area and unrecoverable before the engine
  reaches kill state `$0b`; if so, the archive's apparent frontier is dominated
  by doomed states rather than viable parents.
- Source is the exact H19/e002 report used by H20–H26. Reconstruct the active
  archive mechanically with the existing capacity-two, fewer-actions retention
  rule. Audit every active entry in maximal corrected
  `(world 0, level 2, progress 39)` cells and every active entry in the inclusive
  H23 approach band at progress 32–39. No parent selection or search execution
  occurs.
- For each audited input, replay from clean gameplay genesis to its endpoint,
  snapshot, then restore that identical snapshot once for each continuation:
  (a) no input and (b) each of the nine frozen single chords. Each continuation
  holds the chord for a fixed 120 frames. Classification is mechanical over the
  continuation traces: `kill_state` if player engine state reaches `$0b`;
  `below_playable` if the decoded y byte remains in bucket 15 at the end without
  kill; otherwise `controllable`. A parent is `doomed` only if all ten
  continuations classify as `kill_state` or `below_playable`; any controllable
  continuation makes it viable. Report counts and doomed fractions separately
  for the maximal frontier and approach band, with exact deterministic replay.
- Independently amend campaign reports with a per-frame maximum mechanical
  `(world, level, progress)` watermark so transient crossings inside an action
  are recorded even when the endpoint is not retained. The watermark is
  observation only and cannot affect novelty, keys, scheduling, parent choice,
  or admission. Add deterministic tests for both the viability audit and
  watermark replay.
- Decision rule: `mostly unrecoverable` means doomed fraction strictly greater
  than one half in the maximal frontier. If true, preregister the integrator's
  mechanical admission cleanup and rerun H24 unchanged on the cleaned archive.
  Otherwise record the diagnosis false and return to the existing hypothesis
  order. No model is involved.
- Raw destination: `target/smb-completion/d27-frontier-viability/`.

### D27 result — frontier overwhelmingly viable; diagnosis false

- The mechanically reconstructed active maximal frontier contains 374 entries;
  only 15 are doomed under all ten fixed 120-frame continuations, a doomed
  fraction of `15/374 = 4.01%`. The inclusive progress-32-through-39 approach
  band contains 1,934 active entries; only 31 are doomed, `31/1934 = 1.60%`.
- The audit used no-input plus masks
  `[0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10]` and classified
  every parent deterministically. Its complete live and replay reports are
  byte-equal with SHA-256
  `6a5f7065f89fc67f6555a5681dfdc7c3ad8c84b5760da3170e0692505a9cc75a`;
  `viability-summary.json` records `replay_verified=true`.
- Decision: the frontier is not mostly unrecoverable. The registered strict
  majority threshold is missed by a wide margin, so the proposed doomed-state
  archive-admission cleanup and cleaned-H24 panel are not authorized and will
  not be implemented. Bucket-15 prevalence in the old 128-entry sorted window
  was a scheduler tie-break artifact, not evidence that the underlying maximal
  frontier was dominated by post-fall states.
- Campaign reports now carry a report-only per-frame maximum mechanical
  `(world, level, progress)` watermark, including action-interior crossings; a
  deterministic unit fixture verifies an interior progress 41 remains recorded
  when the endpoint returns to 39. It cannot affect search behavior. Raw
  evidence: `target/smb-completion/d27-frontier-viability/`.

## D28 — preregistered correction to frontier-viability classification

- Audit of the D27 implementation before the next mechanism panel found that
  its kill-state classification inspected only the endpoint after each
  120-frame continuation. A parent that entered engine state `$0b` and then
  reset to ordinary play within the continuation could therefore be counted as
  controllable. This does not satisfy the integrator's specified “reaches the
  kill state” classification.
- Rerun the exact D27 source, active-entry reconstruction, 120-frame budget,
  null input plus frozen nine single chords, ordering, and replay. The sole
  correction is mechanical: classify a continuation as kill-state if any
  recorded observation during the continuation, or its endpoint, has player
  engine state `$0b`; otherwise retain D27's endpoint below-playable and
  controllable rules. Do not inspect the corrected result before this entry is
  recorded and do not start another search panel meanwhile.
- The branch rule remains the integrator's: more than half doomed in the
  maximal frontier authorizes the registered admission cleanup and unchanged
  H24 repeat; at most half records the diagnosis false and returns to the
  completion frontier. Live and model-free replay reports must be byte-exact.
- Raw destination: `target/smb-completion/d28-frontier-viability/`.

### D28 interrupted attempt — no result produced

- The first corrected-audit process was explicitly interrupted while its live
  pass was still running because the active completion goal was superseded by
  separately routed executor work. The audit writes its first report only
  after every live continuation completes, so the already-created destination
  directory remained empty. No corrected classification, fraction, or replay
  result existed to inspect, and no branch decision was made from that attempt.
- D28 now resumes by rerunning the frozen registration above from the start.
  Its source, reconstruction, continuation set, frame budget, ordering, replay
  gate, and strict-majority branch rule remain unchanged.

### D28 corrected result — frontier overwhelmingly viable; diagnosis false

- With kill-state classification applied to every recorded observation as
  registered, the maximal frontier has 15 doomed entries out of 374,
  `15/374 = 4.01%`. The inclusive progress-32-through-39 approach band has 31
  doomed entries out of 1,934, `31/1934 = 1.60%`.
- The correction changed no classification from D27. This is consistent with
  the target stopping a continuation on its first observed death frame, but
  D28 now establishes the specified trace-wide property directly rather than
  relying on that executor behavior.
- Live and no-model replay reports are byte-equal with SHA-256
  `6a5f7065f89fc67f6555a5681dfdc7c3ad8c84b5760da3170e0692505a9cc75a`;
  `viability-summary.json` records `replay_verified=true`.
- Decision: the registered strict-majority threshold is missed by a wide
  margin. The frontier is not mostly unrecoverable, so the doomed-state archive
  cleanup and unchanged H24 repeat remain unauthorized. Raw evidence:
  `target/smb-completion/d28-frontier-viability/`.

## M16 — preregistered verified model context and strategy journal

- Before another model-involved panel, add the same evidence-only field
  semantics and verified-dynamics context to both triage and instrumentor
  operator views. The text may state only meanings and dynamics independently
  established by the existing raw-WRAM, memory-decode, and film audits. It must
  not name this game or a resembling game, describe a route or layout, identify
  an item location, or suggest an artifact field, score term, action, or goal.
  Include the integrator's supplied observations-over-expectations sentence
  verbatim.
- Every instrumentor request carries a structured prompt-only strategy journal
  with `beliefs`, `failed_approaches`, `open_questions`, and `current_plan`.
  The initial journal summarizes only recorded plateau evidence. Each returned
  journal is limited to 1,200 whitespace-delimited words; an oversized result
  gets one compression retry, and a still-oversized retry is rejected in favor
  of the previous journal. Compression may not alter the initial call's
  mechanical decision or generated source.
- Record each journal input, initial output, optional compressed output,
  effective output, cap decision, and host-level input/output exchange. The
  journal is never read by novelty, archive keys, scheduling, parent choice,
  admission, generated-artifact validation, or retirement. Recorded-journal
  replay must validate the exchange chain without a model, and the standard
  generated-file campaign replay must remain exact.
- Acceptance: checked-in schemas match the Rust types; deterministic fake-model
  fixtures prove recording, compression, rejection fallback, and journal-chain
  replay; both model views contain the complete verified context; workspace
  build, tests, Clippy, and formatting pass. No search campaign is part of M16.

### M16 result — accepted

- Both SMB model operator views now receive one plain sentence for every
  recorded observation field and the same short verified-dynamics account of
  progress, terminal death, post-death campaign persistence, and the frozen
  milestone ladder. The required observations-over-expectations sentence is
  present verbatim. A focused fixture rejects game names from the dynamics text
  and verifies the complete field list.
- Instrumentor decisions now carry a four-section strategy journal. Every call
  records its input, initial output, optional compression output, effective
  output, and cap status; the host records the input/output exchange and current
  journal as campaign artifacts. A deterministic no-model replay walks the
  recorded exchange chain and verifies its final state.
- Fake-model fixtures prove the 1,200-word limit, one compression retry, and
  rejection to the previous journal after a second oversized response. They
  also prove that compression cannot change the first call's action, source,
  name, scope, or rationale. The journal has no reader in novelty, keys,
  scheduling, parent choice, admission, validation, accounting, or retirement.
- Validation: all 47 fuzzer library tests, seven SMB model-host tests, four
  instrumentor tests, the remaining workspace tests and doc tests pass with
  cached/offline dependency resolution. Workspace Clippy, formatting, and diff
  checks pass; Clippy emits only the pre-existing configuration warning for the
  removed `rand::thread_rng` symbol.

## H27 — preregistered journal-informed ranking at the current frontier

- Falsifiable claim: with the verified field semantics, dynamics, recorded
  plateau history, and persistent strategy journal, the instrumentor can infer
  a non-progress within-cell ranking that preserves states better prepared to
  produce descendant novelty beyond genuine corrected 1-3 progress 39.
- Source is the exact H19 development seed `0x5eed_e002` archive at
  `target/smb-completion/h19-dev/e002-burst/archive-live.json`, whose maximum
  tuple is `(world 0, level 2, progress 39)`. Evidence includes its recorded
  observation traces and the independently validated raw-plus-decoded film
  manifest/video under `target/smb-completion/h20-plateau-diagnosis/`. The
  operator view contains M16's context and initial journal but no suggested
  ranking terms, fields, action, route, layout, item, or goal.
- Luna xhigh receives one ranking invocation through the existing decision
  schema, with at most three compile/fixture attempts. The ranking must pass
  the existing pure/deterministic source checks, observation fixtures, fixed
  seed `0x5eed_ef00` 256-execution isolation pilot, exact pilot replay, and M16
  journal-chain replay before the panel. Progress terms remain forbidden.
- Development controls and ranking arms use seeds
  `0x5eed_e000..=0x5eed_e005`, 5,000 target executions, the same H19 source,
  frozen accepted H3 scheduler, nine-mask vocabulary, stratified durations,
  one-or-two suffix, 512-action bound, and no other generated artifact. The
  ranking is consulted only for full-cell replacement; fewer actions remains
  the final tie-breaker. No arm reaches 20,000 executions, and at most six arms
  run concurrently.
- Acceptance requires the ranking arms to exceed corrected 1-3 progress 39 on
  at least 4/6 development seeds, strictly more successes than controls, with
  no regression below genuine 1-3. If accepted, repeat unchanged on held-out
  seeds `0x5eed_e100..=0x5eed_e105` and require the same threshold. Any
  promotion must replay exactly from recorded seed, observations, generated
  files, labels, and journals with no model. Otherwise record the ranking as a
  registered null and resume the mechanical queue only after this report.

### H27 pre-model launch failure — no decision produced

- The first host invocation prepared the complete operator view, then failed at
  the subprocess launch boundary with `No such file or directory`. The supplied
  instrumentor path `/private/tmp/harmony-smb-h27-target/release/instrumentor-agent`
  did not exist: the combined Cargo build emitted `smb-model-host` but not that
  separately selected binary. No `model-records`, journal, generated source,
  validation record, control, or ranking report exists, so Luna did not receive
  the view and no result was exposed.
- Build the same committed instrumentor-agent code explicitly and resume H27
  from its prepared evidence directory. The frozen source, evidence, model,
  attempts, seeds, budgets, controls, acceptance, and replay rules remain
  unchanged.

### H27 result — rejected

- Attempt 1 returned a vertical-readiness ranking but source validation rejected
  its use of `flag_active` because the existing no-progress validator forbids
  the token `flag`. Attempt 2 corrected that exact error and installed a bounded
  32-observation ranking over ordinary engine state, vertical bucket peak and
  motion, bucket diversity, and changed-index activity. Its 242-word journal
  passed without compression.
- Attempt 2 passed deterministic observation fixtures, fixed seed
  `0x5eed_ef00` 256-execution isolation control/candidate/replay, and the
  no-model journal-chain replay. Across the full panel the ranking remained
  active, made `[699, 622, 539, 689, 466, 511]` replacements, and those
  replacements produced `[190, 153, 168, 179, 162, 142]` descendant novelties.
- Controls exceeded corrected 1-3 progress 39 on 0/6 seeds and ranking arms on
  0/6. Every per-frame progress watermark remained exactly
  `(world 0, level 2, progress 39)`, so no held-out panel or promotion is due.
  Control retained counts were `[3711, 3715, 3892, 3560, 3651, 3654]`; ranking
  retained counts were `[3866, 3708, 3704, 3796, 3674, 3706]`.
- The complete seed-0 ranking replay is byte-equal with SHA-256
  `de7607e275f92c38f53497343c3e86338b2f89dc8d17e426d3580f8e60e20326`;
  the aggregate report records `replay_verified=true`. Decision: reject this
  journal-informed ranking for SMB promotion and resume the mechanical queue
  only after the ordered field-semantics correction below. Raw evidence and
  all model/generated/journal artifacts:
  `target/smb-completion/h27-luna-20260812/`.

## M16 amendment — ordered-field direction and range

- H27's recorded view and result remain immutable. Before any later
  instrumentor call, amend both model views so every spatial or ordered
  observation field states its verified range and direction. In particular,
  state explicitly that vertical buckets are `0..=15` and larger values are
  lower on the screen. This is decode polarity, not route knowledge.
- Record a standalone copy of the corrected operator context and seed the next
  strategy journal with both the direction correction and H27's null result.
  Validate the exact sentences mechanically and rerun the focused no-model
  context/journal gates. No model call or search panel is part of this
  amendment.

### M16 amendment result — accepted

- The next-call field semantics now state the inclusive range and direction
  for every spatial or ordered observation field. In particular,
  `decoded.player_y_bucket` is recorded as `0..=15`, with larger values lower
  on the screen. H27's registration, recorded view, and null result were not
  modified.
- The next journal carries both the polarity correction and the immutable H27
  null result, including that H27's larger-bucket-as-higher interpretation was
  opposite the corrected decode direction. These entries remain prompt-only.
- The standalone no-model record is
  `target/smb-completion/post-h27-model-context/`. Its field-semantics SHA-256
  is `e7445808395d7073c2f09ef04657fb27bcc492082a8080ffbf05e557c5695ba5` and
  its journal SHA-256 is
  `5b40286ca1304f892fd3e60c1c4a79efe23985d093e3ffe3f4193d51c0abbddb`.
- The focused host and instrumentor tests, format check, diff check, and
  focused clippy gate pass. Clippy still prints the pre-existing workspace
  configuration warning for the removed `rand::thread_rng` path; it reports no
  code warning.


## D29 — preregistered screen-relative player-column decode audit

- Integrator steer after H27: the uniform progress-39 watermark points at a
  missing observation field rather than a search failure. Progress is computed
  from the recorded screen-page and screen-x bytes, which measure the camera.
  Two retained states in the same progress bucket and vertical bucket are
  therefore indistinguishable even when one is most of a screen width behind
  the other, fewer-actions replacement plausibly keeps whichever input reaches
  the camera bucket first, and no ranking can prefer a nearer state because the
  quantity is not recorded anywhere.
- Diagnostic claim: a work-RAM byte measures the player's horizontal column
  within the visible screen, and it can be identified mechanically from this
  program's own recorded raw work RAM and rendered frames, without a
  disassembly, route, layout, or any external table.
- This registration revises an earlier draft of the same audit that was written
  but never executed. No process, report, or classification existed against
  that draft, so revising it is not an alteration of a frozen registration. The
  revisions are itemized in the final bullet.
- Source is the exact H19 development seed `0x5eed_e002` archive already used by
  H20 through H27. Reconstruct the active archive with the existing
  capacity-two, fewer-actions retention rule, order entries by `(input, id)`
  exactly as D27 and D28 do, and audit two fixed slices: the first eight active
  entries at the maximal corrected tuple `(world 0, level 2, progress 39)`, and
  the first eight at corrected `(world 0, level 2, progress 32)`. Direct
  reconstruction of the source counts 374 active entries at progress 39 and 228
  at progress 32, which is the lowest populated bucket of the registered
  progress-32-through-39 approach band. The second slice exists because a filter
  below requires continuations in which the recorded camera actually advances,
  and that is not guaranteed at the boundary itself.
- For each audited entry, replay from clean gameplay genesis to its endpoint and
  snapshot. From that identical snapshot run three continuations of 120
  single-frame chords each: no buttons `0x00`, right `0x01`, and left `0x02`.
  Record the complete 2 KiB work RAM and a 256-column rendered-frame signature
  at the endpoint and after every continuation frame. A continuation reaching
  player engine state `$0b` is truncated at that frame, which is retained, and
  only the recorded prefix is used. Rendering uses the video-enabled target; M4
  established that video-enabled and headless execution produce identical
  complete work-RAM traces.
- Write `camera` for `screen_page * 256 + screen_x` in pixels, `v(e, c, f)` for
  the audited index's value on entry `e`, continuation `c`, and recorded frame
  `f`, with `f = 0` the endpoint. Candidate filters, fixed before execution,
  applied to each of the 2,048 work-RAM indices:
  - C0: the index takes at least eight distinct values across all audited
    entries, continuations, and frames.
  - C1: the absolute change between consecutive recorded frames is at most 8
    everywhere.
  - C2: in the left continuation the final value is at least 8 below the
    endpoint value on at least twelve of the sixteen entries, and exceeds the
    endpoint value by more than 4 on none.
  - C3: in the right continuation the final value is never more than 16 below
    the endpoint value.
  - C4: a right continuation qualifies when its camera advances by 32 pixels or
    more. At least one audited right continuation must qualify, and in every
    qualifying continuation the index's absolute change must be strictly less
    than that continuation's camera advance. This separates a screen-relative
    byte from an absolute one. If no continuation qualifies, the audit reports
    C4 inapplicable and selects nothing.
- Film check, fixed before execution. A comparison is a pair of continuations of
  the same entry at the same recorded frame index, present in both, whose
  screen-page and screen-x bytes are equal in both and whose two candidate
  values differ by at least 8. Let `L` and `H` be the lowest and highest columns
  whose rendered pixels differ, `d` the candidate difference,
  `offset = L - min(candidate values)`, and `width = (H - L + 1) - d`. An index
  passes when some integer `o` in `-24..=24` has at least eight comparisons with
  `|offset - o| <= 6` and `width` in `4..=40`. Clarified before execution: the
  pass rule is exactly that existence test, and the `o` recorded beside it is
  the one the most comparisons agree with, breaking ties toward zero, so the
  recorded offset describes the identification rather than the low edge of the
  tolerance band. If no index passes, the film half is reported
  inconclusive and nothing is selected. The tolerance is deliberate: a byte that
  places a sprite may sit at a constant nonzero offset from the lowest column
  whose pixels actually differ, and occasional unrelated moving pixels must not
  falsify a real identification.
- Selection: record every index surviving C0 through C4 and the film check.
  Reject any survivor that has another survivor exactly 4, 8, or 12 bytes away
  in either direction, because a replicated four-byte-stride group is a
  rendering buffer rather than an engine variable. Choose the lowest remaining
  index. If none remains, the audit is inconclusive and no observation field is
  added.
- Fixed before execution, so a selected index cannot be described after the
  fact: if the audit selects an index, the decoded observation state gains
  exactly one field, `player_screen_column`, carrying that raw byte, and both
  model operator views gain exactly this sentence: "decoded.player_screen_column
  measures the verified player horizontal column within the visible screen in
  the inclusive range 0..=255, with larger values farther right on the screen;
  it is screen-relative and does not by itself indicate position within the
  level." Archive keys, the base novelty map, retention, scheduling, parent
  choice, admission, and milestones stay unchanged; the field is observation and
  ranking input only. The no-behavior-change gate is a fixed seed
  `0x5eed_ef00`, 256-execution archive campaign from the same source whose
  complete report must be byte-identical before and after the field is added.
- The audit runs no search, changes no search behavior, and involves no model.
  Its live and no-model replay reports must be byte-equal, and the replay's
  recorded frames must equal the live pass's. Rendered PNGs are written for the
  first, middle, and final recorded frame of every continuation of the first
  audited entry in each source slice, and for both frames of the first four
  agreeing film-check comparisons of the selected index, so the visual half can
  be inspected directly.
- Raw destination: `target/smb-completion/d29-player-column-decode/`.
- Revisions to the unrun draft: the audited set gains a second eight-entry slice
  at progress 32; continuations run 120 frames rather than 60, matching D27 and
  D28; the camera is stated in pixels; C4 states its qualifying threshold and
  its inapplicable outcome instead of failing silently; C0 excludes constant
  bytes; the film check replaces "the lowest differing column is within 4 of the
  smaller candidate value" and its span-spread rule with an agreeing-offset rule
  that tolerates a constant sprite offset and unrelated moving pixels; and the
  field name, its exact operator sentence, and the no-behavior-change gate are
  fixed here rather than left to be chosen after the result.

### D29 result — inconclusive; the audited continuations do not exercise the claim

- The audit ran exactly as registered on the sixteen selected entries. All eight
  progress-39 endpoints share camera pixel 635 and all eight progress-32
  endpoints share camera pixel 522; every one of the 48 continuations recorded
  its full 121 frames without reaching engine state `$0b`.
- Filter counts: 158 of 2,048 indices took at least eight distinct values, 2 of
  those also changed by at most 8 between consecutive frames, and 0 survived the
  left-direction filter. No right continuation advanced the camera by 32 pixels
  or more, so C4 was inapplicable by its registered rule. Nothing survived, the
  film half had no candidate to check, and the audit selected no index. No
  observation field is added and the registered no-behavior-change gate is not
  due.
- Live and no-model replay reports are byte-equal with SHA-256
  `cc24f4c74fe1ca6dc5b934df5abb4db32e04a334b0b2289d18516fbd22453f83`, the 18
  rendered frames are identical between the two passes, and
  `player-column-summary.json` records `replay_verified=true`. The audit
  therefore failed to identify a byte; it did not fail to run.
- The registered filters assume the audited endpoints are states the three
  continuations actually steer. Two independent recorded facts say these
  sixteen are not: no camera advance in any right continuation, and only two
  smoothly changing bytes in 5,760 recorded frames. Diagnose that before
  designing a second audit. Raw evidence:
  `target/smb-completion/d29-player-column-decode/`.

### Post-D29 continuation diagnosis — fail; two observation faults at the frontier

- The diagnosis re-ran D29's exact sixteen entries and recorded the program's own
  decoded state and milestones at every continuation frame, plus work RAM every
  tenth frame and frame strips every ten frames. It runs no search and involves
  no model. Raw evidence: `target/smb-completion/d29-diagnosis/`.
- On all sixteen entries the no-input, right, and left continuations are
  bit-identical for all 120 frames. The controller has no authority at these
  endpoints, which is why D29's direction filters had nothing to measure and why
  no camera advance existed for its relative test.
- The frame strips show why. At the audited endpoint the player is visible near
  the left edge of the screen and is already falling; by frame 60 the player is
  no longer drawn. Byte `$00ce`, the byte `player_y_bucket` is decoded from,
  advances about four per frame and wraps repeatedly, while byte `$00b5`
  increments once per wrap. The fall is therefore continuous, but the decoded
  vertical bucket cycles through all sixteen values while it happens.
- **First fault.** On ten of the sixteen entries the camera resets to page 0
  x 0 within 120 frames, the engine byte passes `$08` to `$06` to `$00`, and
  byte `$075a` decrements by exactly one at that frame. The decoded level tuple
  stays `(world 0, level 2)` across the reset. Throughout every one of these
  traces the program's own `decoded.dead` stays false, because the frozen
  terminal-death condition is `$000e == $0b` and this death path never takes
  that value. An execution that falls into the gap is therefore not stopped: it
  keeps running from the restarted level, and its later action-boundary
  snapshots are early-level states admitted under the same level tuple.
- **Second fault.** Because the decoded vertical bucket is `$00ce / 16` with no
  page term, one falling player passes through all sixteen vertical buckets at
  one camera bucket. Archive keys therefore admit a whole column of cells per
  fall. This is the mechanical origin of the vertical-bucket signatures already
  noticed at H5 and H22 and of the large retained-entry counts at progress 39.
- D27 and D28 could not see either fault. Their `kill_state` class requires
  `$0b`, which never occurs on this path, and their `below_playable` class
  requires vertical bucket 15 at the continuation endpoint, which the wrapping
  byte usually is not. Both audits therefore classified free-falling and
  already-restarted parents as controllable. Their registrations and reports
  stand as recorded, but their shared conclusion that the maximal frontier is
  overwhelmingly viable is not supported for this failure mode.
- The diagnosis inspected a small hand-chosen set of byte indices. The follow-up
  audit does not use that inspection: it remains a blind scan of all 2,048
  indices under fixed filters, verified against rendered frames.

## D30 — preregistered steerable-entry player-column decode audit

- D29 remains frozen above with its inconclusive result. D30 repeats the same
  diagnostic claim on entries the controller actually steers: a work-RAM byte
  measures the player's horizontal column within the visible screen, and it can
  be identified mechanically from this program's own recorded raw work RAM and
  rendered frames, without a disassembly, route, layout, or external table.
- Source, active-archive reconstruction, slice progress buckets 39 and 32,
  `(input, id)` ordering, three fixed continuations `0x00`, `0x01`, `0x02` of
  120 single-frame chords, per-frame work RAM and 256-column frame signatures,
  truncation at engine state `$0b`, the video-enabled target, the film check,
  the four-byte-stride rejection, lowest-index selection, the byte-equal
  live-and-replay gate, and the rendered PNG set are exactly D29's.
- The sole change is which entries are audited. Scan the ordered active entries
  of each slice and keep an entry only if it is steerable: the recorded work RAM
  at the last recorded frame of the right continuation differs from the left
  continuation's. This is a mechanical control-authority test over the same
  fixed continuations and uses no route, layout, or field meaning. Scan at most
  64 candidates per slice and audit the first eight steerable entries found in
  each. Record, per slice, the number scanned and the number steerable.
- Because the audited count is no longer fixed at sixteen, C2's threshold
  becomes at least three quarters of the audited entries, rounded up, instead of
  at least twelve. C0, C1, C3, C4, and the film check are unchanged. If fewer
  than eight entries are steerable in total, the audit records the counts and
  reports itself inconclusive without selecting an index.
- The steerable counts are themselves the registered evidence for how much of
  this frontier the controller can move, independent of whether an index is
  found.
- If the audit selects an index, the field name, the exact operator sentence,
  the unchanged-key requirement, and the fixed seed `0x5eed_ef00`,
  256-execution byte-identical archive gate are exactly as fixed in D29. The
  pre-change gate report is already recorded with SHA-256
  `383bc917ec0b1d3b6911059f1526cfd853b31f91bf10b32aeb9ee57a41fa7111` at
  `target/smb-completion/d29-field-gate-before/`.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d30-player-column-decode/`.

### D30 result — inconclusive; the steerability test admitted every entry

- The scan examined 8 entries in each slice and classified 8 of 8 as steerable
  in each, so D30 audited exactly D29's sixteen entries and reproduced its
  filter counts: 158 indices with at least eight distinct values, 2 also smooth,
  0 surviving the left-direction filter, 0 qualifying right continuations, no
  film candidate, and no selected index. Live and no-model replay reports are
  byte-equal with SHA-256
  `ccaddbcdcf076adc9fd697445aa6f3604a979ac84f9d2555c26894b7c4b02b99` and the
  summary records `replay_verified=true`.
- The registered test compared complete work RAM at the last recorded frame of
  the right and left continuations. Direct inspection of the recorded diagnosis
  traces shows that on these entries exactly four work-RAM bytes differ between
  those continuations — indices `$000a`, `$000d`, `$06fc`, and `$074a` — and all
  four hold the controller mask that was just applied. The test therefore
  measured that the two continuations pressed different buttons, not that the
  player moved, so it can never reject an entry.
- The frozen D29 audit was re-run under the refactored code and reproduced its
  recorded report byte for byte, so the shared audit path is unchanged. Raw
  evidence: `target/smb-completion/d30-player-column-decode/` and
  `target/smb-completion/d29-reproduction-check/`.
- Decision: reject whole-work-RAM difference as a control-authority test. The
  replacement must be stated over quantities this program has already decoded
  and verified.

## D31 — preregistered camera-advancing player-column decode audit

- Same diagnostic claim as D29 and D30: a work-RAM byte measures the player's
  horizontal column within the visible screen, identifiable mechanically from
  this program's own recorded raw work RAM and rendered frames without a
  disassembly, route, layout, or external table.
- Source, active-archive reconstruction, slice progress buckets 39 and 32,
  `(input, id)` ordering, the three fixed continuations of 120 single-frame
  chords, per-frame work RAM and 256-column frame signatures, truncation at
  engine state `$0b`, the video-enabled target, filters C0 through C4, the film
  check, four-byte-stride rejection, lowest-index selection, the byte-equal
  live-and-replay gate, and the rendered PNG set are exactly D29's, with C2's
  threshold scaled to three quarters of the audited entries as fixed in D30.
- The sole change is again which entries are audited. An entry is admitted only
  when its right continuation advances the recorded camera, `screen_page * 256 +
  screen_x`, by at least 32 pixels between the endpoint and the last recorded
  frame. The camera is the only quantity used, it is already decoded and film
  validated by this program, and it cannot be moved by a player who has no
  control. Scan at most 128 ordered active candidates per slice and audit the
  first eight admitted in each. Record, per slice, the number scanned and the
  number admitted.
- The admitted counts are registered evidence in their own right: they measure
  how much of this frontier and approach band the controller can still move
  rightwards, independent of whether an index is found. If fewer than eight
  entries are admitted in total, the audit records the counts and reports itself
  inconclusive without selecting an index.
- Implementation note recorded before execution: the endpoint replay now shares
  the longest common action prefix between consecutive ordered candidates, the
  same way D27 and D28 do, because scanning up to 256 candidates from gameplay
  genesis is otherwise too slow. Snapshot and restore are exact, so this changes
  cost and not results; the frozen D29 audit is re-run afterwards and must still
  reproduce its recorded report byte for byte.
- If the audit selects an index, the field name, the exact operator sentence,
  the unchanged-key requirement, and the fixed seed `0x5eed_ef00`,
  256-execution byte-identical archive gate are exactly as fixed in D29, whose
  pre-change report is recorded with SHA-256
  `383bc917ec0b1d3b6911059f1526cfd853b31f91bf10b32aeb9ee57a41fa7111`.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d31-player-column-decode/`.

### D31 result — inconclusive; no scanned frontier or approach entry has rightward control

- The scan examined the registered maximum of 128 ordered active entries in each
  slice. Zero of 128 at progress 39 and zero of 128 at progress 32 advanced the
  recorded camera by 32 pixels or more under 120 frames of held right. Nothing
  was admitted, nothing was audited, and no index was selected. Live and no-model
  replay reports are byte-equal with SHA-256
  `37306c90568dec881c306025a3960f4e0f64bf51ed03ce6f194b0be57a569801` and the
  summary records `replay_verified=true`.
- This is the registered evidence the audit was also meant to produce, and it is
  a stronger statement than the audit itself. Across 256 retained
  representatives spanning a third of the maximal frontier and more than half of
  the progress-32 bucket, holding right for two seconds never moves the camera
  once. Combined with the post-D29 diagnosis, the plain reading is that these
  cells are occupied by states in which the player has already fallen past the
  gap and cannot be steered anywhere.
- Consequence for the missing-field work: the horizontal column cannot be
  identified from this frontier, because identifying it requires states whose
  horizontal position responds to the controller and none of the scanned ones
  do. The audit mechanism is sound and reproduces exactly; it needs live source
  states.
- Consequence for the plateau: twelve consecutive mechanism panels selected
  parents from this pool. A scheduler, vocabulary, duration, burst, ranking, or
  mutator change cannot matter when the parents it chooses among have no
  control authority. Raw evidence:
  `target/smb-completion/d31-player-column-decode/`.

## D32 — preregistered control-authority census and sourced column audit

- Diagnostic claim: the retained archive at this source is largely occupied by
  states with no rightward control authority, and the fraction that retains it
  varies systematically with the progress bucket. If that is false, every
  populated bucket will show a similar admitted fraction.
- Census: reconstruct the active archive of the same exact H19 seed
  `0x5eed_e002` source with the existing capacity-two, fewer-actions rule. Take
  every active entry with corrected `(world 0, level 2)`, order them by
  `(progress, input, id)`, replay each to its endpoint with the prefix-shared
  path, and run one continuation of 120 single-frame right chords, truncating at
  engine state `$0b`. An entry is admitted when the recorded camera,
  `screen_page * 256 + screen_x`, advances by 32 pixels or more. Report per
  progress bucket the active count and the admitted count. No search runs, no
  search behavior changes, and no model is involved. Live and no-model replay
  reports must be byte-equal.
- Sourced audit, fixed here so no bucket is chosen after seeing the census: take
  the admitted entries in descending progress bucket, breaking ties by
  `(input, id)`, and audit the first eight. Everything else — the three
  continuations, per-frame work RAM and column signatures, C0 through C4 with
  C2 at three quarters of the audited entries, the film check, the four-byte
  stride rejection, lowest-index selection, the rendered PNG set, and the
  byte-equal live-and-replay gate — is exactly D29's. If fewer than eight
  entries are admitted anywhere, the audit reports itself inconclusive.
- If the audit selects an index, the field name `player_screen_column`, its
  exact operator sentence, the unchanged-key requirement, and the fixed seed
  `0x5eed_ef00`, 256-execution byte-identical archive gate are exactly as fixed
  in D29, whose pre-change report is recorded with SHA-256
  `383bc917ec0b1d3b6911059f1526cfd853b31f91bf10b32aeb9ee57a41fa7111`.
- Raw destinations: `target/smb-completion/d32-control-census/` and
  `target/smb-completion/d32-player-column-decode/`.

### D32 census result — six percent of the retained level has rightward control

- The census replayed all 3,292 active `(world 0, level 2)` representatives of
  the exact H19 seed `0x5eed_e002` source and admitted 211, or 6.4%. Live and
  no-model replay reports are byte-equal with SHA-256
  `a0ce35b930c5a0ac18bf469e3271fab8e452b9389915eba0f9fc9f15cbbf1d8e` and the
  summary records `replay_verified=true`.
- The registered claim that the admitted fraction is roughly uniform is false by
  a wide margin. Every heavily populated bucket is entirely uncontrollable:
  progress 17 admitted 0 of 257, progress 29 admitted 0 of 46, progress 30
  admitted 0 of 96, progress 32 admitted 0 of 228, progress 33 admitted 0 of 93,
  progress 34 admitted 0 of 1,224, and progress 39 admitted 0 of 374. The
  admitted entries sit almost entirely in the sparse early buckets — for example
  26 of 30 at progress 7 and 24 of 29 at progress 6 — plus ten scattered
  entries at progress 35, 36, and 37.
- The registered progress-32-through-39 approach band therefore contains 1,934
  active representatives of which exactly 10 can be moved rightwards at all.
  H23, H24, and every later panel selected parents uniformly from that band.
- This is the plateau in one line. The scheduler's frontier branch always ranks
  the maximum progress tuple highest, every one of the 374 representatives at
  that tuple has already fallen past the gap, and the handful of live states a
  screen behind it are never treated as the frontier. Raw evidence:
  `target/smb-completion/d32-control-census/`.

### D32 sourced audit result — inconclusive; filter C3 was wrong

- The audit sourced the eight highest-progress admitted entries, five at
  progress 37 and three at progress 36, with endpoint cameras from 577 to 602.
  All eight right continuations qualified for the camera-relative test, against
  zero in D29 and D31. 130 indices took at least eight distinct values, 34 were
  also smooth, and 7 survived the left-direction filter. All 7 were then
  rejected by C3, which requires the right continuation's final value to stay
  within 16 of the endpoint value, so nothing reached the film check and no
  index was selected. Live and no-model replay reports are byte-equal with
  SHA-256 `395bd27af22329f8ce3a2d0820298fbe44c1840deb87d921560d54246daedd0f`
  and the summary records `replay_verified=true`.
- C3 encodes a false assumption. A screen-relative column falls when the camera
  catches up: a player ahead of the scroll anchor who then holds right pushes
  the camera until the anchor settles, and the recorded column drops by far more
  than 16 while the player is moving right the whole time. C3 therefore rejects
  exactly the quantity the audit is looking for. No continuation restarted the
  level in this panel, so this is the anchor effect and not the missed death.
- C4 is weak for the same reason. Once a relative column may change by more than
  a hundred, requiring its change to stay below the camera advance rejects true
  candidates whenever the advance is modest.
- The reproducible part is now clear: seven smooth candidates that fall under
  held left, from live source states, with no verification step reached. Raw
  evidence: `target/smb-completion/d32-player-column-decode/`.

## D33 — preregistered camera-spread player-column decode audit

- Same diagnostic claim as D29 through D32. The source, active-archive
  reconstruction, the three fixed continuations of 120 single-frame chords,
  per-frame work RAM and 256-column frame signatures, the video-enabled target,
  four-byte-stride rejection, lowest-index selection, the rendered PNG set, the
  field name and operator sentence fixed in D29, and the byte-equal
  live-and-replay gate are unchanged.
- Audited entries: from the recorded D32 census, take admitted entries in
  descending progress bucket, breaking ties by `(input, id)`, and take at most
  two per bucket, until eight are chosen. Capping per bucket guarantees the
  audited endpoints span several camera positions, which the verification below
  requires.
- Continuations additionally truncate at the first frame whose recorded camera
  is below the previous frame's, retaining that frame, as well as at engine
  state `$0b`. The recorded camera never decreases during continuous play, so
  this bounds every continuation to one camera epoch and keeps a level reload
  out of the filters.
- Filters, fixed before execution: C0 requires at least eight distinct values
  across all audited frames; C1 requires consecutive recorded frames to differ
  by at most 8; C2 requires the left continuation's final value to be at least 8
  below the endpoint value on at least three quarters of the audited entries,
  rounded up, and to exceed the endpoint value by more than 4 on none. D29's C3
  and C4 are removed for the reasons recorded above; their counts are no longer
  reported.
- Verification replaces them. A comparison is a pair of continuations of the
  same entry at the same recorded frame index, present in both, with equal
  screen-page and screen-x bytes and candidate values differing by at least 8.
  With `L` and `H` the lowest and highest differing rendered columns and `d` the
  candidate difference, `offset = L - min(candidate values)` and
  `width = (H - L + 1) - d`. An index passes when some integer `o` in `-24..=24`
  has at least eight comparisons with `|offset - o| <= 6` and `width` in
  `4..=40`, and those agreeing comparisons include two whose recorded camera
  positions differ by at least 16 pixels. The camera-spread requirement is the
  discriminator D29 tried to get from C4: a byte holding an absolute position
  predicts the rendered column with an offset that shifts by the camera
  difference, so it cannot agree within a tolerance of 6 across cameras 16 or
  more apart, while a screen-relative byte agrees at every camera.
- The audit records the offset the most comparisons agree with, breaking ties
  toward zero, and its agreeing count and camera spread. If no index passes, the
  audit reports itself inconclusive and selects nothing.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d33-player-column-decode/`.

### D33 result — inconclusive; the census admitted momentum, not control

- The audit ran on eight admitted entries, two each at progress 37, 36, 35, and
  28, with endpoint cameras from 461 to 600. 149 indices took at least eight
  distinct values, 34 were also smooth, 5 survived the left-direction filter and
  reached verification: work-RAM indices 136, 137, 1208, 1210, and 1984. None
  passed the film check and no index was selected. Live and no-model replay
  reports are byte-equal with SHA-256
  `cadd2db2aadeb97a7c271b65111e08fdad812e18bcc4d8061980c28293708961` and the
  summary records `replay_verified=true`.
- A measurement dump over the same eight entries recorded zero film comparisons
  for all five candidates: on no frame did any of them differ by 8 or more
  between two continuations at an equal camera. They are not position bytes.
- A second dump explains why. On all eight entries the no-input and the held-left
  continuations render identically on every single frame; no rendered column
  ever differs. Byte `$00b5` rises from 1 to 3 while `$00ce` wraps, exactly the
  free-fall signature of the post-D29 diagnosis. The camera nevertheless advances
  — 592 to 635 on the first entry — because the player still carries rightward
  momentum while falling.
- The D32 census test is therefore not a control-authority test. A camera advance
  under held right admits a falling player who happens to be coasting. The
  census's 211 admitted entries are an upper bound on live states, not a count of
  them, and its per-bucket zeros stand unchanged.
- Raw evidence: `target/smb-completion/d33-player-column-decode/` and
  `target/smb-completion/d33-diagnosis/`.

### Standing summary of the completion boundary

- Across D29 through D33 the audit mechanism itself is sound: every pass
  reproduced byte for byte with no model, and the frozen D29 audit still
  reproduces its recorded report under all later refactors. What it could not
  find is a source state whose horizontal position responds to the controller.
- The reason is a single instrumentation fault with two consequences. The frozen
  terminal-death condition is `$000e == $0b`, and the fall-into-gap death does
  not take that value: the engine byte goes `$08` to `$06` to `$00`, byte
  `$075a` decrements by one, and the level reloads. An execution that dies this
  way is never stopped, so its action-boundary snapshots keep being admitted, and
  because the decoded vertical bucket is `$00ce / 16` with no page term, one
  fall walks through all sixteen vertical buckets and fills a column of archive
  cells at whatever camera bucket it fell from.
- The measured result is that 0 of 374 representatives at the maximal frontier,
  0 of 1,224 at progress 34, 0 of 228 at progress 32, and 0 of 257 at progress 17
  can be moved rightwards at all, and the handful outside those buckets that
  appeared to move turned out to be falling with momentum.
- This is a sufficient explanation for the exact progress-39 watermark in H21
  through H27 and for H26's and H27's unchanged per-frame watermarks. Those
  panels varied the scheduler, the controller vocabulary, hold durations, suffix
  depth, checkpoint retention, generated mutation, and generated ranking, while
  every parent they could choose among had already died.
- It also supersedes the shared conclusion of D27 and D28. Their `kill_state`
  class requires `$0b` and their `below_playable` class requires vertical bucket
  15 at a continuation endpoint; this failure mode produces neither.
- Correcting it is a search change — the death condition alters execution
  termination and admission, and a vertical page term alters archive keys — so
  it is not taken here. It is reported to the integrator with the evidence
  above.

## D34 — preregistered terminal-death decode audit

- Falsifiable claim: the frozen terminal condition `$000e == $0b` misses a death
  mode, and a condition over the current work RAM plus one byte's value at the
  start of a replay is false on every frame of recorded live play and true
  within 240 frames on every recorded uncontrolled continuation.
- Source is the unchanged recorded archive
  `target/smb-completion/h19-dev/e002-burst/archive-live.json`, seed
  `0x5eed_e002`, and the video-enabled target used by D29 through D33.
- Control population, fixed before execution: the recorded champion input
  replayed one frame at a time from gameplay genesis and truncated at the first
  frame whose decoded tuple equals the maximum tuple recorded anywhere in the
  source. Every frame up to and including that one is a control frame. This
  prefix is live by construction — it crosses two recorded tuple transitions —
  and it spans the recorded underground section, which is where a vertical
  threshold is most likely to be wrong.
- Uncontrolled population, fixed before execution: scan active entries at the
  maximum recorded tuple in descending progress bucket, breaking ties by
  `(input, id)`, and admit an entry when its no-input and its held-left
  120-frame continuations produce identical rendered column signatures on every
  frame in common. That identity is model-free evidence that the controller has
  no rendered effect. Admit at most two per bucket and stop at eight, scanning
  at most 128 entries. Each admitted entry is then continued 240 frames with no
  input, and every frame of that continuation is an uncontrolled frame.
- Candidate conditions, fixed before execution and evaluated per frame:
  `K0` is `$000e == $0b`; `K1` is `$075a` strictly below its value at the start
  of the replay; `K2(t)` for each `t` in `1..=7` is `$00b5 >= t`; `K3(t)` for
  each `t` in `1..=7` is `$00b5 * 256 + $00ce >= 256 * t`.
- Acceptance, fixed before execution: a candidate passes when it is true on zero
  control frames and true on at least one frame of every one of the eight
  uncontrolled continuations. The audit reports, per candidate, its true-frame
  count over the controls, the uncontrolled continuations on which it never
  trips, and for passers the median and maximum first-trip frame index.
- Adoption rule, stated now and executed in the separate correction rather than
  here: the correction adopts `K0 or P`, where `P` is the passing candidate with
  the smallest maximum first-trip frame, ties broken toward the earlier
  candidate in the order listed above. Disjoining with `K0` keeps every death
  the frozen condition already detects. If no candidate passes, the audit
  reports itself inconclusive and no correction follows from it.
- The audit additionally records, for every control and uncontrolled frame, the
  raw values of `$000e`, `$075a`, `$00b5`, `$00ce`, the screen page and x bytes,
  and the decoded tuple, so the adopted condition can be rechecked against the
  same evidence later.
- The audit runs no search, changes no search behavior, and involves no model.
  Live and no-model replay reports must be byte-equal. Raw destination:
  `target/smb-completion/d34-death-decode/`.

### D34 result — pass; the frozen condition misses every audited death

- Reading fixed before the numbers below were inspected: "the start of the
  replay" is gameplay genesis in both populations, so `K1` compares against the
  genesis value of `$075a` in the control and in every continuation.
- The control replay consumed all 220 recorded champion actions and 10,006
  frames before reaching the maximum recorded tuple, spanning 2,924 frames at
  tuple `(0, 0)`, 7,080 at `(0, 1)` and 2 at `(0, 2)`. Over all of them `$075a`
  never moved from 2, `$00b5` took only the values 1 and 0 — 9,795 and 211
  frames — and the largest recorded `$00b5 * 256 + $00ce` was 496.
- The uncontrolled scan admitted the first eight entries it tested, ids 4395,
  4396, 4394, 4083, 4056, 4029, 4180 and 4055, at progress 39, 39, 38, 38, 37,
  37, 36 and 36. On every one of them the no-input and held-left continuations
  render identically on all 121 frames in common.
- `K0` is true on zero control frames and trips on **none** of the eight
  continuations. The frozen terminal condition detects none of these deaths.
- `K1` is true on zero control frames and trips on only two of the eight, at
  frames 137 and 132. `$075a` falls from 2 to 1 on ids 4395 and 4396 and does
  not move within 240 frames on the other six.
- `K2(1)` and `K3(1)` are true on 9,795 control frames and fail. `K2(t)` and
  `K3(t)` are identical for every `t` at or above 2, which is a consequence of
  their definitions rather than independent agreement. `K2(2)` through `K2(5)`
  and `K3(2)` through `K3(5)` are true on zero control frames and trip on all
  eight continuations; `K2(6)` trips on two and `K2(7)` on none.
- The registered adoption rule selects **`K2(2)`**, `$00b5 >= 2`: zero control
  frames, first-trip frames `[0, 0, 10, 7, 11, 17, 19, 19]`, median 11 and
  maximum 19. `K3(2)` is the same condition and loses the tie by order.
- The recorded trajectories say what the population is. Ids 4395 and 4396, both
  at the maximal frontier bucket, are already at `$00b5 = 3` at their recorded
  archive endpoint: they are stored states that had already left the play area
  before they were retained. The other six begin at `$00b5 = 1` and reach
  `$00b5 = 5` within 240 frames while `$000e` never leaves 8, a descent of more
  than four pages with the engine state unchanged.
- Limitation recorded plainly: the control is one recorded input. It is 10,006
  frames long and crosses three recorded tuples including the whole second one,
  but a condition that is never true across a single trajectory is weaker
  evidence than one tested across many. The correction below therefore keeps
  `K0` disjoined and states the exposure.
- Live and no-model replay reports are byte-equal with SHA-256
  `ac0fa58e9067d2a394c79814aa7372cea6af3b1a5888ed6c48508d3a3037218e` and the
  summary records `replay_verified=true`. Raw evidence:
  `target/smb-completion/d34-death-decode/`.

## M35 — preregistered corrected terminal condition and frontier rebuild

- Mechanism claim: the target's terminal condition becomes `$000e == $0b` or
  `$00b5 >= 2`, the disjunction D34's registered adoption rule selected. It
  changes when an execution stops and therefore which states are admitted, and
  nothing else. Archive keys, novelty, the parent scheduler, the nine-mask
  controller vocabulary, the stratified duration policy, the one-or-two suffix
  policy, retention, the 512-action bound, and ranking are all unchanged. The
  vertical archive key keeps its recorded form; this registration does not touch
  it.
- Exposure stated plainly: the second clause rests on one 10,006-frame control
  trajectory. Keeping `K0` disjoined means the correction can only add detected
  deaths, never remove one, so a wrong second clause can truncate live play but
  cannot resurrect a death the frozen condition already caught.
- Gates fixed before execution. G1: the frozen condition implies the corrected
  one, as a unit test over both clauses. G2: the recorded champion input replays
  under the corrected target from gameplay genesis to the maximum recorded tuple
  without terminating, in the same 220 actions and 10,006 frames D34 recorded.
  G3: `cargo fmt --check`, `cargo clippy --all-features` with `-D warnings`,
  `cargo nextest run --all-features`, and `cargo deny check` all pass. G4: the
  re-admission pass below is byte-equal across two runs. G5: no entry retained
  by any rebuilt archive has `$00b5 >= 2` at its endpoint.
- Rebuild, part one — re-admission. Every entry of the recorded source archive
  is replayed from gameplay genesis under the corrected target. An entry
  survives when the corrected condition is false on every frame up to and
  including its endpoint. The pass reports the surviving count, the surviving
  count per tuple and per progress bucket, and the maximum surviving
  `(world, level, progress)`, and writes the survivors as an archive report. It
  runs no search and involves no model.
- Rebuild, part two — resume. From the surviving archive, resume the frozen
  protocol unchanged: frozen parent scheduler, nine-mask vocabulary, stratified
  durations, one-or-two suffixes, archive keys and retention, no ranking,
  512-action bound, 5,000 executions, single shortest mechanical frontier input.
  Six arms on the development seeds `0x5eed_e000..=0x5eed_e005`, at `nice -n 10`,
  at most six concurrently.
- Falsifiable claim for part two: the twelve-panel plateau was an artifact of
  undetected deaths, so the six corrected arms will **not** all stop at one
  identical maximum progress bucket at the maximum tuple they reach. Recorded
  control: H20's twelve arms all stopped at exactly progress 39. If the six
  corrected arms again agree exactly, the diagnosis is incomplete and this is
  recorded as such rather than explained away.
- Selection fixed now to prevent post-hoc choice: assignment work continues on
  the arm with the maximum `(world, level, progress)` tuple, ties broken by the
  fewest retained entries and then by the smaller seed.
- Raw destinations: `target/smb-completion/m35-readmission/` and
  `target/smb-completion/m35-rebuild/`.

### M35 result — accepted; the plateau broke on the correction alone

- The corrected terminal condition is `$000e == $0b` or `$00b5 >= 2`, as adopted.
- G1 passes: a unit test holds the frozen clause decisive at every value of the
  vertical page byte, so no death the frozen condition detected is lost.
- G2 passes exactly. Under the corrected condition the recorded champion input
  still replays from gameplay genesis to the maximum recorded tuple in 220
  actions and 10,006 frames, with a largest `$00b5` of 1 and a largest combined
  vertical position of 496 — the same numbers D34 recorded.
- G3 passes: `cargo fmt --check`, `cargo clippy --all-features` with
  `-D warnings`, 69 tests run and 69 passed under `cargo nextest run
  --all-features`, and `cargo deny check` reporting advisories, bans, licenses
  and sources ok. Clippy prints only the known pre-existing configuration
  warning for the removed `rand::thread_rng` path.
- G4 passes: the re-admission pass records `replay_verified=true`.
- G5 passes: re-admitting the selected rebuilt arm returns 2,947 of 2,947
  entries surviving and zero below the play area at any endpoint.
- Re-admission of the recorded source archive: of 4,832 recorded entries, 1,188
  survive and **3,644 were already below the play area at the endpoint at which
  they were retained**. Per tuple the survivors are 235 of 242, 701 of 803, and
  252 of 3,787.
- The per-bucket table names the fault exactly. In the deepest tuple every
  heavily populated bucket is entirely dead: progress 34 keeps 0 of 1,502,
  progress 32 keeps 0 of 229, progress 33 keeps 0 of 94, progress 30 keeps 0 of
  96, progress 29 keeps 0 of 46, progress 19 keeps 0 of 44, and progress 17
  keeps 1 of 257. Progress 39 keeps 1 of 382. The sparse buckets are the live
  ones: 38 keeps 4 of 4, 37 keeps 6 of 6, 36 keeps 3 of 3, 35 keeps 2 of 2. A
  1,502-entry bucket that retains nothing is one fall recorded sixteen vertical
  buckets deep, over and over.
- Resume panel, six arms, frozen protocol, 5,000 executions each. Retained
  counts were `[2947, 2235, 2182, 2091, 2156, 2240]` and deaths were
  `[2777, 3302, 3358, 3431, 3387, 3300]`, against the 45 deaths the same
  protocol recorded on the same source before the correction. Every arm records
  `replay_verified=true`.
- The registered claim is accepted. The six arms did **not** all stop at one
  identical maximum progress: seed `0x5eed_e000` reached
  `(world 0, level 2, progress 74)` while the other five reached progress 39.
  Progress 74 is the first advance past the boundary that held on 12/12 arms in
  H20 and on every arm of H21 through H27. Nothing about the scheduler, the
  controller vocabulary, the durations, the suffixes, the retention rule, the
  budget or the ranking changed; only undetected deaths stopped being admitted.
- The selected arm is `0x5eed_e000`, the unique maximum. Its deepest tuple holds
  a continuous live ladder from progress 47 to 74 — 131 entries at 74, 42 at 71,
  62 at 64, 84 at 49 — where the pre-correction archive had a single live entry
  above progress 38.
- Recorded consequence for the earlier audits: D29 through D34 continued their
  fixed 120- and 240-frame continuations past deaths the frozen condition did
  not detect, so under the corrected condition they cannot reproduce and are not
  expected to. Their recorded reports stand as evidence of what the frozen
  target did, and D34's adoption decision was taken before the correction was
  applied.
- Raw evidence: `target/smb-completion/m35-readmission/`,
  `target/smb-completion/m35-rebuild/`, and
  `target/smb-completion/m35-gate-g5/`.

## D36 — preregistered player-column decode audit at the rebuilt frontier

- Falsifiable claim: one work-RAM index holds the player's horizontal position
  within the rendered screen, and it is identifiable from the program's own film
  and memory evidence once the audited states are alive. D29 through D33 could
  not test this claim because no state they could source responded to the
  controller.
- Source is the M35-selected rebuilt archive
  `target/smb-completion/m35-rebuild/e000/archive-live.json`, whose maximum
  tuple is `(world 0, level 2, progress 74)`, built under the corrected terminal
  condition. The video-enabled target, the three fixed continuations of 120
  single-frame chords with masks `0x00`, `0x01` and `0x02`, the per-frame work
  RAM and 256-column frame signatures, four-byte-stride rejection, lowest-index
  selection, the rendered PNG set, the field name `player_screen_column`, the
  operator sentence fixed in D29, and the byte-equal live-and-replay gate are
  unchanged.
- Audited entries: scan active entries at the maximum tuple in descending
  progress bucket, breaking ties by `(input, id)`, at most two admitted per
  bucket and at most 128 scanned, until eight are admitted. An entry is admitted
  when its held-right continuation advances the recorded camera by at least 32
  pixels **and** its held-right and held-left continuations differ in at least
  one rendered column on at least one frame in common. The second clause is the
  D33 correction: a camera advance alone admits a falling player who coasts,
  while a rendered difference between opposite masks cannot.
- Continuations truncate at the first frame whose recorded camera is below the
  previous frame's, retaining that frame, and at the corrected terminal
  condition.
- Filters, fixed before execution: C0 requires at least eight distinct values
  across all audited frames; C1 requires consecutive recorded frames to differ
  by at most 8; C2 requires the left continuation's final value to be at least 8
  below the endpoint value on at least three quarters of the audited entries,
  rounded up, and to exceed the endpoint value by more than 4 on none.
- Verification is D33's camera-spread rule verbatim. A comparison is a pair of
  continuations of the same entry at the same recorded frame index, present in
  both, with equal screen-page and screen-x bytes and candidate values differing
  by at least 8. With `L` and `H` the lowest and highest differing rendered
  columns and `d` the candidate difference, `offset = L - min(candidate values)`
  and `width = (H - L + 1) - d`. An index passes when some integer `o` in
  `-24..=24` has at least eight comparisons with `|offset - o| <= 6` and `width`
  in `4..=40`, and those agreeing comparisons include two whose recorded camera
  positions differ by at least 16 pixels. The audit records the offset the most
  comparisons agree with, breaking ties toward zero, with its agreeing count and
  camera spread.
- If fewer than eight entries are admitted the audit reports the scan and stops.
  If no index passes it reports itself inconclusive and selects nothing.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d36-player-column-decode/`.

### D36 result — inconclusive; the registered scan rule never left one bucket

- The scan ran to its 128-entry limit without admitting a single entry, so no
  audit was performed and nothing was selected. Recorded per bucket: 119 entries
  scanned at progress 74 and 9 at progress 73, with zero admitted in either.
- The registration is at fault, and the fault is stated rather than repaired in
  place. It capped the number of entries **admitted** per bucket at two but put
  no cap on the number **scanned** per bucket. The rebuilt frontier bucket holds
  131 entries, so a bucket that admits nothing consumes the whole scan budget
  before any other bucket is reached. Only two of the deepest tuple's buckets
  were ever examined.
- A second deficiency shows in the same numbers: the report records only the
  conjunction, so the zero cannot be attributed to the camera-advance clause or
  to the rendered-difference clause. Both are recorded separately below.
- Raw evidence: `target/smb-completion/d36-player-column-decode/`.

## D37 — preregistered corrected scan for the player-column decode audit

- Same falsifiable claim, source, target, continuations, filters, verification,
  field name, operator sentence, and byte-equal live-and-replay gate as D36.
- Corrected scan rule: scan active entries at the maximum tuple in descending
  progress bucket, breaking ties by `(input, id)`, examining at most four
  entries per bucket, admitting at most two per bucket, and examining at most
  128 in total, until eight are admitted. Capping the entries examined per
  bucket is what D36 lacked; it guarantees the scan reaches at least
  thirty-two buckets before exhausting its budget.
- Corrected admission rule: an entry is admitted when its held-right and its
  held-left continuations differ in at least one rendered column on at least one
  frame in common. D36's camera-advance clause is dropped, and the reason is
  stated before the numbers are seen: the audit needs states whose horizontal
  position answers the controller, and a rebuilt archive already contains only
  live states, so requiring the camera to move as well tests reachable terrain
  rather than controllability and can reject exactly the states the audit needs.
  Camera spread across the audited set is supplied by the per-bucket admission
  cap, which forces eight admitted entries to span at least four buckets.
- The scan reports, per bucket, entries examined, entries whose held-right
  continuation advances the recorded camera by at least 32 pixels, entries whose
  opposite-mask continuations differ in a rendered column, and entries admitted.
  The two clauses are recorded separately whatever the outcome.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d37-player-column-decode/`.

### D37 result — inconclusive; no candidate survived the left-direction filter

- The corrected scan worked as registered. It examined 54 entries across sixteen
  progress buckets, recorded 22 whose held-right continuation advances the
  camera and 8 whose opposite-mask continuations differ in a rendered column,
  and admitted those 8, at progress 62, 62, 61, 61, 60, 60, 59 and 59. 150
  indices took at least eight distinct values and 48 were also smooth. **Zero**
  survived filter C2, so nothing reached verification and nothing was selected.
  Live and no-model replay agree and the summary records
  `replay_verified=true`.
- The two clauses separate cleanly and the separation is the finding. Per bucket
  the camera-advancing counts are 0, 0, 4, 4, 1, 2, 1, 0, 1, 1, 2, 4 at progress
  74 down to 63, with zero answering anywhere in that range; at progress 62 down
  to 59 the counts invert to zero advancing and two answering per bucket. No
  entry in the deepest twelve buckets answers the controller at all.
- A diagnosis of the deepest buckets says why, and it is the same fault one
  threshold later. Eight entries sampled at progress 74, 73, 72 and 63 record
  continuations lasting 2, 2, 4, 8, 10, 14, 16, 22 and 25 frames before the
  corrected condition stops them, with `$00ce` already at 176 to 253 on page 1
  and rising every frame, identical trajectories under all three masks, and the
  camera advancing 1161 to 1199 while it happens. **Those buckets are falls in
  flight.** The player runs off an edge, keeps travelling right while falling,
  and the camera scrolls with him, so an action boundary that lands in that
  window is retained at a progress bucket no live state ever reached.
- The corrected condition stops such a run within a few frames, but it cannot
  stop it before the boundary, because a vertical threshold that admits recorded
  live play — which reaches `$00ce` 240 on page 1 — necessarily also admits the
  first part of a fall. The live frontier of the rebuilt archive is therefore
  progress 62, not 74. That is still an advance of 23 buckets over the boundary
  that held from H20 through H27, and it is the number the standing summary
  should carry.
- The eight admitted entries were then measured directly. On four of them, ids
  1546, 2051, 1951 and 1770, the no-input and held-left continuations render
  identically on all 121 frames and every candidate index ends where it started:
  the player is pinned and cannot move left at all. Those four were admitted
  only because held-right differs from held-left somewhere, which a one-frame
  difference satisfies. On the other four the recorded horizontal excursion
  between the held-left and held-right endpoints is 5, 6, 42 and 6 units.
- The index that behaves like a screen-relative horizontal position is already
  visible in this evidence: index 134 ends at `[80, 80, 82, 79, 76, 76, 40, 74]`
  under held left and `[80, 80, 82, 79, 81, 82, 82, 80]` under held right, lower
  under left on every entry that moves at all and equal on every entry that does
  not. Indices 1820, 1821 and 1855 carry the same quantity, and index 587 and
  its stride-4 relatives carry its mirror. This is suggestive and is **not** a
  selection: D29's verification requires candidate differences of at least 8 at
  equal camera, and only one of the eight entries produces one.
- The filter that rejected everything is C2, which compares the held-left
  endpoint against the entry's own starting value. That comparison is confounded
  by momentum — on entry 1762 the no-input continuation drifts further left than
  the held-left continuation does — and by pinning. The contrast the film rule
  actually needs is between the two opposite masks at the same frame.
- Raw evidence: `target/smb-completion/d37-player-column-decode/`,
  `target/smb-completion/d37-diagnosis/`,
  `target/smb-completion/d37-deep-diagnosis/` and
  `target/smb-completion/d37-film-columns/`.

## D38 — preregistered responsiveness-sourced player-column decode audit

- Same falsifiable claim, source archive, video-enabled target, three fixed
  continuations of 120 single-frame chords with masks `0x00`, `0x01` and `0x02`,
  camera-decrease truncation, filter C0, filter C1, film verification with the
  camera-spread requirement, four-byte-stride rejection, lowest-index selection,
  rendered PNG set, field name `player_screen_column`, operator sentence, and
  byte-equal live-and-replay gate as D37.
- Corrected sourcing. For every active entry at the maximum tuple, in descending
  progress bucket with ties by `(input, id)`, examining at most eight per bucket
  and at most 256 in total, record the number of frames on which the held-right
  and held-left continuations differ in at least one rendered column. Call it
  the entry's responsive frames. Admit entries in descending responsive frames,
  breaking ties by descending progress and then `(input, id)`, at most two per
  progress bucket, requiring at least 60 responsive frames, until eight are
  admitted. Ranking by responsive frames is not circular: it is a rendered
  measurement that names no work-RAM index.
- The reason, stated before the numbers are seen: D37 established that the
  deepest buckets of this archive are falls in flight and that four of its eight
  admitted entries were pinned against terrain and could not move at all. An
  audit that must observe horizontal motion has to source the states that have
  some, and depth is not what supplies it. Camera spread across the audited set
  is still supplied by the per-bucket admission cap.
- Corrected filter C2. For at least three quarters of the audited entries,
  rounded up, the held-left continuation's final value is at most the held-right
  continuation's final value minus 8, and on no audited entry does it exceed the
  held-right final value by more than 4. This replaces D29's comparison against
  the entry's own starting value, which D37 showed is confounded by momentum and
  by pinning. The threshold of 8 is not free: the film rule only forms
  comparisons where the candidate values differ by at least 8, so a candidate
  that never reaches that separation could not be verified in any case.
- The scan reports, per bucket, entries examined, their responsive-frame counts,
  and entries admitted. If fewer than eight entries reach 60 responsive frames
  the audit reports the scan and stops. If no index passes verification it
  reports itself inconclusive and selects nothing.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d38-player-column-decode/`.

### D38 result — inconclusive; the responsiveness measure ranks facing, not motion

- The scan examined 256 entries across the deepest buckets, found 45 reaching 60
  responsive frames, and admitted eight: ids 1845, 2223, 1609, 2222, 1654, 1898,
  1721 and 1746, at progress 60, 59, 59, 58, 58, 57, 56 and 56. 152 indices took
  at least eight distinct values and 25 were also smooth. Zero survived the
  corrected direction filter, so nothing reached verification and nothing was
  selected. Live and no-model replay agree and the summary records
  `replay_verified=true`.
- The measure saturated, which is the giveaway. Every one of the top fourteen
  entries scored exactly 119 responsive frames out of 121 — the maximum a
  121-frame continuation can reach. A measure that assigns the same maximum to
  every candidate is not ranking them.
- The reason is recorded in the endpoints. On all eight admitted entries the
  held-left and held-right continuations end with **identical** work RAM at every
  smooth candidate index. The continuations render differently on 119 frames and
  finish in the same state. What the two masks changed was the direction the
  player is drawn facing, which repaints his sprite on almost every frame while
  moving him nowhere. Counting frames that differ therefore ranks pinned states
  highest, which is the opposite of what the audit needs.
- A second, structural weakness is visible in the same run. C0 and C1 are
  conjunctive across the whole audited set, so one heterogeneous entry removes a
  candidate for all of them: index 134, which D37 recorded behaving like a
  screen-relative horizontal position, is absent from this run's 25 smooth
  survivors because a single audited entry steps it by more than 8 in one frame.
- The measurement that separates facing from motion is already available in the
  same film evidence and names no work-RAM index: the **width of the differing
  column span**. A facing flip repaints one sprite, so its span is about one
  sprite wide; two players genuinely apart produce a span of their separation
  plus a sprite width.
- Raw evidence: `target/smb-completion/d38-player-column-decode/` and
  `target/smb-completion/d38-diagnosis/`.

## D39 — preregistered span-sourced player-column decode audit

- Same falsifiable claim, source archive, video-enabled target, continuations,
  camera-decrease truncation, filters C0 and C1, opposite-mask direction filter
  C2, film verification with the camera-spread requirement, four-byte-stride
  rejection, lowest-index selection, rendered PNG set, field name, operator
  sentence, and byte-equal live-and-replay gate as D38.
- The single change is the sourcing measure. For every examined entry, take the
  frames on which the held-right and held-left continuations differ in a
  rendered column, discard those whose differing span exceeds 128 columns
  because a half-screen difference is a scroll or an unrelated actor rather than
  the player, and record the largest remaining span. Admit entries in descending
  largest span, breaking ties by descending progress and then `(input, id)`, at
  most two per progress bucket, requiring a largest span of at least 24, until
  eight are admitted. Scanning is unchanged at eight per bucket and 256 in
  total.
- Twenty-four is not a free parameter. The film rule only forms comparisons
  where the candidate values differ by at least 8 and accepts a width in
  `4..=40`, so a usable comparison has a span of at least 8 plus 4. Requiring
  24 asks for a separation clear of one sprite width.
- The scan reports every examined entry's largest span alongside its responsive
  frames, so the two measures can be compared directly. If fewer than eight
  entries reach a span of 24 the audit reports the scan and stops. If no index
  passes verification it reports itself inconclusive and selects nothing.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d39-player-column-decode/`.

### D39 result — stopped; only two of 256 states separate the player at all

- The scan examined 256 entries across 36 progress buckets from 74 down to 33.
  Only **2** reached a largest differing span of 24, so the audit stopped as
  registered without auditing anything.
- The recorded span distribution is bimodal and says exactly what the measure
  was built to say. 176 entries record a largest span of 0, 63 record exactly
  17, eight record 16, five record 13, one records 14 and one 18. Two exceed the
  threshold: id 1610 at progress 40 with span 98, and id 1871 at progress 59
  with span 60. Seventeen columns is one sprite width. The 63-entry spike at 17
  is the facing flip D38 identified, now measured directly and separated from
  motion.
- The measure has a recorded confound and it matters. Frames whose differing
  span exceeds 128 columns are discarded as scroll or unrelated actors, and a
  span of 0 therefore means either that nothing differed on any frame or that
  everything that differed was screen-wide. Id 1526 records 85 responsive frames
  and a largest span of 0, so for that entry every differing frame was
  screen-wide.
- The cause is visible by comparing against D37. Id 2223 recorded a 42-unit
  separation between its held-left and held-right endpoints at index 134, which
  should span about 58 columns, yet this scan records 17 for it. The frames on
  which the two continuations are genuinely apart are exactly the frames on
  which their **cameras** have also diverged, and a camera difference shifts the
  whole screen. The film verification already handles this by comparing only
  frames whose recorded camera bytes are equal; the sourcing measure did not.
- Raw evidence: `target/smb-completion/d39-player-column-decode/`.

## D40 — preregistered equal-camera span sourcing

- Same falsifiable claim, source archive, target, continuations, filters, film
  verification, stride rejection, selection, field name, operator sentence, and
  byte-equal live-and-replay gate as D39.
- The single change: the largest differing span is measured only over frames on
  which the two continuations record **equal screen-page and screen-x bytes**.
  That is the same condition the film verification already imposes on every
  comparison it forms, so sourcing and verification now agree about which frames
  carry usable evidence. The 128-column ceiling is kept and the count of frames
  it discards is reported separately from the count of frames with no difference
  at all, so a span of 0 is no longer ambiguous.
- Admission is otherwise unchanged: descending largest span, ties by descending
  progress and then `(input, id)`, at most two per progress bucket, requiring at
  least 24, until eight are admitted; eight examined per bucket and 256 in
  total.
- If fewer than eight entries reach a span of 24 the audit reports the scan and
  stops. If no index passes verification it reports itself inconclusive and
  selects nothing. It runs no search, changes no search behavior, and involves
  no model. Raw destination:
  `target/smb-completion/d40-player-column-decode/`.

### D40 result — stopped; the retained frontier has almost no horizontal freedom

- Restricting the measure to equal-camera frames removed the confound and did
  not change the answer. Of 256 entries examined across the same 36 progress
  buckets, **2** reach a largest span of 24, so the audit stopped as registered.
- The ambiguity D39 recorded is gone. Wide frames are now counted separately and
  are almost absent: 240 entries record none, fifteen record one, one records
  two. Every one of the 183 entries with a largest span of 0 also records zero
  responsive frames, so a zero now means what it says — at equal camera the two
  opposite masks render the player identically on every frame.
- The distribution is the finding. 183 of 256 states do not answer the
  controller at all. 62 answer with a span of exactly 17 and eight with 16 —
  one sprite width, the player turning to face the other way without moving.
  One records 18. Only id 1610 at progress 40, with span 98, and id 1871 at
  progress 59, with span 60, put the player anywhere.
- Stated plainly: at the deepest buckets of the rebuilt archive the player is
  almost always either falling or wedged. Assignment 1(b)'s premise was that
  ranking could not prefer a nearer state because the field recording horizontal
  position does not exist. The field is still missing and D37's evidence still
  points at index 134, but the measurement says the field is not what is
  limiting these states: 73 of 256 answer the controller at all, and 2 of 256
  can move.
- Raw evidence: `target/smb-completion/d40-player-column-decode/`.

### Standing summary after the terminal-condition correction

- The frozen terminal condition missed the fall-into-gap death entirely. D34
  established it on eight recorded continuations, none of which the frozen
  condition detects, and adopted `$00b5 >= 2` beside it under a rule fixed in
  advance. M35 applied it with all five gates green.
- Re-admitting the recorded archive under the corrected condition kept 1,188 of
  4,832 entries and found 3,644 already below the play area at the endpoint at
  which they had been retained. Every heavily populated bucket in the deepest
  tuple kept nothing.
- Rebuilding with the frozen protocol broke the boundary that had held on every
  arm from H20 through H27. Deaths rose from 45 to about 3,300 per 5,000
  executions and one of six arms reached progress 74 where twelve consecutive
  arms had stopped at exactly 39. Nothing but the death condition changed.
- The live frontier is progress 62, not 74. Progress 63 through 74 are falls in
  flight: the player leaves an edge, keeps travelling right while falling, and
  the camera scrolls with him, so an action boundary landing in that window is
  retained at a bucket no live state reached. A vertical threshold cannot close
  that window, because recorded live play reaches `$00ce` 240 on page 1 and a
  fall passes through the same values on its way down.
- Below that, the retained states are wedged. Of 256 live states examined from
  progress 74 down to 33, 183 do not answer the controller at all, 70 answer
  only by changing which way the player faces, and 2 move him more than one
  sprite width.
- Both of those are properties of what the archive **retains**, not of what the
  observation state records. Correcting them is a search change of the same kind
  as the terminal condition, so it is reported rather than taken.

## D41 — preregistered two-state film-discriminated player-column audit

- Falsifiable claim: one work-RAM index holds the player's horizontal position
  within the rendered screen, and D33's film rule can identify it from the only
  two recorded states in which the controller moves the player at all. D37's
  evidence points at index 134; this audit is registered to confirm or refute
  that, and it selects by its own rule rather than by that expectation.
- Audited entries are not chosen here. They are whatever D40's registered scan
  admits when asked for two: the same examination order, the same eight per
  bucket and 256 in total, the same equal-camera span measure, the same
  descending-span admission with at most two per progress bucket and a minimum
  span of 24. On the recorded rebuilt archive that scan admits ids 1610 at
  progress 40 and 1871 at progress 59, and the audit fails rather than proceeds
  if it admits fewer than two.
- Everything else is D38's: the video-enabled target, three fixed continuations
  of 120 single-frame chords with masks `0x00`, `0x01` and `0x02`,
  camera-decrease truncation, filter C0 requiring at least eight distinct
  values, filter C1 requiring consecutive frames to differ by at most 8, filter
  C2 contrasting the two opposite masks at the same frame, four-byte-stride
  rejection, lowest-index selection, the rendered PNG set, the field name
  `player_screen_column`, the operator sentence fixed in D29, and the byte-equal
  live-and-replay gate.
- With two audited entries, C2's three-quarters threshold rounds to two, so both
  entries must show the held-left final value at least 8 below the held-right
  final value, and neither may exceed it by more than 4.
- The weakness of a two-entry set is stated plainly: C0 and C1 discriminate less
  with fewer recordings, so more indices will reach verification than in an
  eight-entry audit. The film rule is what carries the audit, and it is not
  weakened. It still requires at least eight comparisons agreeing on one offset
  within a tolerance of 6 with a width in `4..=40`, and it still requires two of
  those agreeing comparisons to come from recorded camera positions at least 16
  pixels apart. The two admitted entries sit at camera positions hundreds of
  pixels apart, so that requirement can only be met by comparisons drawn from
  both of them — which is precisely the discriminator that separates a
  screen-relative byte from an absolute one.
- The audit reports every index that passes the film rule, not only the selected
  one, so whether the selection is unique is visible in the record.
- The audit runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d41-player-column-decode/`.

### D41 result — inconclusive; one audited entry dies under held left

- The scan admitted exactly the two entries the registration named, ids 1610 at
  progress 40 and 1871 at progress 59. 170 indices took at least eight distinct
  values and 24 were also smooth. Zero survived filter C2, so nothing reached
  the film rule and nothing was selected. Live and no-model replay agree and the
  summary records `replay_verified=true`.
- The recorded continuation lengths say why. On id 1610 the no-input and
  held-left continuations both stop after 105 frames while the held-right
  continuation runs the full 121: holding left walks that player off an edge and
  the corrected terminal condition stops him. C2 compares the two masks at their
  **final** recorded frames, so on that entry it compares a falling player
  against a walking one, and no index can satisfy it.
- The evidence that survives the confound is worth recording. Index 953 ends at
  108 under held left and 184 under held right on id 1871, a separation of 76
  columns with the polarity a screen-relative horizontal position would have. On
  id 1610 the same index ends at 222 against 227, five apart, because the
  held-left value is the value at the moment of death. One entry is not a
  verification and this is not a selection.
- The frame the audit should compare at is already computed. D40's sourcing
  measure records, for every entry, the frame of largest equal-camera differing
  span — the moment of maximum separation, at equal camera, with both
  continuations still running. That is the same frame the film rule would draw
  its comparison from.
- Raw evidence: `target/smb-completion/d41-player-column-decode/` and
  `target/smb-completion/d41-diagnosis/`.

## D42 — preregistered maximum-separation direction filter

- Same falsifiable claim, source archive, admitted entries, target,
  continuations, filters C0 and C1, film verification with the camera-spread
  requirement, four-byte-stride rejection, lowest-index selection, rendered PNG
  set, field name, operator sentence, and byte-equal live-and-replay gate as
  D41.
- The single change: filter C2 is evaluated at each entry's frame of largest
  equal-camera differing span rather than at the final recorded frame. At that
  frame both continuations are still running, their recorded cameras are equal,
  and the player is as far apart as the controller ever puts him. The rule
  itself is unchanged — for at least three quarters of the audited entries,
  rounded up, the held-left value is at most the held-right value minus 8, and
  on no entry does it exceed the held-right value by more than 4 — and with two
  entries that means both.
- The reason is stated before the numbers are seen: D41 recorded that a
  continuation which ends in death makes its final frame meaningless for a
  positional contrast, and the maximum-separation frame is both live and
  equal-camera by construction. It is also the frame the film rule already
  prefers, so C2 and verification now test the same evidence.
- The audit reports every index that passes the film rule, not only the selected
  one. It runs no search, changes no search behavior, and involves no model.
  Raw destination: `target/smb-completion/d42-player-column-decode/`.

### D42 result — inconclusive; neither audited entry separates the player

- The audit ran on the same two admitted entries. 170 indices took at least
  eight distinct values and 24 were also smooth. Zero survived filter C2 at the
  maximum-separation frame, so nothing reached the film rule and nothing was
  selected. Live and no-model replay agree and the summary records
  `replay_verified=true`. Index 134 is neither confirmed nor refuted.
- Moving C2 to the maximum-separation frame removed the death confound and
  exposed a deeper one. The two entries' maximum-separation frames are frame 36
  for id 1610, spanning 98 columns from 40 to 137, and frame 120 for id 1871,
  spanning 60 columns from 1 to 60.
- On id 1610 at frame 36 **every** smooth candidate index holds the same value
  in both continuations — index 953 reads 150 under both masks. The two frames
  differ across 98 columns while no smooth, distinct work-RAM index differs at
  all, so whatever is repainted there is not tracked by any candidate the audit
  can verify.
- On id 1871 at frame 120 the differing columns are 1 through 60, at the far
  left of the screen, while index 953 reads 108 under held left and 184 under
  held right. The one candidate with a large, correctly-signed contrast places
  the player nowhere near the columns that actually differ. The 60-column
  difference is not the player.
- The conclusion is the honest one and it is about the archive, not the method.
  The rebuilt archive contains no pair of continuations in which the controller
  visibly moves the player far enough, at an equal camera, for the film rule to
  identify a horizontal-position byte. D40 measured that directly — 2 of 256
  states separate by more than one sprite width — and D42 shows that both of
  those two separate something other than the player.
- Assignment 1(b) closes without a verified field. The candidate index 134
  remains recorded as suggestive from D37 and unverified. Nothing was added to
  the decoded observation state or to either operator view, so the search,
  the archive keys, the novelty map and the scheduler are untouched.
- Raw evidence: `target/smb-completion/d42-player-column-decode/` and
  `target/smb-completion/d42-diagnosis/`.

## M43 — preregistered context amendment for the corrected terminal condition

- H27's recorded view and result remain immutable, as does the M16 amendment.
  Before the next instrumentor call, both model views must state the terminal
  condition the target actually applies, because the recorded one is now wrong.
- The verified-dynamics text currently says a run ends at the first observed
  player-engine kill state. M35 replaced that with a disjunction, so the
  sentence is amended to state both clauses as decode facts: a run ends at the
  first frame whose player-engine byte holds the verified kill value **or**
  whose vertical page byte is at or above 2, and the second clause was adopted
  because a recorded audit found a death mode the first clause detects on none
  of eight recorded uncontrolled continuations, while the second is false on all
  10,006 frames of a recorded live control. The `decoded.dead` field sentence is
  amended to match.
- This is decode polarity and terminal semantics, not route knowledge. It names
  no game, no route, no layout, no item, and suggests no artifact field, score
  term, action, or goal.
- The next strategy journal is re-seeded from the record. It retains the M16
  amendment's direction correction and H27's immutable null, removes the
  superseded belief that the earlier viability audit found few doomed frontier
  entries, and adds, in plain mechanical language: that the terminal condition
  was corrected and what it measured; that re-admitting the previous archive
  kept 1,188 of 4,832 entries with 3,644 already past the terminal threshold
  when retained; that rebuilding under the corrected condition raised deaths
  from 45 to about 3,300 per 5,000 executions and produced one arm of six
  reaching a bucket twenty-three past the boundary twelve earlier arms shared;
  that the buckets above the live boundary are states the corrected condition
  stops within a few frames; and that of 256 examined retained states, 183 show
  no rendered response to the controller at all, 70 respond only by changing the
  direction the player is drawn facing, and 2 move him more than one sprite
  width. It also records that a registered attempt to decode a screen-relative
  horizontal position from this archive returned no verified field.
- No suggested ranking term, field, action, route, layout, item or goal is added.
  The observations-over-expectations sentence stays verbatim.
- Acceptance: the amended sentences are present and mechanically checked by the
  existing focused fixture, which also still rejects game names and verifies the
  complete field list; the recorded-journal chain still replays without a model;
  and `cargo fmt --check`, `cargo clippy --all-features` with `-D warnings`,
  `cargo nextest run --all-features` and `cargo deny check` all pass. A
  standalone no-model record of the corrected context and re-seeded journal is
  written. No model call and no search panel are part of M43.
- Raw destination: `target/smb-completion/post-m35-model-context/`.

### M43 result — accepted

- Both model views now state the corrected terminal condition. The
  verified-dynamics text says a run ends at the first frame whose player-engine
  byte holds the verified kill state or whose recorded vertical page byte is at
  or above 2, and records why the second clause was added: the first fires on
  none of the eight recorded uncontrolled continuations, while the second is
  false on all 10,006 frames of the recorded live control and true within 19
  frames on every one of those continuations. The `decoded.dead` sentence states
  the same disjunction.
- The next strategy journal carries seven beliefs and four failed approaches. It
  keeps the M16 direction correction and H27's immutable null, drops the
  superseded viability belief, and adds the correction and its measured effects,
  the re-admission counts, the rebuilt-arm outcome, the character of the highest
  buckets, the 183/70/2 controller-response census, and the recorded fact that a
  registered attempt to decode a screen-relative horizontal position returned no
  verified field. No ranking term, field, action, route, layout, item or goal is
  suggested anywhere in either view or the journal.
- The focused context fixture passes, including its rejection of game names and
  its check of the complete field list, and the recorded-journal chain still
  replays without a model. All 69 tests pass, and formatting, Clippy with
  `-D warnings` and the dependency check are clean.
- The standalone no-model record is
  `target/smb-completion/post-m35-model-context/`. Its field-semantics SHA-256
  is `869d7deea161c057a846614c3c469e7bbf71aa6dc02de7fad9c2f99dd012a479`, its
  verified-dynamics SHA-256 is
  `524fc2992b55c56d60e8f1e5b93050de8de0c4fc55c3164aa0afa863e29ec65d`, and its
  journal SHA-256 is
  `834c6dd144ea9582d766be1ae2f23243e7cc54cef8cdc5e08e4379a0e835dfdb`.

## H44 — preregistered journal-informed ranking at the rebuilt frontier

- Falsifiable claim: with the corrected terminal condition, the corrected field
  semantics and dynamics, and the re-seeded journal, the instrumentor can infer a
  non-progress within-cell ranking that preserves states better prepared to
  produce descendant novelty beyond the rebuilt frontier.
- Source is the M35-selected rebuilt archive
  `target/smb-completion/m35-rebuild/e000/archive-live.json`, seed
  `0x5eed_e000`, built under the corrected terminal condition. Film evidence is
  generated from that same archive by the existing frontier film path and
  supplied as the recorded manifest and video. The operator view contains M43's
  corrected context and re-seeded journal and no suggested ranking term, field,
  action, route, layout, item or goal.
- H27's protocol is otherwise unchanged: one ranking invocation through the
  existing decision schema with at most three compile and fixture attempts;
  the existing pure and deterministic source checks with progress terms still
  forbidden; observation fixtures; the fixed seed `0x5eed_ef00` 256-execution
  isolation pilot and its exact replay; the recorded-journal chain replay;
  development controls and ranking arms on seeds
  `0x5eed_e000..=0x5eed_e005` at 5,000 executions each; the frozen scheduler,
  nine-mask vocabulary, stratified durations, one-or-two suffix and 512-action
  bound; the ranking consulted only for full-cell replacement with fewer actions
  as the final tie-breaker; no other generated artifact; no arm at or above
  20,000 executions; at most six arms concurrent.
- Exposure stated before execution. The source's maximum recorded tuple is a
  fall in flight, and the unchanged protocol selects the shortest input at that
  tuple as the resume input, so both controls and ranking arms begin from a
  prefix the corrected condition stops within a few frames. Bootstrapping that
  prefix still inserts every live action boundary along it, so the archive is
  not empty of live states, but the frontier scheduler will still favour the
  buckets that are falls. This is the fenced retention question showing up
  inside the panel; the protocol is not altered to hide it.
- Acceptance is measured on viable progress rather than recorded progress,
  because recorded progress counts falls in flight. Viable progress is the
  deepest bucket at the maximum tuple holding a state whose no-input
  continuation survives 120 frames, measured by examining at most eight entries
  per bucket in descending order and stopping at the first bucket with one. The
  same measurement is applied to the source archive and to every arm's archive.
  It is a measurement only and changes nothing the search does.
- Acceptance requires the ranking arms to exceed the source archive's viable
  progress on at least 4/6 development seeds, with strictly more successes than
  the controls, and no arm regressing below the deepest recorded tuple. Only
  then repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` with the
  same threshold. Any promotion must replay exactly from recorded seed,
  observations, generated files, labels and journals with no model. Otherwise
  record the ranking as a registered null.
- Raw destination: `target/smb-completion/h44-luna/` and the film evidence at
  `target/smb-completion/h44-frontier-film/`.

### H44 result — ranking rejected; the corrected condition kept paying without it

- Source viability, measured before the panel: the rebuilt archive's recorded
  maximum bucket is 74 and its viable progress is **62**. Buckets 74 down to 63
  hold zero viable states among eight examined each, so the registered
  acceptance baseline is 62.
- The instrumentor returned a usable artifact on its first attempt: an
  installed ranking named `vertical_activity_ranking` scoring bounded ordinary
  engine state, absolute vertical bucket movement, and distinct changed work-RAM
  addresses, penalising terminal observations. Its source SHA-256 is
  `3b6641fff9527b7b502c98c1d8bfa43294f5e04895ee6ac57096e11a8e387b49`. It passed
  the deterministic source checks with progress terms still forbidden, the
  observation fixtures, the fixed seed `0x5eed_ef00` 256-execution isolation
  pilot with its exact replay, and the recorded-journal chain replay. Its
  166-word journal passed without compression.
- Across the panel the ranking stayed active and made `[97, 80, 57, 73, 86, 54]`
  replacements, which produced `[129, 22, 19, 35, 67, 7]` descendant novelties.
  The seed-`0x5eed_e000` ranking arm and its no-model replay are byte-equal with
  SHA-256 `37296fd9089c289af5d399425f691b1375d98b0420d4307ebc0f2accab038d6e`,
  and the report records `replay_verified=true`.
- Viable progress by arm. Controls: `[94, 62, 108, 61, 112, 61]`. Ranking arms:
  `[94, 62, 65, 62, 144, 62]`. Against the baseline of 62 the controls succeed
  on 3/6 and the ranking arms on 3/6.
- **Decision: reject.** The registration required at least 4/6 ranking successes
  and strictly more than the controls; the panel delivered 3/6 and a tie. No
  held-out panel and no promotion are due. The single best arm in the whole
  panel is a ranking arm at viable progress 144, and one arm is not the
  threshold.
- The controls carry the larger result. Resuming the same frozen protocol from
  the rebuilt archive, with no generated artifact at all, raised viable progress
  from 62 to 94, 108 and 112 on three of six seeds. The correction is still
  paying out on its own two panels later.
- The outcome is bimodal in both groups. Three control arms and three ranking
  arms stayed at viable 61 or 62 with a recorded maximum of exactly 74 — the
  source's own fall-in-flight bucket — while the others reached 94 and beyond.
  An arm either leaves the source frontier or it does not move at all, which is
  what the fenced retention question predicts: every arm resumes from a prefix
  the corrected condition stops within a few frames, and whether an arm escapes
  depends on the sampling that follows.
- Recorded progress overstates all of this and the gap is now large: the
  controls record 99, 108 and 123 where they are viable at 94, 108 and 112, and
  the best ranking arm records 144 and is viable at 144. Reporting the recorded
  maximum alone would have claimed advances that are partly falls.
- Raw evidence: `target/smb-completion/h44-luna/`,
  `target/smb-completion/h44-viability/`,
  `target/smb-completion/h44-luna-source-viability.json` and the film at
  `target/smb-completion/h44-frontier-film/`. The panel report is 715 MB
  because it embeds all thirteen archives.

### Standing summary after the ranking panel

- Viable progress is now the number that matters, and it is defined here: the
  deepest bucket at the maximum tuple holding a state whose no-input
  continuation survives 120 frames, measured over at most eight entries per
  bucket. Recorded progress counts falls in flight and overstates the frontier;
  the two differ by 5 to 11 buckets on the arms measured so far.
- The boundary history in viable terms: progress 39 held on every arm from H20
  through H27 under the frozen terminal condition. Correcting that condition and
  rebuilding gave viable 62. Resuming once more from the rebuilt archive gave
  viable 94, 108 and 112 on three of six control seeds and 144 on one ranking
  seed. Nothing in the scheduler, controller vocabulary, durations, suffixes,
  retention rule or budget changed across any of it.
- Two registered generated rankings have now been measured at two different
  boundaries and neither met its promotion rule. H27's failed 0/6 at a boundary
  that was an artifact; H44's tied its controls at 3/6 at a real one.
- The unresolved item is unchanged and still fenced: the archive retains states
  the corrected terminal condition stops a few frames later, they occupy the
  deepest buckets, and every resumed arm therefore starts from one. Half the
  arms in H44 never left it. A viability test at admission is the registered
  next hypothesis.

## H45 — preregistered viability test at admission

- Motivating evidence, recorded before this registration. The archive retains
  states the corrected terminal condition stops a few frames later. D37 sampled
  the rebuilt archive's deepest buckets and recorded continuations lasting 2, 2,
  4, 8, 10, 14, 16, 22 and 25 frames under every mask. H44 measured the source
  archive's viable progress at 62 against a recorded maximum of 74, with zero
  viable states among eight examined in each of buckets 74 down to 63. And H44's
  own arms came out bimodal: three of six controls and three of six ranking arms
  never left the source frontier, finishing at viable 61 or 62 with a recorded
  maximum of exactly 74 — the source's own fall-in-flight bucket — while the
  others reached 94, 108, 112 and 144. Every arm resumes from a prefix the
  corrected condition stops within a few frames, and whether an arm escapes it
  is left to sampling.
- Falsifiable claim: refusing to retain a state that no fixed continuation can
  keep alive removes those prefixes from the archive, so arms stop stalling at
  the source frontier. Challenger arms will exceed the source archive's viable
  progress of 62 on at least 5 of 6 development seeds, with strictly more
  successes than the controls.
- Mechanism, and it is the only variable. Before a candidate snapshot is
  admitted, the target is probed from that snapshot with each of the fixed masks
  `0x00`, `0x01` and `0x81` for at most 120 frames, stopping at the first mask
  that survives the corrected terminal condition. The candidate is admitted only
  if some mask survives, and the snapshot is restored exactly afterwards so
  execution continues unchanged. The probe emits no observations, consumes no
  randomness, and is applied identically in the bootstrap walk and the suffix
  loop.
- Three masks rather than one, stated as a design choice with its reason: a
  single no-input probe would also reject every state that survives only by
  acting immediately, which is exactly the kind of state the search needs. Doing
  nothing, holding right, and the recorded button-plus-right mask cover the
  neutral case, the forward case and the case that leaves the ground. A state is
  refused only when all three die.
- Everything else is unchanged: archive keys, novelty, the frozen parent
  scheduler, the nine-mask controller vocabulary, the stratified duration
  policy, the one-or-two suffix policy, capacity-two cells with fewer-actions
  replacement, the 512-action bound, no ranking and no other generated artifact.
  The vertical-page key term stays fenced.
- Source is the same M35-selected rebuilt archive
  `target/smb-completion/m35-rebuild/e000/archive-live.json` with the same single
  shortest mechanical frontier input. Controls run the frozen retention rule and
  challengers run the probe, on seeds `0x5eed_e000..=0x5eed_e005`, 5,000
  executions each, at `nice -n 10`, at most six arms concurrently, no arm at or
  above 20,000 executions.
- Acceptance is measured with H44's registered viable-progress measurement,
  unchanged: the deepest bucket at the maximum tuple holding a state whose
  no-input continuation survives 120 frames, examining at most eight entries per
  bucket. Acceptance requires challenger viable progress above 62 on at least
  5/6 seeds and strictly more successes than the controls. Only then repeat
  unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105` with the same
  threshold; a promotion must replay exactly with no model.
- The shared clause is declared rather than hidden: the admission probe and the
  acceptance measurement both ask whether a state survives 120 frames, and the
  probe's first mask is the measurement's only mask. A challenger archive is
  therefore expected to score better on the measurement partly by construction.
  What the claim is really about is distance, so the panel additionally reports
  each arm's recorded maximum progress and the gap between recorded and viable.
  If the challengers clear the threshold without also travelling further in
  recorded terms, that is recorded as a weak result rather than an acceptance.
- One challenger arm replays exactly, and its live and replay reports must be
  byte-equal. Raw destination: `target/smb-completion/h45-viability/`.

### H45 result — accepted on development and held-out seeds

- Gates first. The frozen retention path reproduces the recorded M35 seed
  `0x5eed_e000` arm byte for byte after the change, SHA-256
  `40bf875ca4e43f82042855549c50501cf42bd2e4a8346b466a48693f9c0281ff`, so the
  probe is inert when it is not asked for. A unit test holds the two policies
  byte-identical on a target whose terminal condition never fires and holds the
  probed policy identical across repeated runs. The six frozen control arms are
  byte-identical to H44's six control arms at the same seeds, which
  independently confirms the two campaign paths agree. All four quality gates
  pass with 70 tests.
- Development panel, viable progress against the source's 62. Controls
  `[94, 62, 108, 61, 112, 61]`, three successes. Challengers
  `[102, 114, 97, 107, 94, 99]`, **six successes**. The registration required at
  least 5/6 and strictly more than the controls, so development accepts.
- Held-out panel on seeds `0x5eed_e100..=0x5eed_e105`, run unchanged. Controls
  `[89, 74, 91, 64, 62, 112]`, five successes. Challengers
  `[94, 93, 97, 105, 94, 112]`, **six successes**. The held-out threshold is met
  as well, so the mechanism is accepted.
- What it actually did, stated carefully: it removed the stall, not the ceiling.
  The worst arm across all twelve challengers is viable 93; the worst across all
  twelve controls is 61. Challenger medians are 100.5 and 95.5 against control
  medians of 78 and 81.5. But on two development seeds the challenger scored
  **below** its own control — 97 against 108 and 94 against 112 — and on
  held-out seed `0x5eed_e105` the two tie at 112. The probe reliably prevents an
  arm from being trapped at the source frontier; it does not reliably make a
  good arm better.
- The declared shared clause did not carry the result. Recorded maximum progress
  moved with viable progress rather than apart from it: ten of the twelve
  challenger arms record exactly their viable bucket and the other two are
  within 2, while the controls run 5 to 21 buckets ahead of their viable figure
  — control `0x5eed_e103` records 85 and is viable at 64. On the three
  development seeds where the controls stalled, the challengers advanced in
  recorded terms too, from exactly 74 to 114, 107 and 101. That is the claim
  measured on the axis the shared clause does not touch.
- Mechanical accounting. Deaths per 5,000 executions fell from about 2,500 to
  3,400 down to about 1,100 to 1,300, because the archive no longer holds
  doomed parents to expand from. Rejections rose from about 65 to 150 up to
  about 420 to 1,036, which is the probe refusing candidates. Retained entries
  rose from about 2,300 to 3,400 up to about 3,580 to 4,092.
- Promotion replay: the seed `0x5eed_e000` challenger arm and its no-model
  replay are byte-equal with SHA-256
  `8991d328e40cc111ad9fd8b28089e138931658e0c4eb5eaac86f955c74c8f8d1`, and the
  summary records `replay_verified=true`. No model is involved anywhere in this
  panel.
- Decision: **promote the viability test at admission.** Retention now refuses a
  candidate that none of the fixed masks `0x00`, `0x01` and `0x81` keeps alive
  for 120 frames. Archive keys, novelty, the parent scheduler, the controller
  vocabulary, the duration and suffix policies, the action bound and ranking are
  unchanged, and the vertical-page key term stays fenced.
- Raw evidence: `target/smb-completion/h45-viability/`,
  `target/smb-completion/h45-viable-progress/`,
  `target/smb-completion/h45-held/`, `target/smb-completion/h45-held-viable/`
  and the frozen-path gate at `target/smb-completion/h45-gate-frozen/`.

- Correction recorded beside the decision above, because the wording overstated
  what changed in the code. Both retention policies remain selectable and the
  frozen one is still the default of every previously recorded mode, so every
  panel from H1 through H44 continues to reproduce exactly. What the promotion
  means in practice is that subsequent panels are run with the probing policy,
  through the mode that selects it, and that the frozen policy is kept only for
  reproducing recorded evidence.

### Standing summary after the admission probe

- The frontier in viable terms: 39 under the frozen terminal condition, 62 after
  correcting it, 94 to 112 on the better half of the arms resumed from that
  rebuild, and 93 to 114 on **every** arm once retention refuses states nothing
  can keep alive. Two changes, both to what counts as dead or worth keeping,
  and none to the scheduler, the controller vocabulary, the timing, the suffix
  policy, the archive keys or the budget.
- Two generated rankings have been measured at two boundaries and neither met
  its promotion rule. Both mechanical corrections in this stretch did.
- The observation state still records no horizontal position within the screen.
  D36 through D42 could not verify one because too few retained states moved the
  player; the rebuilt archives now retain far more live states, so that audit is
  worth re-running before the question is called closed.
- The vertical-page key term remains fenced and is the next single variable.

## D46 — preregistered player-column decode audit on a probed archive

- Same falsifiable claim as D36 through D42: one work-RAM index holds the
  player's horizontal position within the rendered screen, identifiable from the
  program's own film and memory evidence. D40 and D42 returned nothing because
  the archives they could source from held almost no state the controller moves.
  Every state a probed archive retains survived the H45 admission probe, so the
  evidence base has changed and the claim is worth testing again.
- Source is fixed by rule rather than chosen: the H45 challenger arm with the
  maximum viable progress, ties broken by fewest retained entries and then the
  smaller seed. On the recorded panel that is seed `0x5eed_e001` at viable
  progress 114, `target/smb-completion/h45-viability/probe-e001/archive-live.json`.
- Sourcing is D40's, unchanged: eight entries examined per progress bucket and
  256 in total, in descending progress with ties by `(input, id)`; the largest
  differing column span measured only on frames whose recorded camera bytes are
  equal, discarding spans above 128 columns; admission in descending largest
  span with ties by descending progress, at most two per bucket, requiring at
  least 24, until eight are admitted.
- Filters C0 and C1 are unchanged. C2 is D42's contrast at each entry's
  maximum-separation frame. Verification is D33's camera-spread film rule
  verbatim, with four-byte-stride rejection and lowest-index selection, and
  every index that passes the film rule is reported, not only the selected one.
- If fewer than eight entries reach a span of 24 the audit reports the scan and
  stops. If no index passes verification it reports itself inconclusive and
  selects nothing.
- Execution host. This audit may run on the ARM machine. Two gates cover that.
  The host must verify the ROM's SHA-256 equals the recorded
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea` before the
  audit runs; and the audit's live and no-model replay reports must be
  byte-equal on that host, exactly as locally.
- A third gate tests the assumption that permits remote work at all: the same
  audit is run on both machines from the same source bytes, and the two reports
  must be byte-equal. Emulation, decoded observations and rendered column
  signatures are all integer arithmetic over the same ROM, so a difference would
  mean one of the two hosts is not reproducing the recorded program and would
  invalidate remote evidence rather than merely this audit.
- The audit runs no search, changes no search behaviour, and involves no model.
  Raw destination: `target/smb-completion/d46-player-column-decode/` locally and
  the same relative path under `/root/harmony-smb-goal/` on the ARM machine.

### D46 result — inconclusive, but the evidence base did change and both hosts agree

- All three host gates pass. The ARM machine verified the ROM SHA-256 as
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`, matching
  the recorded value. Its live and no-model replay reports are byte-equal and
  the summary records `replay_verified=true`. And the third gate holds: run on
  both machines from the same source bytes, the audit's report and its scan
  record are byte-equal — SHA-256
  `ae63aba30f3adf9afdf9aa4c8ba87e782793f36be4dfc516833eab40b799b368` and
  `b85a9c4b9e69109b6ab355db13a7c968c70163e374970dabba21985d376361a3` on both —
  as is the combined checksum of all 45 rendered frames,
  `6abd4e6419fae0b6c4fec4cc8d6683f16c12560d8411430e0308dcee51a6340f`. Remote
  audit evidence reproduces the recorded program exactly.
- The evidence base changed as predicted. Scanning 256 entries across 37 buckets
  from progress 114 down to 62, **26** reach a differing span of 24 or more
  against D40's 2 of 256 on the unprobed archive, with recorded spans of 128,
  118, 115, 98, 95, 92, 90, 89, 88, 87, 82, 81, 80, 72, 48, 39, 35, 31, 28 and
  24. 132 entries still show only a sprite-width difference and 90 show none.
- The audit nonetheless returned nothing. Eight entries were admitted, 243
  indices took at least eight distinct values and 51 were also smooth, and zero
  survived filter C2, so nothing reached the film rule.
- The diagnosis is the same shape as before and now it is decisive about the
  filter rather than the archive. At the maximum-separation frame, six of the
  eight audited entries hold **identical** candidate values in both
  continuations while 95 to 128 columns of the picture differ; only ids 1772 and
  2577 separate a candidate, and there the stride-4 family at indices 519
  through 547, 941, 1196, 1198 and 1877 reads 26 against 48 and 26 against 55.
  A wide rendered difference is not by itself the player, and the span measure
  cannot tell the two apart.
- C2 is now the binding constraint and it cannot be satisfied. It demands the
  candidate separate on at least three quarters of the audited entries, while
  the evidence says only a minority of any span-sourced set separates the player
  at all. C2 was inherited from D29, written before the audit had a film rule
  strong enough to discriminate on its own.
- Raw evidence: `target/smb-completion/d46-player-column-decode/` on both
  machines and `target/smb-completion/d46-diagnosis/`.

## D47 — preregistered audit with verification in place of the direction filter

- Same falsifiable claim, source archive, target, sourcing scan, filters C0 and
  C1, four-byte-stride rejection, lowest-index selection, rendered PNG set,
  field name, operator sentence, byte-equal live-and-replay gate, and host gates
  as D46.
- The single change: filter C2 is removed from the gate chain. Every index that
  passes C0 and C1 goes to the film rule, which is unchanged — at least eight
  comparisons agreeing on one offset within a tolerance of 6 with a width in
  `4..=40`, including two whose recorded camera positions differ by at least 16
  pixels.
- The reason, stated before the numbers are seen: C2 is a pre-filter that
  demands three quarters of the audited entries separate the candidate, and D46
  measured that only a minority of any span-sourced set separates the player at
  all. The film rule needs no such majority. It asks whether one index predicts
  the differing columns with a consistent offset across cameras that are far
  apart, and comparisons that carry no separation simply fail to agree.
- Polarity is recorded rather than required. For each index that survives the
  film rule, count the comparisons at equal camera whose candidate values differ
  by at least 8, and the fraction of those in which the held-left continuation
  holds the smaller value. Report the index as right-increasing when that
  fraction is at least three quarters, left-increasing when it is at most one
  quarter, and inconsistent otherwise, with the counts. A selected index whose
  polarity is inconsistent is reported as selected-without-direction, and no
  direction is claimed for it.
- The risk is stated plainly: without a direction pre-filter more indices reach
  verification, so a false positive is likelier than in D29's design. What must
  carry the audit is the film rule's eight-comparison agreement, its width
  bounds, its camera-spread requirement and the stride rejection. Every
  surviving index is reported so that whether the selection is unique is
  visible in the record.
- The audit runs no search, changes no search behaviour, and involves no model.
  Raw destination: `target/smb-completion/d47-player-column-decode/`.

### D47 result — inconclusive; a verified family with the wrong direction, all stride-rejected

- Host gates pass again. The report is byte-equal across the two machines with
  SHA-256 `881ab8700eda31160c4bab7669a1c353db8ee6861233d6a0d59710f2c8f90372`,
  and live and no-model replay agree on each.
- Removing filter C2 did what it was registered to do: all 51 smooth candidates
  reached the film rule, and six of them passed it — indices 516, 520, 524, 528,
  532 and 536. Each records 84 agreeing comparisons out of 914, a camera spread
  of 265 pixels, 457 separating comparisons, and offsets of −3, −3, −11, −11,
  −19 and −19 in index order.
- Nothing was selected. The six survivors are spaced exactly four apart, so the
  four-byte-stride rejection removed every one of them. That rule exists to stop
  the audit picking an arbitrary member of an object table, and this is exactly
  the case it was written for: the six carry identical agreement statistics and
  offsets that step by 8 in pairs, so they are one quantity recorded at several
  fixed displacements rather than six independent bytes.
- The recorded polarity argues against the family being the field. On 457
  separating comparisons the held-left continuation holds the **larger** value
  383 times, so this quantity increases leftward. A screen-relative horizontal
  position increases rightward. The family behaves like a mirrored or
  right-referenced coordinate.
- The polarity-correct family failed verification. Indices 519, 523, 527, 531,
  535, 539, 543, 547, 941, 1196, 1198 and 1877 — the same table read four bytes
  across, and the family D46 recorded reading 26 against 48 and 26 against 55
  under the two masks, which is the rightward direction — all reached the film
  rule and none passed it. Index 134, the candidate D37 recorded as suggestive,
  also reached the film rule and failed.
- So the two halves of the claim now fail on different indices: the family the
  film rule verifies has the wrong direction and is a table, and the family with
  the right direction is not verified. That is a sharper result than the earlier
  nulls and it is not a starvation problem — 26 of 256 states separate the
  player and 914 comparisons were available.
- Raw evidence: `target/smb-completion/d47-player-column-decode/` on both
  machines.

## D48 — preregistered complement of the verified family

- Falsifiable claim: the screen-relative horizontal position is the byte
  complement of the family D47 verified. That family increases leftward; if it
  is a right-referenced coordinate, then `255 - raw` is the same quantity
  measured from the left, and it must pass the film rule on its own with
  rightward polarity.
- The family, recorded in full: indices 516, 520, 524, 528, 532 and 536, each
  with 84 agreeing comparisons of 914, camera spread 265, and offsets −3, −3,
  −11, −11, −19, −19 in index order. The representative is the lowest surviving
  index, **516**, chosen by that rule and not by inspection.
- Exactly one transform is tried: `derived = 255 - raw` applied to index 516.
  No other constant, no other index, no other form. If this fails the field
  question closes as a registered null.
- The audit evaluates exactly one candidate, so the four-byte-stride rejection
  cannot fire. That is stated rather than relied on.
- Filters C0 and C1 are unchanged and are preserved exactly by a complement —
  it maps distinct values to distinct values and leaves every frame-to-frame
  step size identical — so neither can be the reason the derived value passes.
  The film rule is verbatim: the offset searched over `-24..=24`, a tolerance of
  6, a width in `4..=40`, at least eight agreeing comparisons, and at least two
  of them from recorded camera positions 16 or more pixels apart.
- Acceptance additionally requires the recorded polarity to be right-increasing,
  meaning the held-left continuation holds the smaller value on at least three
  quarters of the separating comparisons. A derived value that verifies with the
  wrong direction is a null, not a selection.
- Prediction fixed before execution: if the hypothesis is right the agreeing
  count should rise well above the raw byte's 84 of 914, because a complement
  turns an offset that varies with the pair of values into one that does not.
  An agreeing count near 84 would mean the complement changed nothing and the
  agreement was coincidental.
- Source archive, sourcing scan, audited entries, replay gate and both host
  gates are unchanged from D47.
- If it verifies, the field is admitted in a separate mechanism entry that
  states its derivation, its inclusive range and its direction in both operator
  views, and carries a no-behaviour-change gate over the search. This audit
  admits nothing by itself.
- Raw destination: `target/smb-completion/d48-player-column-decode/`.

### D48 result — rejected; the complement is decisively wrong

- Host gates pass. The report is byte-equal across both machines with SHA-256
  `7da2ecd979bf2c5db3dd9ee7fe27bff089a74f3f98c0839e64ca19270d271359`, and live
  and no-model replay agree on each.
- The single derived candidate behaved exactly as the registration predicted it
  would if the complement preserved the filters: one index took at least eight
  distinct values, one was smooth, and one reached the film rule. So C0 and C1
  decided nothing, as stated in advance.
- The film rule rejected it. `255 - raw` at index 516 produced no offset in
  `-24..=24` with eight agreeing comparisons, so no evidence was recorded and
  nothing was selected.
- A measurement dump over the same 914 comparisons says how badly. Only 212 have
  a width in the required `4..=40`, and among those the computed offsets run
  from −171 to 33 and no offset in the searched window collects even one
  agreeing comparison within the tolerance of 6. The registered prediction was
  that a correct complement would raise agreement well above the raw byte's 84
  of 914; it fell to zero. The relationship is not a complement about 255, and
  it is not marginally wrong.
- **The field question is closed as a registered null.** The observation state
  records no horizontal position within the screen, nothing was added to it or
  to either operator view, and the search, archive keys, novelty map and
  scheduler remain untouched by this line of work.
- What the whole sequence D29 through D48 established, stated plainly so it is
  not re-litigated by accident. The audit mechanism is sound and reproduces byte
  for byte, including across two processor architectures. The two failures that
  it did find — a terminal condition that missed a whole death mode, and a
  retention rule that kept states nothing could keep alive — were both real,
  both correctable, and together moved the frontier from 39 to between 93 and
  114. What it never found is the field it was built to find. Twelve audits over
  four archives could not verify any work-RAM byte, raw or derived, as the
  player's horizontal position within the screen, and the last two attempts
  failed on evidence rather than on sourcing.
- Raw evidence: `target/smb-completion/d48-player-column-decode/` on both
  machines and `target/smb-completion/d48-diagnosis/`.

## C49 — registered observational conquest campaigns

- These are observational runs, not a test. There is no falsifiable claim, no
  control, and no acceptance rule. The deliverable is the recorded archive, the
  milestone ladder it reaches, and film of the furthest trajectory.
- Stack: the promoted one. Corrected terminal condition from M35, probing
  retention from H45, no model, no generated ranking and no generated mutator.
  The frozen parent scheduler, nine-mask controller vocabulary, stratified
  durations, one-or-two suffixes, capacity-two cells with fewer-actions
  replacement and the 512-action bound are unchanged, and the vertical-page key
  term is not part of these runs.
- Source is the archive with the deepest recorded live frontier,
  `target/smb-completion/h45-viability/probe-e001/archive-live.json` at viable
  progress 114, resumed from its single shortest mechanical frontier input of
  278 actions.
- Two runs with distinct seeds. `0x5eed_c001` on the local machine at
  `nice -n 10`; `0x5eed_c002` on the ARM machine under the recorded box rules,
  also at `nice -n 10`. Executor is the snapshot-resume archive path in both.
- 50,000 target executions each. The standing 20,000-execution ceiling is lifted
  for these two runs only, by integrator authorization recorded here.
- Constraint recorded in advance so it is not mistaken for difficulty later: the
  resume input is 278 actions against the frozen 512-action bound, leaving 234
  actions of headroom. If a run stops advancing while its deepest entries sit at
  the bound, that is the bound.
- No inline replay is run, because replaying 50,000 executions doubles a run of
  several hours. The seed, source, mode and executor are recorded here so either
  run reproduces on demand, and byte-exact determinism of this exact
  configuration is already established by H45's replayed challenger arm.
- World and level transitions are reported as they are observed. Raw
  destinations: `target/smb-completion/c49-conquest-local/` and, on the ARM
  machine, `target/smb-completion/c49-conquest-arm/`.

## H51 — preregistered vertical-page term in the archive key

- Motivating evidence. The archive's vertical key term is the recorded low
  position byte divided by sixteen, with no page term. D34 recorded that live
  play occupies vertical pages 0 and 1 — 211 frames on page 0 and 9,795 on page
  1 across a 10,006-frame control — so the key gives the same bucket to a state
  at page 0 and one 256 pixels lower at page 1. That aliasing is the last
  surviving piece of the fault that produced the progress-39 plateau: it is what
  let a single fall walk through all sixteen buckets, and H45 removed the falls
  without removing the aliasing.
- Falsifiable claim: separating the two vertical pages in the archive key gives
  the search distinct cells for states it currently merges, so arms reach
  further. Challenger arms will exceed the source archive's viable progress on
  at least 4 of 6 development seeds, with strictly more successes than the
  controls.
- The threshold is 4/6 rather than H45's 5/6, and the reason is stated before
  execution rather than after. H45 targeted a failure that trapped half the
  arms, so most arms could be expected to change. This panel starts from a
  frontier where the controls already advance, so the discriminating
  requirement is the comparison against the controls, not the raw count.
- Mechanism, and it is the only variable. The archive key's vertical term
  becomes the recorded vertical page byte times sixteen plus the recorded low
  position byte divided by sixteen, clamped to the byte range. Live states hold
  pages 0 and 1, so the term ranges over 0 through 31 where it previously ranged
  over 0 through 15.
- The decoded observation state is deliberately **not** changed. Its
  `player_y_bucket` keeps its recorded meaning and its documented range of 0
  through 15, so both operator views stay true and no model-facing text moves.
  This panel changes one archive key term and nothing else: novelty, the parent
  scheduler, the controller vocabulary, the duration and suffix policies,
  retention capacity, fewer-actions replacement, the 512-action bound, the
  corrected terminal condition and the probing retention rule are all unchanged.
- Gate fixed before execution: with the previous key term selected, a resumed
  arm must reproduce H45's recorded challenger arm at the same seed byte for
  byte, so the change is inert when it is not asked for.
- Source is `target/smb-completion/h45-viability/probe-e001/archive-live.json`
  at viable progress 114, with the same single shortest mechanical frontier
  input. Controls carry the previous key term and challengers the new one, both
  on the promoted stack, on seeds `0x5eed_e000..=0x5eed_e005` at 5,000
  executions each, at `nice -n 10`, with at most five arms concurrent so the
  running conquest campaign keeps a lane.
- Acceptance uses the registered viable-progress measurement unchanged. If it
  accepts, repeat on held-out seeds `0x5eed_e100..=0x5eed_e105` with the same
  threshold; a promotion must replay exactly with no model.
- Raw destination: `target/smb-completion/h51-vertical-key/`.

### H51 execution note — recorded beside the registration

- The panel started on the local machine and was moved to the ARM machine after
  four arms had run. The local machine's load average reached 145 on ten cores
  because another experiment with priority was running there, and the five panel
  arms were each getting about two thirds of a core. Nothing was kept from the
  local attempt; the panel was restarted from the beginning on the ARM machine.
- The registration fixes the source, seeds, budget, policies and acceptance rule
  but not the host, and D46 established that this program's audits and reports
  are byte-equal across the two architectures, so the move changes no recorded
  quantity. The conquest campaign registered as local stays local and keeps its
  lane; the ARM machine runs its own conquest campaign plus at most five panel
  arms, so neither machine exceeds six concurrent arms.

### C49 correction recorded beside the registration

- The registration says world and level transitions are reported as they are
  observed. The campaign binary writes its report only when a run finishes, so
  for these two runs transitions become observable at completion and not before.
  Nothing was changed in the running campaigns to alter that; a progress file
  would have to be added to the campaign path and that is a code change to a
  running experiment. The deliverable is unchanged and the transitions will be
  reported from the recorded milestone ladder and progress curve.

### H51 result — rejected; the key term is mostly inert once falls are not retained

- The inertness gate passes completely. With the previous key term selected, all
  six control arms reproduce H45's recorded challenger arms byte for byte:
  `8991d328…`, `20a5dea2…`, `0f2c52bf…`, `1528afb6…`, `63a0a476…` and
  `31eaa2b9…`. Those H45 arms were produced on the local machine and these on
  the ARM machine, so the gate also establishes byte-equal reproduction of a
  complete 5,000-execution campaign across the two architectures — a stronger
  result than D46's audit-level equality.
- Viable progress by arm against the source archive's 62. Controls
  `[102, 114, 97, 107, 94, 99]`, six successes. Challengers
  `[102, 114, 113, 94, 94, 111]`, six successes.
- **Decision: reject.** The registration required at least 4/6 and strictly more
  successes than the controls; the panel delivered 6/6 against 6/6, a tie.
- The per-seed comparison is where the real answer is, and it is close to
  nothing. Seeds `e000`, `e001` and `e004` finish at exactly the same viable
  progress, retained count, rejection count and death count as their controls.
  Seed `e002` gains 16 buckets and `e005` gains 12; seed `e003` loses 13. Median
  viable progress moves from 100.5 to 106.5. All twelve archives differ byte for
  byte, so the key term does change which states are kept — it just does not
  change where the arms get to.
- The mechanism explains itself. The key term separates two vertical pages, and
  H45's admission probe already refuses the states that sit on the second one,
  so on half the seeds there was nothing left for it to separate. Fencing this
  change until after the retention rule was the right order; what it also shows
  is that the retention rule absorbed most of what this term was for.
- A flaw in my own registration, recorded beside it rather than by editing it.
  The acceptance rule counted successes against the source archive's viable
  progress of 62, but both groups resume from a probed archive that clears 62 on
  every seed, so the success count saturated at 6/6 and carried no information.
  The comparison that carries information here is per-seed against the paired
  control, and a future panel at a deep frontier should register that instead.
  The rejection stands on the registered rule and is also what the per-seed
  numbers say.
- Raw evidence: `target/smb-completion/h51-vertical-key/` and
  `target/smb-completion/h51-viable/` on the ARM machine, with summaries and
  viability reports copied locally.

### Standing summary after the vertical-page panel

- Three mechanical changes have now been measured against the frozen stack. The
  corrected terminal condition and the admission probe both met their promotion
  rules and together moved viable progress from 39 to between 93 and 114. The
  vertical-page key term did not, and the reason is that the probe had already
  removed the states it was meant to distinguish.
- Two generated rankings have been measured at two boundaries and neither met
  its rule. The horizontal-position field is a closed registered null.
- Nothing else in the search is currently under suspicion from recorded
  evidence. The next question is what a model panel can do on an honest
  frontier, which is the assignment that follows.

### C49 local result — the first world completed and the second entered

- Seed `0x5eed_c001`, 50,000 executions, 23,248 retained, 29,996 rejected by the
  admission probe, 4,592 deaths. No entry reached the 512-action bound; the
  longest retained input is 435 actions, so the bound did not bind.
- The decoded tuple advanced twice. Retained entries by tuple: 463 at
  `(0, 0)`, 1,030 at `(0, 1)`, 4,137 at `(0, 2)`, **7,243 at `(0, 3)`** and
  **10,375 at `(1, 0)`**. The source archive's deepest tuple was `(0, 2)`, so
  this run crossed two transitions the program had never crossed.
- Film confirms both, independently of the decode. The furthest trajectory ends
  on the level-completion sequence of the fourth level of the first world — a
  castle interior at frame 300, then the end-of-castle message — and a separate
  film of a deep `(1, 0)` state shows ordinary play in the first level of the
  second world, with its own scenery, pipes and enemies. Raw work RAM and the
  rendered frames agree.
- The deepest genuine play in the second world is progress bucket 124, held by
  1,276 retained entries, with buckets 121 through 124 holding 2,173 between
  them. The single entry at bucket 144 is the castle-completion sequence of the
  previous level, not play.
- That exposes a limitation of the viable-progress measure worth recording: it
  reports 144 for this archive, because a cutscene state trivially survives 120
  frames of no input. The measure was built to exclude falls and it does; it
  does not distinguish play from a scripted sequence. Where the two differ, the
  play figure is the honest one.
- The progress curve is unremarkable and healthy: active entries rise from 355
  at 100 executions to 18,570 at 45,100, occupied cells from 349 to 13,573, and
  deaths accumulate steadily from 12 to 4,431. The archive never exhausted and
  never stalled.
- Deliverables: the recorded archive at
  `target/smb-completion/c49-conquest-local/`, film of the furthest trajectory
  at `target/smb-completion/c49-film-local/` including `film.mp4`, and film of
  the deepest second-world play at `target/smb-completion/c49-film-w2b/`.
- The frozen milestone ladder is now saturated. All four of its rungs —
  progress in the first level, that level's end task, entry into the second
  level, entry beyond it — were already true in the source archive, so the
  ladder records nothing about this run. A campaign that has completed a world
  needs a ladder that can still measure it.
- The companion run on the ARM machine, seed `0x5eed_c002`, is still executing.

## M52 — preregistered extended milestone ladder and play measurement

- Mechanism claim, in three parts. First, the recorded ladder becomes a
  quantity that cannot saturate: the maximum corrected `(world, level,
  progress)` tuple, and every distinct `(world, level)` observed with the
  execution at which it first appears and the deepest progress reached in it.
  The four fixed named rungs are kept exactly as they are and are not replaced;
  they simply stop being the only thing recorded.
- Second, the ladder is versioned and selectable, so nothing recorded moves. A
  campaign run under the frozen ladder policy serializes precisely the fields it
  serialized before — the extended ladder is omitted from the report entirely
  rather than written as an empty value — so every earlier recorded campaign
  keeps its recorded ladder and reproduces byte for byte. Only a campaign run
  under the extended policy carries the new record, stamped with its version.
- Third, an offline derivation, because the two conquest campaigns already ran
  under the frozen ladder and must not be re-run to be measured. Every archive
  entry records its key and its creating execution, so the maximum tuple, the
  set of observed `(world, level)` pairs, each one's first creating execution
  and each one's deepest progress are all derivable from a recorded archive
  without emulation. The difference from the in-campaign record is stated: the
  offline derivation reports first execution but not first frame, because frames
  are not recorded per entry.
- Fourth, the play measurement. The viable-progress measure counts a state that
  survives 120 frames of no input, and C49 showed that a scripted
  level-completion sequence satisfies it — the local run's deepest bucket, 144,
  is a cutscene with the same engine-state byte as ordinary play, so no decoded
  field separates them. Play progress is therefore measured with the rendered
  test D37 established: the deepest bucket at the maximum tuple holding a state
  whose held-right and held-left 120-frame continuations differ in at least one
  rendered column. Examining at most eight entries per bucket in descending
  order, stopping at the first bucket that qualifies, mirrors the viable
  measurement exactly. Both figures are reported wherever they differ, with play
  as the primary one.
- Gates fixed before execution. G1: with the frozen ladder policy selected, a
  resumed arm reproduces a recorded arm byte for byte, proving the change is
  inert when it is not asked for. G2: the offline derivation applied to the
  local conquest archive reproduces the tuple counts already recorded in this
  log by hand — 463, 1,030, 4,137, 7,243 and 10,375 — and its maximum tuple is
  `(1, 0, 144)`. G3: the four quality gates pass. G4: the play and viable
  measurements are byte-equal across two runs on the same archive.
- No search campaign, no acceptance rule about progress, and no model are part
  of M52. Raw destination: `target/smb-completion/m52-ladder/`.

### M52 result — accepted

- G1 passes exactly. Under the frozen ladder policy a resumed arm reproduces
  H45's recorded challenger arm byte for byte, SHA-256
  `20a5dea2578787f857e52768f2b153c1736d9609c99666fce893ee7820999403`, and the
  written report contains no ladder field at all. Every earlier recorded
  campaign keeps its recorded ladder and replays exactly.
- G2 passes. The offline derivation over the local conquest archive returns
  version 2, maximum tuple `(1, 0, 144)`, and exactly the five observed pairs
  whose counts were recorded by hand in the C49 entry.
- G3 passes: formatting, Clippy with `-D warnings`, 70 tests, and the dependency
  check. G4 passes: the play and viable measurement is byte-equal across two
  runs on the same archive, SHA-256
  `2f6cbd5f4f95ee959322ca0fe3a2896a851924a1b8fbb47b3a8567df03a4048d`.
- The play measurement does what C49 asked for. On the local conquest archive it
  reports play 124 against viable 144: the deepest bucket survives 120 frames of
  no input but its rendered frames do not answer the controller, which is the
  scripted level-completion sequence. The two figures are now reported
  separately wherever they differ, with play primary.
- The ladder cannot saturate. It records the maximum corrected tuple and every
  observed `(world, level)` with the execution at which it first appeared and
  the deepest progress reached inside it, so a campaign that completes a world
  is still measured by it.

### C49 ARM result — the same two transitions, independently

- Seed `0x5eed_c002`, 50,000 executions, 20,055 retained, 31,477 rejected,
  5,820 deaths.
- Its ladder records the same five pairs and the same two crossings the local
  run made: the fourth level of the first world first reached at execution
  1,959 against the local run's 1,579, and the first level of the second world
  at execution 8,051 against 8,872. Two independent seeds crossed both
  boundaries at comparable cost, so this is behaviour of the promoted stack and
  not a lucky seed.
- Depth inside the second world differs. The ARM run reports play 97, viable 97
  and recorded 97 — its deepest state is ordinary play, with no scripted
  sequence above it — against the local run's play 124 and viable 144. Reported
  on the primary figure the two runs reached 124 and 97.
- Both runs spent the large majority of their budget inside the second world:
  the local run entered it at execution 8,872 of 50,000 and the ARM run at
  8,051, so roughly forty thousand executions each were spent there without
  leaving it.
- Raw evidence: `target/smb-completion/c49-conquest-arm/` on the ARM machine and
  `target/smb-completion/m52-ladder/` on both.

## M53 — preregistered panel prerequisites for the second-world frontier

- Four changes, all to how the host runs a model panel, none to what the model
  is asked for. The artifact interface, the decision schema, the trait the
  generated ranking implements, the validators, the forbidden progress tokens
  and the journal contract are untouched.
- **One: the panel runs the promoted stack.** The generated-ranking harness
  calls the frozen retention path for both its control and its ranking arm, so a
  panel run today would execute without the admission probe H45 promoted. Both
  arms are changed to the probing retention path. The control's existing call
  already resolves to the one-or-two suffix policy, so the change is retention
  and nothing else. Without it the panel would measure a generated ranking
  against a search that has been retired, with controls that stall the way H44's
  did.
- **Two: the resume source becomes the deepest play bucket.** The frozen rule
  takes the shortest input at the maximum recorded tuple. On the conquest
  archives that is a scripted level-completion sequence, not play. The rule
  becomes the shortest input at the deepest bucket whose states answer the
  controller, measured by M52's play measurement on the host and passed to the
  harness. The rationale is recorded: the resume source and the acceptance
  measure must agree about what the frontier is, and H44 showed the cost of the
  mismatch — its recorded progress ran up to eleven buckets ahead of its viable
  progress and would have claimed advances that were partly falls.
- **Three: the panel splits into a decision phase and an arm phase.** The model
  is reachable only from the local machine, and the local machine is carrying
  another experiment at a load average above 150, while thirteen arms at five
  thousand executions with the admission probe is several hours of work. The
  decision phase performs the single model invocation, the validators, the fixed
  isolation pilot and its replay, and the journal-chain replay, and records the
  decision. The arm phase rebuilds the generated ranking from that recorded
  source on whichever machine runs it and executes the arms. Nothing about the
  decision, the validation or the arms changes; only where they run.
- **Four: the context and journal are amended a third time.** Both operator
  views gain the extended ladder and the fact that progress is measured within
  the current level and restarts when the tuple advances, which is a decode fact
  from the recorded conquest archives. The journal is re-seeded with the
  vertical-page null, the closed horizontal-field question, the world completed
  and the second entered, the play-versus-viable distinction, and the replicated
  fact that both conquest seeds spent roughly forty thousand of their fifty
  thousand executions inside the second world without leaving it. No ranking
  term, field, action, route, layout, item or goal is suggested.
- Gates fixed before execution. G1: the harness control arm and the equivalent
  command-line campaign, both under probing retention at a fixed seed and a
  fixed small budget, produce byte-equal reports. G2: the recorded decision's
  generated source has the same SHA-256 on both machines and the arm rebuilt
  from it produces byte-equal reports on both for one seed. G3: the four quality
  gates pass. G4: the amended context fixture passes, still rejecting game names
  and checking the complete field list, and the recorded-journal chain still
  replays without a model.
- No search acceptance rule and no progress claim are part of M53. Raw
  destination: `target/smb-completion/m53-panel-prerequisites/`.

## H54 — preregistered journal-informed ranking at the second-world play frontier

- Falsifiable claim: with the corrected context, the re-seeded journal and a
  frontier inside the second world, the instrumentor can infer a non-progress
  within-cell ranking that preserves states better prepared to produce
  descendant novelty beyond that frontier.
- Source is fixed by rule: the conquest campaign with the greater play progress,
  ties by fewer retained entries and then the smaller seed. That is the local
  run, seed `0x5eed_c001`, at play progress 124 inside the second decoded world,
  `target/smb-completion/c49-conquest-local/archive-live.json`. Film evidence is
  the recorded film of that same play bucket at
  `target/smb-completion/c49-film-w2b/`.
- Protocol is H27's with M53's four changes and nothing else: one ranking
  invocation through the existing decision schema with at most three
  compile-and-fixture attempts; the existing pure and deterministic source
  checks with progress terms still forbidden; observation fixtures; the fixed
  seed `0x5eed_ef00` 256-execution isolation pilot and its exact replay; the
  recorded-journal chain replay; controls and ranking arms on seeds
  `0x5eed_e000..=0x5eed_e005` at 5,000 executions each; the frozen scheduler,
  nine-mask vocabulary, stratified durations, one-or-two suffix and 512-action
  bound; the ranking consulted only for full-cell replacement with fewer actions
  as the final tie-breaker; no other generated artifact; at most six arms
  concurrent on either machine.
- Acceptance is paired, which is the lesson H51 recorded. Each ranking arm is
  compared against the control arm at the same seed, and acceptance requires the
  ranking arm's play progress to be strictly greater than its paired control's
  on at least 4 of 6 development seeds. Counting successes against a fixed
  baseline saturates at a deep frontier and carries no information, which is
  exactly how H51's rule failed.
- Play progress is M52's measurement and is primary. Viable progress and
  recorded progress are reported alongside it for every arm, and any arm where
  they differ is recorded with all three figures.
- If it accepts, repeat unchanged on held-out seeds `0x5eed_e100..=0x5eed_e105`
  with the same paired threshold. Any promotion must replay exactly from
  recorded seed, observations, generated files, labels and journals with no
  model.
- Raw destination: `target/smb-completion/h54-luna/`.

### H54 first decision phase — host defect, no decision produced

- The decision phase exhausted its three attempts and produced no ranking. The
  three attempts failed for two different reasons and only the first is about
  the model.
- Attempt 1 was rejected by the existing validator: the returned source used the
  forbidden token `progress`. That is a real, recorded model outcome of the same
  kind as H27's first attempt, which used `flag`.
- Attempts 2 and 3 failed to compile with `cannot find function
  run_smb_archive_search_with_retention in this scope`. That is my defect, not
  the model's. M53 changed the harness template to call the probing retention
  path but the matching edit to the template's import block did not apply,
  because the block spans more lines than the text I matched, and the edit
  silently changed nothing. Two of the three attempts were consumed by a broken
  host.
- The import block is corrected, and the corrected template was compiled offline
  against attempt 3's own recorded ranking source before any further model call,
  so the fix is verified rather than assumed. The failed directory is retained as
  `target/smb-completion/h54-luna-failed/`; its recorded model sources under
  `artifact-validation/` are untouched, and only its scratch build tree was
  reused for that compile check.
- The registration's three-attempt limit is a limit on what the model is asked
  to do, and a host that cannot compile its own template did not ask it. The
  decision phase is re-run from a clean directory with the same fixed source,
  film, context, journal, seeds, budgets and validators. This follows the
  precedent recorded at H27, where a launch-boundary failure produced no
  decision and the panel was resumed unchanged.

### M53 result — accepted

- G1 passes exactly. The generated-ranking harness's control arm and the
  equivalent command-line campaign, both under probing retention from the same
  initial input at seed `0x5eed_ef00` for 256 executions, produce byte-equal
  reports with SHA-256
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360`. The panel
  now runs the promoted stack.
- The source-selection change is verified against the recorded archive: the
  deepest play bucket 124 holds 1,276 entries whose shortest input is 422
  actions, while the maximum recorded tuple holds a single entry of 351 actions
  that is the scripted level-completion sequence. The panel resumes from the
  former.
- The split into a decision phase and an arm phase works. The decision phase ran
  locally, where the model is reachable; the arm phase rebuilt the generated
  ranking from the recorded decision on the ARM machine and is executing there.
- G3 passes: formatting, Clippy with `-D warnings`, 70 tests, and the dependency
  check. G4 passes: the amended context fixture passes and the recorded-journal
  chain replays without a model. The amended context is recorded at
  `target/smb-completion/m53-panel-prerequisites/context/` with field-semantics
  SHA-256 `869d7deea161c057a846614c3c469e7bbf71aa6dc02de7fad9c2f99dd012a479`,
  verified-dynamics `6c7bbf11e735611fb5f68f7a9f4efda8c2e38f891da5de382e777f66dcb2225f`
  and journal `7a6d0aaa0b369501e158feddb4f8b64de86d8d718b0465c5534ecfbbf59ea0f8`.
- One defect found and fixed during execution, recorded rather than smoothed
  over: the arm phase initially refused to start because the guard that stops a
  decision phase resuming a half-run model trial also fired for the arm phase,
  which legitimately consumes a completed decision. The guard is now applied to
  the decision phase only.

### H54 decision phase — accepted on the first attempt

- The instrumentor returned a usable artifact on attempt 1: an installed ranking
  named `archive-ranking-changed-state` scoring distinct changed work-RAM
  addresses, transitions in the size of the changed-address footprint, and the
  number of records showing any change at all. Its source SHA-256 is
  `2a27043379213d4eb73359b883514105c26929bab68e7db24c6a212c35aa9e4a` and its
  138-word journal passed without compression.
- It passed the deterministic source checks with progress terms still forbidden,
  the observation fixtures, the fixed seed `0x5eed_ef00` 256-execution isolation
  pilot with its exact replay, and the recorded-journal chain replay.
- The arm phase is executing on the ARM machine. Its result follows.

### Campaign-mode merge — recorded here because it touches this experiment's modules

- On the integrator's ruling, `exec/campaign-mode` was merged into this branch as
  a strict fast-forward from `0b7ea732` to `4a97e2f2`. Twelve commits, adding a
  campaign coordinator, its binary and its own lab log, and touching two modules
  this experiment owns.
- What it changed here, checked before merging. `phase4b` gains a lifetime frame
  counter documented as work accounting only: it is not campaign state, no
  snapshot carries it and `restore` does not touch it, so it cannot alter
  replay. `phase4c` widens the archive internals to crate visibility, which is
  no behaviour change, and threads an emulated-frame total out of the internal
  search so every existing wrapper maps it away and returns exactly the report
  it returned before.
- All four gates pass on the merged tree: `cargo fmt --check`, `cargo clippy
  --all-features` with `-D warnings`, 76 tests run and 76 passed under
  `cargo nextest run --all-features` — six more than before the merge, all of
  them the incoming campaign tests — and `cargo deny check` reporting
  advisories, bans, licenses and sources ok.
- A determinism spot-check of this experiment's own path was run beyond the
  required gates, because every recorded result here depends on byte-exact
  reproduction. The M53 gate campaign, seed `0x5eed_ef00` for 256 executions on
  the conquest archive under probing retention and the extended ladder,
  reproduces its recorded SHA-256
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` across the
  merge.
- Landmine recorded on the integrator's instruction: the executor-identity
  binary composes its process exit code from a superseded tenfold
  frame-reduction criterion, so it exits non-zero even when the ruling-level
  acceptance passes — identity bits plus semantic hashes against the re-frozen
  reference. Judge that binary by the contents of its report, never by its exit
  status.
- The H54 arm phase was not touched. It runs from a copy synced to the ARM
  machine before the merge and is unaffected by anything in this worktree.

### H54 arm phase — VOID on defective substrate, by integrator ruling

- The arm phase was stopped in flight on the integrator's ruling and its verdict
  is void. It is recorded as substrate-confounded: **no acceptance verdict was
  computed, and none should be read into the recorded arms in either direction.**
- The ruling's rationale, checked here against `phase4c` before the abort. The
  frozen selector — the one both the control and the ranking arms run — sorts
  active entries by `(milestone_key, archive key, id)` and expands the last 128.
  At this depth `milestone_key` saturates: **21,756 of the source archive's
  23,248 entries hold the identical maximal value**, so the primary sort key
  distinguishes nothing and ordering falls through to the archive key, whose
  fields after progress are the vertical bucket, the engine-state byte and a
  six-bit fingerprint. The resulting window is **entirely at vertical bucket 11
  and engine state 8**, and it contains 127 of the 1,276 entries at the deepest
  play bucket — the ones that happen to sort last by fingerprint. Three draws in
  four came from that slice. A ranking measured against a control that draws
  from the same arbitrary slice is confounded whichever way it falls, which is
  the same reasoning that voided the Scale Panel.
- State at the abort: twelve of thirteen arms had completed — six controls and
  six ranking arms on seeds `0x5eed_e000..=0x5eed_e005` — and the thirteenth,
  the no-model replay arm, was 21 minutes in. No `m13-report.json` was written,
  so no aggregate ever existed.
- Everything is retained. All twelve completed arm reports, their process logs,
  `h54.log`, the recorded decision, the model records, the strategy-journal
  artifacts and the failed first-decision directory are untouched on the ARM
  machine. The chain was stopped by explicit process identifier only, never by
  pattern match, because pattern matching self-matches wrapper argv on that box.
- **The decision phase stands.** The accepted ranking `archive-ranking-changed-state`,
  its source with SHA-256
  `2a27043379213d4eb73359b883514105c26929bab68e7db24c6a212c35aa9e4a`, its
  138-word journal, its validator record and its isolation pilot are valid and
  exactly replayable. Re-testing that ranking on a corrected selector requires
  no new model call: the arm phase reads the recorded decision and rebuilds from
  it.
- The registration that follows is the corrected selector. The ranking panel is
  not re-registered here; whether and when to re-run it on the corrected
  substrate is a separate decision.

## H56 — preregistered selection correction

- Drafted in advance on the integrator's instruction while the H54 arms were
  still running, and registered here unchanged in substance now that the arm
  phase is void and the work is unblocked. Nothing had been executed against it.

## Motivating evidence, measured before drafting

The diagnosis was supplied by the integrator and independently checked here
against `phase4c` and against the recorded conquest archive
`target/smb-completion/c49-conquest-local/archive-live.json`.

- The frozen selector — the one every promoted campaign uses — sorts the active
  entries by `(milestone_key, archive key, id)` and then takes the **last 128**
  as its frontier window. `milestone_key` is the four frozen rungs: reached
  onward, reached the second level, reached the first level's end task, and the
  greatest first-level scroll bucket.
- That signal is dead at the current depth. **21,756 of 23,248 recorded entries
  hold the identical maximal `milestone_key`** `(true, true, true, 195)`. The
  primary sort key distinguishes nothing among them, so ordering falls through
  to the archive key, whose fields after progress are the vertical bucket, the
  engine-state byte and a six-bit fingerprint.
- The window that results is not a sample of the frontier. Its 128 entries are
  **all at vertical bucket 11** and all at engine state 8. Of the 1,276 entries
  at the deepest play bucket, **127 are in the window** — those that happen to
  sort last by vertical bucket and fingerprint. This is the same defect family
  as the bucket-15 artifact: a key field that was never meant to rank anything
  is deciding what the search expands, three draws in four.
- The remaining draw in four is uniform over all active entries, which at this
  depth is roughly twenty-three thousand states.
- Nothing records what a parent has produced. `ArchiveEntry` carries a report, a
  snapshot and its observations, and no counter of any kind. A parent that has
  been mutated thousands of times with no retained descendant is sampled exactly
  like one never tried — the energy idea that a coverage-guided fuzzer would
  supply and this custom cell archive never reimplemented.

## Mechanism

Three changes, and the registration states plainly that they are three rather
than pretending they are one, because they are not separable: correcting the key
without correcting the tie handling would concentrate every draw on a single
state, and correcting both without accounting would leave that state unable to
yield the frontier when it stops producing.

1. **Selection key.** The frozen `milestone_key` is removed from selection and
   replaced by the corrected `(world, level, progress)` tuple with M52's ladder
   semantics: lexicographic, so a later pair always outranks an earlier one and
   a larger progress inside an earlier pair never outranks a later pair. The
   four named rungs remain in every report and are not touched; they stop being
   a ranking signal, which is the only thing they had saturated at.
2. **Tie handling.** The 128-entry sorted slice is removed. The frontier becomes
   the best surviving **tie class**: all active entries at the maximal
   `(world, level)` whose progress lies within a fixed band of the deepest
   progress in that pair, sampled uniformly. Classes are considered in
   descending order and a class every member of which is exhausted is skipped,
   so the frontier falls through rather than dead-ending. The band is fixed at
   the recorded `FRONTIER_PROGRESS_BAND` of 8 and is not a new parameter.
3. **Per-entry accounting.** Each entry records the number of times it has been
   selected as a parent and the number of selections that produced at least one
   retained descendant. An entry is exhausted when its selections since its last
   retained descendant reach a fixed threshold of 64. A retained descendant
   resets the counter. If every active entry is exhausted, all counters reset to
   zero and selection proceeds; the search must not deadlock, and the reset is
   deterministic and recorded in the report.

The falling-through rule is what makes the first two safe together. On the
recorded conquest archive the maximal tuple `(1, 0, 144)` holds exactly **one**
entry, and it is the scripted level-completion sequence. Under the corrected key
and strict tie handling that single state would take three draws in four until
it exhausts, at which point its class is skipped and the frontier becomes the
band below it, which is the play frontier at 124 with 1,276 members. That
behaviour is intended, is stated here before execution, and is the reason the
accounting is part of this registration rather than a later one.

## Deliberately not changed

The one-in-four uniform draw over the whole archive is diagnosed as dilution and
is **left alone**. It is a fourth variable, the accounting already concentrates
the frontier path, and the uniform draw is the archive's only remaining source
of diversity if the frontier path is wrong. It is recorded here as the natural
follow-up rather than folded in, on the same one-variable-at-a-time discipline
that governed the terminal condition, the retention rule and the key term.

## Gates fixed before execution

- **G1, inertness.** With the frozen selector selected, a resumed arm reproduces
  a recorded arm byte for byte, so the correction is inert when it is not asked
  for. Every earlier recorded campaign continues to replay exactly.
- **G2, determinism.** One corrected arm replays byte-identically from its
  recorded seed with no model.
- **G3.** `cargo fmt --check`, `cargo clippy --all-features` with `-D warnings`,
  `cargo nextest run --all-features`, and `cargo deny check`.
- **G4, accounting honesty.** The selection and novelty counters, and any
  counter reset, are reported per campaign, so the claim that exhausted parents
  were starved is checkable from the record rather than asserted.

## Acceptance

Paired against the frozen selector, which is the discipline the terminal-condition
correction and H54 both used and the one whose absence made H51's rule
uninformative. Controls run the frozen selector and challengers the corrected
one, from the same source, on development seeds `0x5eed_e000..=0x5eed_e005` at
5,000 executions each. Acceptance requires the challenger's **play progress** to
be strictly greater than its paired control's on at least 4 of 6 seeds. Play
progress is M52's measurement and is primary; viable and recorded progress are
reported alongside for every arm. If it accepts, repeat unchanged on held-out
seeds `0x5eed_e100..=0x5eed_e105` with the same paired threshold, and any
promotion must replay exactly with no model.

The source is fixed by the same rule H54 used: the recorded archive with the
greatest play progress at the time of execution, ties by fewer retained entries
and then the smaller seed, resumed from the shortest input at its deepest play
bucket.

## Supersession lineage

- Supersedes the frontier-window behaviour of the parent scheduler frozen at H3
  and carried unchanged through every panel since. That scheduler is not
  withdrawn from the record; it remains the control in this panel and the
  executor of every result already recorded.
- Supersedes any reading of D27, D28 and the H21 through H27 panels that treats
  their frontier as a sample of the deepest states. It was a key-order slice,
  and at their depth the saturation had not yet occurred, so their recorded
  results stand as recorded while the inference about what was being expanded
  does not.
- Depends on M52 for its ordering semantics and on H45 for the retention rule
  that makes the tie classes live states rather than falls.

### H56 requirements for the mechanism builder

- Division of labour, recorded so the record shows who did what: this experiment
  wrote the registration above and owns the science. The mechanism is
  implemented by a separate builder in its own worktree against these
  requirements. **No selector code was written here**; a partial implementation
  begun before this ruling was reverted and the tree carries none of it.
- **Requirement one — the selection key, in M52 ladder terms.** Selection must
  rank by the corrected `(world, level, progress)` tuple, ordered
  lexicographically. A later pair always outranks an earlier one, and a larger
  progress inside an earlier pair never outranks a later pair, because progress
  is measured within the current pair and restarts when the pair advances. The
  four frozen named rungs must play no part in ranking; they remain in every
  report untouched.
- **Requirement two — principled tie handling.** The 128-entry sorted slice must
  go. The frontier is a tie class, not a slice: all active entries at the best
  `(world, level)` within a fixed band of the deepest progress in that pair,
  sampled uniformly. Classes are considered in descending order, and a class
  every member of which is exhausted must be **skipped** so the frontier falls
  through. That fall-through is a requirement, not an optimisation: on the
  recorded conquest archive the maximal tuple holds exactly one entry and it is
  a scripted sequence, so a selector that cannot fall through would spend three
  draws in four on a state that cannot produce.
- **Requirement three — per-entry accounting that starves exhausted parents.**
  Each entry must record how often it has been selected as a parent and how
  often a selection produced at least one retained descendant. A parent whose
  selections since its last retained descendant reach a fixed threshold is
  exhausted and is not sampled while any unexhausted member of its class
  remains. A retained descendant resets it. If every active entry is exhausted
  the counters reset deterministically rather than the search deadlocking, and
  that reset must be counted and reported.
- **Hard constraint — a new explicit policy beside the frozen one.** The
  corrected selector must be a new selector policy carried alongside the frozen
  one, in exactly the pattern the retention policy and the archive-key policy
  already use, selected per campaign and defaulting to frozen. Every recorded
  artifact in this experiment must continue to replay byte-exact on the frozen
  path. This is not stylistic: the entire evidence chain from H20 onward depends
  on byte-exact reproduction of recorded archives, and three separate gates in
  this log are byte-equality against recorded arms.
- **Gates the mechanism must clear before the acceptance arms run.** Inertness:
  with the frozen selector selected, a resumed arm reproduces a recorded arm
  byte for byte. Determinism: one corrected arm replays byte-identically from
  its recorded seed with no model. Quality: `cargo fmt --check`, `cargo clippy
  --all-features` with `-D warnings`, `cargo nextest run --all-features`, and
  `cargo deny check`. Honesty: the selection and novelty counters, the count of
  skipped classes and the count of counter resets are reported per campaign, so
  the claim that exhausted parents were starved is checkable from the record
  rather than asserted.
- The acceptance arms registered above return to this experiment once the
  mechanism merges.

### H56 result — rejected at one paired win of six

- The mechanism merged as a strict fast-forward from `746eedfa` to `5c73d677`
  and cleared every gate. G1, inertness: the M53 gate campaign reproduces its
  recorded SHA-256
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` on the
  merged tree, and a frozen-selector archive carries no selector field at all.
  G2, determinism: the corrected seed `0x5eed_e000` arm and its no-model replay
  are byte-equal with SHA-256
  `97cfb6700fcb0e0b717adf7818e974bbe547349133874d00a16fcf14ab273a47`. G3:
  formatting, Clippy with `-D warnings`, 83 tests run and 83 passed, and the
  dependency check. G4, accounting honesty: every corrected archive reports its
  policy and its counters.
- Paired play progress against the frozen selector, from the conquest archive at
  its deepest play bucket, seeds `0x5eed_e000..=0x5eed_e005` at 5,000 executions
  each. Controls `[124, 124, 124, 124, 124, 124]`. Challengers
  `[124, 124, 129, 124, 124, 124]`. **One paired win of six against a threshold
  of four. Rejected.** No held-out panel is due.
- The panel is a weak discriminator and the reason is mine. Eleven of twelve arms
  finished at exactly the play bucket they resumed from. The source reached that
  bucket only after roughly forty thousand executions inside the second world,
  so five thousand executions from it is a regime in which neither selector was
  going to move, and a rule that compares two selectors in a regime where
  nothing advances mostly measures noise. This is the same family of
  registration flaw H51 recorded — there the success count saturated, here the
  budget is too small for the measure to resolve anything — and it is recorded
  beside the registration rather than by amending it. The rejection stands on
  the registered rule.
- The action bound was not the constraint. No entry in any arm reached the
  512-action cap; the longest recorded input is 434.
- What the corrected selector demonstrably did change is worth recording,
  because it is not nothing and it is not the acceptance measure. It retains
  substantially more and dies substantially less: retained
  `[3333, 3438, 3303, 3359, 3379, 3450]` against the controls'
  `[2582, 2655, 2608, 2642, 2696, 2674]`, and deaths `[64, 69, 77, 71, 91, 93]`
  against `[138, 120, 123, 122, 115, 116]`. The archive shape differs the way
  tie-class sampling predicts: the control concentrates 797 of its entries in
  the single deepest bucket and leaves 121 through 123 thin, while the
  challenger spreads 659, 704, 552 and 500 across those four buckets. The
  frontier is being sampled instead of sliced.
- The exhaustion machinery never engaged. Across the corrected arms
  `classes_skipped` is zero and `counter_resets` is zero, because roughly
  sixty-eight percent of tie-class selections produced a retained descendant and
  no parent came near the sixty-four-selection barren threshold. The third of
  the three corrections is therefore **untested** by this panel: it was
  exercised by the builder's own gates but never fired in a real arm. Any future
  claim about starving exhausted parents needs a regime where parents actually
  exhaust, which means a longer budget or a harder frontier.
- The uniform draw behaved as designed and as diagnosed: about 1,250 of 5,000
  selections went to it, a quarter of the budget spread across the whole shelf.
  It remains unchanged and remains the recorded follow-up.
- Raw evidence: `target/smb-completion/h56-selection/` and
  `target/smb-completion/h56-progress/` on the ARM machine, with summaries and
  progress reports copied locally.

### D57 — measurement: the 125-to-143 span is neither a wall nor a stretch of terrain

- A recorded-artifact measurement dispatched by the integrator. It runs no
  search, changes no mechanism, involves no model, and retains nothing. It walks
  recorded inputs action by action, and at every action boundary inside a
  requested progress span it runs the three admission-probe masks from a
  snapshot and records how long each survived and which clause of the terminal
  condition ended it.
- **The span holds no terrain on the 144-reaching input.** That input, 351
  actions, produces exactly **one** action boundary at the deepest decoded pair:
  its last, at progress 144 and camera 2304. It never passes through 125 to 143
  because it never plays there.
- **Bucket 144 is not second-world terrain.** The deepest genuine play any
  recorded trajectory reached is the 422-action input ending at progress 124 and
  camera **1986**, which walks the second world continuously — 67 boundaries
  from progress 0 and camera 0 up to 124, every one of them admitted by the
  probe on all three masks. The 144 entry sits at camera 2304 with the previous
  level's scenery, reached by a **shorter** input, and the film already recorded
  it as the castle-completion sequence. A shorter input reaching a higher bucket
  at an unrelated camera is the signature of a stale camera during a level
  transition, which is exactly the caveat M52 attached to progress restarting
  when the pair advances.
- **The span is reachable and retainable.** Across thirteen probed archives —
  the twelve H56 arms and the ARM conquest — exactly one entry lies in 125 to
  143: the corrected seed `0x5eed_e002` arm at bucket **129**, camera 2071. That
  arm is also H56's single paired win. Walking its input, boundaries at 121,
  123 and 129 all survive the probe; at 129 the no-input and held-right masks
  survive the full horizon and only the button-plus-right mask dies, at frame 50,
  below the play area. The probe admits it.
- **Answer to the question asked.** The span is **not structurally
  unretainable**. There is no stretch where the player is committed and no-input
  always dies; on every boundary any recorded trajectory produced there, no-input
  survives the full probe horizon. The wall is not a retention-mechanism
  problem, and nothing here argues for changing the admission probe.
- What the gap actually is: the end of the deepest recorded trajectory, plus a
  transition artifact nineteen buckets above it that no play ever connects to.
  Progress 124 was never a barrier — it is where the recorded input stops. One
  corrected arm extended it to 129 in five thousand executions, which is
  ordinary search depth, not a crossing.
- Consequence for what gets registered next, recorded so the inference is not
  made later by accident: H56 resumed from 124 believing it faced a frontier,
  and the measure it was scored on could only move by extending an ordinary
  trajectory. Neither the selector nor the retention rule is implicated by this
  span. The open question stays what it was — depth in the second world — and
  the recorded budget flaw in H56 remains the first thing a re-test must fix.
- Raw evidence: `target/smb-completion/d57-span/`.

## H58 — preregistered selection re-test at a budget that can resolve it

- Falsifiable claim, unchanged from H56: the corrected selector reaches deeper
  play than the frozen one. Controls run the frozen selector and challengers the
  corrected one, from the same source, on development seeds
  `0x5eed_e000..=0x5eed_e005`. Acceptance requires the challenger's play
  progress to be strictly greater than its paired control's on at least 4 of 6
  seeds. On acceptance, repeat unchanged on held-out seeds
  `0x5eed_e100..=0x5eed_e105` with the same paired threshold.
- **The only change is the budget: 20,000 executions per arm, against H56's
  5,000.** The standing ceiling of twenty thousand is lifted for these arms by
  explicit integrator authorization, recorded here as C49's was.
- What changes at this budget, stated before execution. H56's own numbers are
  the argument: eleven of its twelve arms finished at exactly the bucket they
  resumed from, and the source archive needed roughly forty thousand executions
  inside the second world to reach that bucket. At five thousand neither
  selector could move, so the paired comparison mostly measured noise; at
  twenty thousand both can move, so it measures selection. D57 removed the
  competing explanation: the span above the resume bucket is not a wall, no
  boundary there fails the admission probe, and one H56 arm already crossed into
  it, so a longer budget is the right lever rather than a mechanism change.
- **The exhaustion machinery is expected to fire in a real arm for the first
  time.** H56 recorded `classes_skipped` zero and `counter_resets` zero, because
  about sixty-eight percent of tie-class selections produced a retained
  descendant and no parent approached the sixty-four-selection barren threshold.
  At four times the budget parents should begin to exhaust. The selector
  counters — tie-class selections, productive selections, classes skipped and
  counter resets — are reported prominently for every corrected arm **either
  way**. If they remain zero at twenty thousand executions that is itself a
  finding about the threshold, and it must be reported as such rather than
  omitted.
- Source, and a stated deviation from the mechanical rule. H56's source rule
  selects the recorded archive with the greatest play progress, which is now the
  corrected seed `0x5eed_e002` arm at play 129. This registration instead keeps
  **H56's own source**, the conquest archive at play bucket 124, for two
  reasons: it makes the two panels differ in exactly one variable, which is the
  point of a re-test; and it avoids sourcing a comparison between two selectors
  from an archive that one of them produced. The deviation is recorded here
  rather than taken silently.
- Gates, unchanged from H56. Inertness: the frozen path reproduces a recorded
  campaign byte for byte. Determinism: one corrected arm replays byte-identically
  from its recorded seed with no model. Quality: the four gates. Honesty: the
  selector accounting is reported per corrected arm.
- Execution: the ARM machine under the standing box rules, six arms concurrent,
  `nice -n 10`. Several hours are expected.
- Lineage: this supersedes H56's budget and nothing else. H56's rejection stands
  as recorded, at the rule it was registered under. Promotion on held-out
  acceptance triggers the standing ruling that the frozen selector path is
  deleted outright in the immediately following commit, with historical
  re-verification moving to checkout of the recording commits.
- Raw destination: `target/smb-completion/h58-selection/`.

## Standing method — cheapest test first

- Recorded as method on the integrator's instruction, and binding on every
  registration after the H58 re-test. It exists because two expensive panels
  were lost to assumptions that a free measurement would have caught: H54's arms
  drew from a frontier window that was an arbitrary key-order slice, and H56's
  arms ran at a budget where neither selector could move. Both defects were
  checkable against recorded artifacts at registration time, before a single arm
  launched.
- **One — assumption checks before arms.** Every registration lists its
  load-bearing assumptions explicitly. The recurring ones are that the frontier
  is real, that the measure can move in the regime being run, and that the key
  or filter being relied on still discriminates at this depth. Every assumption
  that is checkable against recorded artifacts is checked, and its result is
  recorded in the registration, before any arm launches. An assumption that
  cannot be checked for free is stated as an exposure instead.
- **Two — pilot before fleet.** One arm at the registered budget must
  demonstrate the measure can move before the remaining arms launch. A pilot
  that cannot move voids the panel at a twelfth of its cost. The pilot is part
  of the panel and its seed is fixed in the registration, not chosen afterwards.
- **Three — preregistered early stopping.** Multi-seed panels state an honest
  stop rule in advance. The worked example: stop after three exactly-tied pairs
  and record the panel as unresolvable in this regime. The rule is registered so
  that stopping is neither peeking nor theatre — a panel that has already shown
  it cannot discriminate is halted and recorded as such rather than run to a
  full count for appearances.
- These three are conditions on registration and execution, not on judgement.
  They do not relax any acceptance rule, do not permit an acceptance threshold
  to move after the fact, and do not licence reading a result before its
  registration is complete.
- The H58 re-test proceeds unchanged. Its regime is already evidence-validated:
  D57 recorded one arm reaching bucket 129, above the resume bucket, so the
  measure is known to be able to move there.

### H58 result — rejected at one paired win of six, again

- Gates first. Determinism: the corrected seed `0x5eed_e000` arm and its
  no-model replay are byte-equal with SHA-256
  `ce11b5988d7e9781d7f9a7457088633610cc49004ed2004358122fe1b8db472a` and the
  summary records `replay_verified=true`. Inertness: a frozen-selector archive
  carries no selector field. Quality gates pass. Accounting is reported for
  every corrected arm.
- Paired play progress at 20,000 executions per arm. Controls
  `[124, 124, 124, 124, 124, 124]`. Challengers
  `[124, 124, 145, 124, 124, 124]`. **One paired win of six against a threshold
  of four. Rejected.** No held-out panel is due and the standing deletion ruling
  does not fire.
- Quadrupling the budget did not change the verdict, and the shape of the
  failure is sharper than H56's. **Not one control arm moved a single bucket in
  120,000 executions**, and five of six challengers did not either. The only
  arm that advanced is seed `0x5eed_e002`, which is the same seed that produced
  H56's single win: 129 at five thousand executions, 145 at twenty thousand. The
  win is reproducible and seed-specific, not budget-driven.
- The 145 result is genuine play, checked rather than assumed. Walking that
  arm's input, the boundaries run 123 at camera 1970, 129 at 2071, 129 at 2079,
  140 at 2250, 141 at 2270, 144 at 2312, 144 at 2316 and 145 at 2320 — a
  monotone camera through continuous terrain. Its archive populates the span
  D57 examined: four entries at 129, three at 130, one at 131, two at 140, 168
  at 141, 223 at 142, 76 at 143, 50 at 144 and two at 145.
- A refinement to D57, recorded because it corrects a reading rather than the
  finding. Bucket 144 occurs in two physically different situations that happen
  to share a camera value: the conquest archive's 144 sits at camera 2304 during
  the previous level's completion sequence, while this arm's 144 sits at camera
  2312 in second-world play. D57's conclusion that the conquest 144 is a
  transition artifact stands; the additional fact is that real terrain also
  passes through that bucket.
- The three-mask admission probe earned its design at the tip. At the deepest
  two boundaries the **no-input** mask dies — below the play area at frames 27
  and 40 — and the button-plus-right mask dies to the kill state, while
  held-right survives the full horizon. A single no-input probe, which is what
  H45 registered against, would have refused exactly the states that reach 145.
- **The exhaustion machinery still never fired, and the arithmetic says it
  cannot.** Across all six corrected arms `classes_skipped` is zero and
  `counter_resets` is zero. The reason is the size of the tie class. On a
  stalled arm the class holds 5,749 and 6,487 members against about 15,000
  tie-class draws, which is **2.6 and 2.3 draws per parent** against a barren
  threshold of 64 — unreachable by a factor of twenty-five. On the arm that
  advanced the class is 521 members and 28.6 draws per parent, still under the
  threshold but an order of magnitude closer.
- That is the finding this panel bought, and it is about the correction itself.
  The frozen selector concentrated three draws in four on an arbitrary
  128-entry slice. The corrected selector removed the arbitrariness but replaced
  it with a tie class of five to six thousand members, so per-parent effort fell
  to roughly two and a half selections in twenty thousand executions. Sampling a
  band uniformly at this depth spreads effort as thinly as the uniform draw it
  was meant to improve on. Whether the small class on the advancing arm is cause
  or consequence of advancing is **not** established here: advancing lifts the
  deepest bucket and the band follows it, discarding the shallow population, so
  the two are confounded in this data.
- Recorded against the new standing method: this panel ended with five exactly
  tied pairs. The stop rule the method gives as its worked example — halt after
  three exact ties and record the panel unresolvable in regime — would have
  stopped it after roughly half the arms. H58 was registered before the method
  took effect and the integrator directed it to proceed unchanged, so it ran to
  a full count; the next multi-seed panel carries a stop rule.
- Raw evidence: `target/smb-completion/h58-selection/` and
  `target/smb-completion/h58-progress/` on the ARM machine, with summaries and
  progress reports copied locally, and the walked boundaries at
  `target/smb-completion/d57-span/corrected-e002-145-boundaries.json`.

## H59 — preregistered concentration correction

- First registration under the standing cheapest-test-first method, and its
  assumption checks are recorded below **before** any arm launches. One of them
  fails, and the failure is recorded here rather than discovered by a fleet.
- Falsifiable claim: concentrating frontier draws on recently discovered members
  of the winning tie class reaches deeper play than spreading them across the
  whole class. Mechanism as dispatched: within the winning tie class, sample
  uniformly from only its `K` most recently retained members, with retention
  order taken as entry creation order so the choice is deterministic and free of
  key-order accident, and `K` fixed at 128. Band, fall-through, and the
  sixty-four-selection barren threshold are unchanged.

### Assumption checks, run against recorded artifacts before any arm

- **Assumption one: the measure can move in this regime. HOLDS.** H58's seed
  `0x5eed_e002` reached play bucket 145 from the same source and budget, with a
  monotone camera through continuous terrain, and D57 established that the span
  above 124 is not a wall. A twenty-thousand-execution arm from this source can
  move.
- **Assumption two: the frontier is real. HOLDS.** D57 walked the deepest
  recorded trajectory and found every boundary probe-admissible, with no
  committed stretch where no-input always dies. Bucket 124 is where the recorded
  input stops, not a barrier.
- **Assumption three: capping the class at 128 makes the barren threshold of 64
  reachable. FAILS, by a factor of about twenty-five.** The dispatched
  arithmetic divides roughly fifteen thousand tie-class draws by a window of 128
  and obtains about 117 draws per parent. That assumes the 128 members are a
  fixed set. They are not: a recency window turns over once per retention into
  the class, so every class member passes through it. Measured on H58's own
  archives, the band on stalled seed `0x5eed_e000` holds 5,749 members of which
  **5,740 were created during the run**, and on seed `0x5eed_e004` 6,487 of
  which 6,478 were. Fifteen thousand draws spread across 5,740 parents passing
  through the window is **2.6 draws per parent**, which is exactly the figure
  H58 already recorded for sampling the whole class, and 2.3 on `e004`. On the
  advancing seed `0x5eed_e002` the band is 521 members, all created in-run, and
  the figure is 28.6 either way.
- Consequence, stated plainly: **a recency-ordered cap does not change average
  draws per parent at all.** It changes when a parent's draws arrive — in a
  burst shortly after its retention — and which parents receive them. That may
  be worth testing on its own merits, and the pilot below tests it. But the
  stated rationale that the cap makes exhaustion reachable does not survive
  contact with the recorded data, and no arm needed to be spent to find that out.

### The fork this exposes, for the integrator's ruling

- The 117-draws figure is correct under a different eviction rule: a window that
  holds `K` parents and releases one **only when it exhausts**, rather than when
  a newer retention displaces it. Under that reading the resident set is nearly
  static, a parent accumulates draws until it reaches the barren threshold, and
  exhaustion genuinely becomes reachable.
- These are two different mechanisms with different predictions, and the
  registration does not silently choose between them. **Recency-displacement**
  concentrates in time and tests whether recently discovered states are better
  parents. **Exhaustion-eviction** concentrates in population and tests whether
  starving barren parents advances the frontier. The builder specification must
  say which is being built; the requirements below are written for both and
  marked.

### Pilot before fleet

- One arm, corrected plus capped, seed `0x5eed_e000`, at 20,000 executions,
  from H58's source at play bucket 124. That seed stalled at exactly 124 in both
  H56 and H58, under both selectors, so it is the sharpest available test.
- Preregistered pilot question: **does a twice-stalled seed advance past play
  bucket 124 under concentration?** If it moves, the paired fleet runs. If it
  does not, this registration voids at one twelfth of the fleet's cost and is
  recorded as such.
- The pilot's seed is fixed here and is not chosen after the fact.

### Fleet, on a moving pilot

- Paired as before: frozen controls against corrected-plus-capped challengers,
  seeds `0x5eed_e000..=0x5eed_e005` at 20,000 executions, acceptance requiring
  the challenger's play progress strictly greater than its paired control's on
  at least 4 of 6. Held-out `0x5eed_e100..=0x5eed_e105` on acceptance.
- **Stop rule, preregistered:** the fleet halts after three exactly tied pairs
  and is recorded as unresolvable in this regime. H58 ended with five such
  ties and ran to a full count only because it predated the method.
- Promotion on held-out acceptance triggers the standing deletion ruling for the
  frozen selector path.

### Builder requirements

- **No selector code is written in this experiment's tree.** The mechanism is
  built by a separate builder, as with H56.
- The cap is a **new explicit policy value beside the existing frozen and
  corrected ones**, defaulting to the existing behaviour, in the same pattern as
  the retention, key, ladder and selector policies. Every recorded artifact must
  continue to replay byte-exact on the frozen path and on the uncapped corrected
  path; three gates in this log are byte-equality against recorded arms.
- Under **recency-displacement**: the sampled set is the `K` members of the
  winning tie class with the greatest entry identifiers, `K` fixed at 128;
  membership is recomputed per draw; a member leaves when 128 newer class
  members exist. Barren accounting is unchanged.
- Under **exhaustion-eviction**: the sampled set is a resident set of at most
  `K` class members admitted in entry-identifier order; a resident leaves only
  when its barren count reaches the threshold, and a new member is admitted in
  its place. Barren counts persist for residents across draws.
- Accounting must report, per campaign, the sampled-set size, how many distinct
  parents passed through it, the draws per parent it produced, and the existing
  skipped-class and counter-reset counts. The draws-per-parent figure is the
  number the assumption check above turned on, so it must be measurable from the
  record rather than recomputed by hand.
- Gates before the pilot: inertness on both existing paths, one corrected arm
  replaying byte-identically, the four quality gates, and the accounting
  reported.
- Raw destination: `target/smb-completion/h59-concentration/`.

### H59 pilot amendment — campaign-mode arm of record

- Amended **before execution**, on the integrator's latency directive. Nothing
  had run against the pilot as first registered; the serial arm was never
  launched, so this is a revision rather than an alteration of a frozen
  registration.
- The pilot becomes one live concentrated-policy **campaign** arm on the ARM
  machine at all twelve cores, 20,000 executions, campaign seed derived from
  `0x5eed_e000`, with the recorded stream as the arm of record and its serial
  replay as the determinism evidence. Roughly thirty-five minutes of wall time
  instead of two and a half hours. The pilot question is unchanged in substance:
  does the concentrated policy advance play past bucket 124 from the source
  where the frozen scheduler massed at 124 across the conquest run's 50,000
  executions and H58's 120,000 control executions?
- **Assumption check run before launching, and it failed.** The campaign engine
  resolves an archive origin with `select_frontier_resume_input`, which takes the
  shortest input at the maximum recorded `(world, level, progress)`. On the
  conquest archive that is entry 7495 at bucket **144** with 351 actions — the
  scripted castle-completion state D57 identified — not entry 17324 at bucket
  **124** with 422 actions, which is what H56 and H58 resumed from. Launching the
  campaign against the conquest archive directly would have compared a
  concentrated arm starting from a cutscene against serial arms starting from
  play, and the pilot question would have been unanswerable.
- Correction, recorded as a derived artifact rather than a mechanism change: the
  pilot's origin is a single-entry archive holding exactly the entry the serial
  arms resumed from, entry 17324. The campaign's own frontier rule then selects
  that entry trivially and provably. The derived origin is
  `target/smb-completion/h59-pilot/origin-play124.json` with SHA-256
  `e6373e65b0b324bf1df76b13f3c58bdcce991089720454f5c9c22d2c146cb40e`, and its
  resume input matches the serial arms'. The campaign uses an origin only to
  choose a resume input, so nothing else about the source is in play.
- The pilot's decision rule is unchanged: if it advances past 124 the paired
  fleet runs with the three-exact-ties stop rule; if it does not, the
  registration voids.

### Standing method amendment — arms of record may be recorded campaigns

- Recorded on the integrator's instruction, alongside cheapest-test-first.
- An arm of record **may** be a recorded campaign rather than a seed-pure serial
  run, subject to the same discipline: preregistered, one live run per
  registered slot, **no retries**, replay-exactness of the recorded stream as
  the determinism gate, and pairing by campaign seed.
- Future fleets default to campaign arms split across both machines.
- The one-live-run-no-retries clause is the load-bearing part: a campaign arm's
  value as evidence rests on it being the run that was registered, not the best
  of several.
- **Superseded within this entry, on a further instruction and recorded plainly
  rather than by editing the sentence above.** Serial execution is dropped
  entirely for arms of record. There is no seed-derivability carve-out: every
  registered arm from here is a recorded campaign, including where a serial arm
  would be more convenient. Serial re-execution survives only as what replay
  verification inherently is — the replay of a recorded stream is a serial
  re-execution and remains the determinism gate. Small recorded-artifact
  diagnostics, such as the span walks and the progress measurements, are
  unaffected and stay as they are.

### H59 pilot result — the twice-stalled seed advanced, decisively

- The registered pilot question was: does the concentrated policy advance play
  past bucket 124 from the source where the frozen scheduler massed at 124?
  **It does.** One live concentrated campaign, 20,000 executions on twelve
  cores, campaign seed derived from `0x5eed_e000`, from the derived
  play-bucket-124 origin.
- The recorded run completed 20,000 executions, retained 12,662 entries against
  11,344 rejected with 946 deaths, emulated 4,353,601 frames, and its stream
  hashes to `cdfed5f0edda6819073b4f23da9821fc826ff8856682f9ceae4678800ab03628`.
- Its extended ladder reaches `(world 1, level 0, progress 197)` and then a new
  pair, `(world 1, level 1)`, first retained at execution 19,674 with 233
  entries. The origin's deepest play bucket was 124.
- The advance is dense, not a single artifact entry. Above bucket 120 the
  second-world pair holds 892, 1048, 755 and 820 entries at 121 through 124,
  then 195, 241, 198, 138, 161, 81 and 54 across 126 through 132, and continues
  through 136 to 147, 155, 156, 168, 174 to 184, 192, 193 and six entries at
  197. This is a populated corridor, not a lucky boundary.
- The comparison set, all from recorded artifacts. Serial arms with the frozen
  selector from this exact origin: six arms, 120,000 executions, **every one at
  exactly 124** (H58). Serial with the corrected uncapped selector from the same
  origin: five at 124, one at 145. Campaign mode with the frozen selector at
  50,000 executions: a shelf topping out at 124 with 1,513 entries there and an
  empty span above it (CM6, recorded in the campaign-mode log). Campaign mode
  with the concentrated selector at 20,000 executions: **197 and a new level.**
- **The mechanism did not work the way its rationale said, and the assumption
  check predicted exactly that.** The recorded concentration accounting is
  window cap 128, final window size 128, 15,129 window draws, **10,281 distinct
  parents through the window**, and 1.471 draws per parent — *lower* than the
  2.6 the uncapped corrected selector recorded, because campaign mode retains
  more and so churns the window faster. `classes_skipped` and `counter_resets`
  are both zero again: exhaustion still never fires, at any budget, under any
  selector yet built.
- What the cap changes is what the registration's assumption check said it would
  change: not draws per parent, but **which** parents receive them. Every
  frontier draw goes to one of the 128 most recently retained members instead of
  being spread across a band of five to six thousand, most of which are old and
  shallow. The recency half of the rationale is supported; the exhaustion half
  remains inert and untested in a real arm across three panels.
- Stated exposure, because it bounds what this pilot licenses: no campaign-mode
  arm exists with the corrected **uncapped** selector. The pilot therefore
  separates concentrated-campaign from frozen-campaign and from both serial
  selectors, but it does **not** separate the recency cap from the tie-class
  correction under campaign mode. A fleet that wants that separation needs the
  uncapped corrected policy as its control rather than the frozen one.
- Determinism gate in flight at the time of writing: the serial replay of the
  recorded stream is running and its result is recorded below when it lands. The
  arm of record is the recorded stream; the replay verifies it, and no retry was
  taken or is permitted.
- Raw evidence: `target/smb-completion/h59-pilot/` on the ARM machine.

## H60 — preregistered attribution arm

- The integrator applied the pilot-before-fleet rule to their own fleet dispatch
  and replaced it with a single attribution arm. Registered before execution;
  nothing has run against it.
- Falsifiable question, fixed here: **does the uncapped corrected policy under
  campaign mode also advance well past bucket 124, or does it stall near the old
  shelf?** The H59 pilot showed a concentrated campaign reaching 197 and a new
  level, but no campaign arm has ever run the corrected policy without the cap,
  so the pilot could not separate the recency cap from the tie-class correction.
  This arm separates them.
- One live uncapped-corrected campaign, one run, **no retries**: the derived
  play-bucket-124 origin
  `target/smb-completion/h59-pilot/origin-play124.json`, SHA-256
  `e6373e65b0b324bf1df76b13f3c58bdcce991089720454f5c9c22d2c146cb40e`, 20,000
  executions, campaign seed derived from `0x5eed_e000`, twelve workers on the
  ARM machine. Every one of those matches the pilot exactly; the selector is the
  only difference, which is the point.
- Decision rule, fixed before execution. If it stalls at or near the recorded
  shelf — anywhere in 124 to 145 — attribution to the cap is settled and the
  concentrated policy proceeds to lean confirmation: held-out seeds
  `0x5eed_e100..=0x5eed_e105` as concentrated campaign arms, paired against this
  recorded attribution arm and the H58 evidence, with **no fresh development
  fleet**. If it also surges toward 197, execution stops and the result is
  reported for a fresh ruling, because the mechanism story would then be about
  the tie-class correction under campaign mode rather than the cap.
- The recorded stream is the arm of record and its serial replay is the
  determinism gate, per the standing method.
- Raw destination: `target/smb-completion/h60-attribution/`.

## C61 — registered concentrated conquest

- Observational, in the C49 pattern: no falsifiable claim, no control, no
  acceptance rule. The deliverable is the recorded archive, the extended
  milestone ladder, and film of the furthest trajectory.
- One live concentrated-policy conquest campaign, 50,000 executions, on the
  local machine at its ten cores, sourced from the H59 pilot's own recorded
  archive `target/smb-completion/h59-pilot/run/archive-live.json` — the run that
  reached `(world 1, level 0, progress 197)` and opened `(world 1, level 1)`.
  One live run, no retries, replay as the determinism gate.
- Property of the origin recorded before launch, because D57 taught this lesson:
  the source's maximum tuple is `(world 1, level 1, progress 0)` with 233
  entries, and the play measurement returned no play bucket there, which is the
  signature of a level-transition moment rather than settled play. The campaign
  resume rule takes the shortest input at that maximum tuple, so this conquest
  resumes from the transition into the new level. For an observational conquest
  that is acceptable and is how CM6 resumed; it is recorded so the ladder is not
  later misread.
- Science and territory proceed simultaneously on separate machines by the
  integrator's direction. Both this and H60 are conditional on the H59 pilot
  replay verifying first.
- Raw destination: `target/smb-completion/c61-conquest/`.

### H60 result — the uncapped corrected policy stalls at the shelf; attribution settled

- Recorded as **observational-unverified**: the live run completed and its ladder
  was derived, and its serial replay was stopped in flight by integrator ruling
  once the ladder had answered the question. The recorded stream hashes to
  `2dff73dbd6d6c9e7f86c8e89b495dc656dcbea49042fd596c8305fec9d6b48c4` and the run
  is reproducible on demand from it, but no replay verdict was produced, so this
  arm carries less evidential weight than a verified one. That is stated rather
  than glossed.
- Twenty thousand executions, twelve workers, the derived play-124 origin,
  campaign seed derived from `0x5eed_e000` — every parameter identical to the
  H59 pilot except the selector.
- **Its ladder stops at `(world 1, level 0, progress 124)`.** Not 125, not 145:
  exactly the shelf, exactly where 170,000 frozen-selector executions and
  140,000 uncapped-corrected serial executions had already stopped. The
  concentrated pilot from the same origin, seed and worker count reached 197 and
  opened a new level.
- Its selector accounting reads 4,938 uniform selections, 15,118 tie-class
  selections, 6,695 productive, and — for the third panel running —
  `classes_skipped` zero and `counter_resets` zero.
- The registered decision rule fires as written: a stall anywhere in 124 to 145
  settles attribution to the cap. **Attribution is settled.** The advance is the
  recency window, not the tie-class correction, and not campaign mode.

### Promotion of the concentrated selector, by integrator ruling

- The concentrated recency policy becomes the only selector. The record cited:
  170,000 frozen-selector executions and 140,000 uncapped-corrected executions
  immobile at bucket 124 from this origin, against a single 20,000-execution
  concentrated arm reaching progress 197 and opening `(world 1, level 1)`, with
  that arm's determinism replay verified byte-identical.
- Per the standing ruling this promotion is followed immediately by deleting the
  frozen and uncapped-corrected selector paths outright. The consequence is
  recorded plainly because it is a real loss: **campaigns recorded under those
  selectors no longer reproduce on head.** Every such artifact — the M53
  inertness reference, the H45 and H51 byte-equality gates, the H56 and H58
  arms, CM6 — reproduces only by checking out the commit that recorded it. The
  identity and inertness references are re-frozen against the concentrated path
  on the new tree.
- What is not in this change, on the integrator's instruction: nothing about
  exhaustion. `classes_skipped` and `counter_resets` have now been zero across
  four panels and every selector ever built, so the exhaustion machinery has
  never fired in a real arm. It is an open mechanism question for after
  promotion and no exhaustion change is folded in here.
- Lineage: supersedes the parent scheduler frozen at H3 and carried through
  every panel to H58, and supersedes the uncapped corrected selector promoted
  into the tree at H56 and never accepted by a panel. H59's pilot is the
  accepting evidence; H60 is the attribution.

### Deletion of the frozen and uncapped-corrected selectors

- Executed on the integrator's ruling, in the commit immediately following the
  promotion record. `SmbArchiveSelectorPolicy` now has one variant,
  `ConcentratedRecency`, and it is the default.
- Deleted outright: the frozen `choose_parent` with its `milestone_key` sort and
  its 128-entry window; the `FRONTIER_WINDOW` constant; `frontier_quality`; the
  uncapped tie-class draw branch; the `experimental_search` and
  `selector_policy` fields of the archive, both dead once the frozen path went;
  the two command-line modes that named the deleted selectors; and the header
  identifiers `frozen_frontier_128` and `corrected_tie_class`, which a recorded
  stream can no longer resolve.
- Two unit tests were retired rather than repaired because the behaviour they
  pinned no longer exists: the frozen-wrapper byte-identity test and the
  frozen-campaign no-selector-fields test. The remaining selector tests were
  repointed at the sole policy and now assert that concentration accounting is
  present rather than absent. 84 tests run, 84 pass.
- Gates on the deleted tree: `cargo fmt --check`, `cargo clippy --all-features`
  with `-D warnings`, 84 tests, `cargo deny check` all clean.
- **Reference re-frozen.** The M53 inertness reference
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` measured
  the frozen path and is now unreproducible on head. The same campaign — seed
  `0x5eed_ef00`, 256 executions, the conquest archive, probing retention,
  extended ladder — under the concentrated selector is
  **`fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`**, and
  that is the standing inertness reference from this commit forward.
- The cost, stated once and plainly: **every campaign recorded before this
  commit reproduces only at the commit that recorded it.** That includes the
  M53 reference, the H45 and H51 byte-equality gates, all H56 and H58 arms, CM6,
  the C49 conquests, and the H59 pilot and H60 attribution arms. Their recorded
  artifacts and hashes stand as evidence; re-verification is by
  `git checkout` of the recording commit, which is exactly what the standing
  ruling intends.

## HANDOFF

This session is retired here. The successor owns the program from its first
message. Everything below is state, not advice.

### Tree

- Branch `codex/smb-completion`, head `976fd0e8`, working tree clean, **nothing
  has ever been pushed** on this branch. Gates on head: `cargo fmt --check`,
  `cargo clippy --all-features` with `-D warnings`, 84 tests, `cargo deny check`
  — all clean.
- The concentrated recency selector is the only selector. The frozen and
  uncapped-corrected paths are deleted. The standing inertness reference is
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24` — seed
  `0x5eed_ef00`, 256 executions, the C49 conquest archive, probing retention,
  extended ladder, via `archive-resume-frontier-viable-ladder`.
- Promoted stack in force: corrected terminal condition (M35), probing retention
  at admission (H45), extended ladder (M52), concentrated recency selector
  (H59/H60). Rejected and not in force: the vertical-page archive key (H51), two
  generated rankings (H27, H44), the horizontal-position field (closed null,
  D29–D48).

### Running now

- **C61, local machine.** Concentrated conquest, campaign seed `0x5eed_c003`,
  ten workers, 50,000 executions, origin the H59 pilot archive. **Live run
  finished**: stream SHA-256
  `3e969f0f63930e606dbc4edb626455df9a2d639a95d9171a507b9473bb57fa1b`, ladder
  written, maximum tuple `(world 1, level 1, progress 27)` — it advanced inside
  the second world's second level. Its serial replay is running as PID 77064 and
  is the determinism gate; expect roughly three to four hours from its start.
  Evidence at `target/smb-completion/c61-conquest/`; log at
  `target/smb-completion/logs/c61.log`, which prints `C61_REPLAY_EXIT` and
  `C61_DONE`.
- **C62, ARM machine.** Concentrated conquest, campaign seed `0x5eed_c004`,
  twelve workers, 50,000 executions, same origin. Live run in progress as PID
  229090 under wrapper 229085. Evidence at
  `/root/harmony-smb-goal/dissonance-v2/target/smb-completion/c62-conquest/`;
  log at `/root/harmony-smb-goal/c62.log`, which prints `C62_RUN_EXIT`,
  `C62_LADDER_DONE`, `C62_REPLAY_EXIT`, `C62_DONE`. Expect roughly an hour live
  and several more for its replay.
- No watcher processes of mine survive; the successor should start its own.
  Neither conquest has film yet — film is the outstanding deliverable for both,
  per C49 practice: `smb-film archive-key <archive> <world> <level> <progress>
  <out>` then `ffmpeg` at framerate 60.

### Open threads, in the order I would take them

1. **Conquest completion.** Both replays must land verified before either
   conquest is evidence. If a replay fails, that conquest is void — one live
   run per slot, no retries, per the standing method.
2. **Film gates.** Neither C61 nor C62 has film. C49's practice is film of the
   furthest trajectory plus a second film of the deepest genuine play where they
   differ. They differ often: see D57 and the H58 refinement.
3. **Exhaustion inertness.** `classes_skipped` and `counter_resets` have been
   zero in every real arm across four panels and every selector ever built. The
   sixty-four-selection barren threshold has never once fired. The integrator
   deferred this deliberately and it is the largest untested piece of promoted
   machinery.
4. **Track-2 ladder** at `steers/TRACK2-STRATEGIC-CALLS.md`, outside this
   worktree. I never read or touched it.
5. **H54 ranking re-test.** The instrumentor ranking
   `archive-ranking-changed-state` was accepted, validated and never fairly
   measured — its panel was voided on defective substrate, not on its merits.
   Its recorded decision, source hash
   `2a27043379213d4eb73359b883514105c26929bab68e7db24c6a212c35aa9e4a`, journal
   and validators are intact at
   `/root/harmony-smb-goal/dissonance-v2/target/smb-completion/h54-luna/`, so a
   re-test on the concentrated selector costs **no model call**: the arm phase
   rebuilds from the recorded decision.

### Landmines not already in the log

- **The campaign engine's resume rule is not the serial one.**
  `select_frontier_resume_input` takes the shortest input at the maximum
  recorded tuple; the serial play-bucket mode takes the deepest bucket that
  answers the controller. On archives whose maximum tuple is a level-transition
  artifact these differ, and the difference silently voids a comparison. This
  caught me once. The fix used here is a derived single-entry origin archive;
  see the H59 pilot amendment.
- **The executor-identity binary's exit code is composed from a superseded
  frame-reduction criterion.** It exits non-zero even when ruling-level
  acceptance passes. Judge its report contents, never its exit status.
- **`pgrep -f` and `pkill -f` self-match wrapper argv on the ARM box.** Kill by
  explicit process identifier only; a pattern kill takes down the harness that
  issued it.
- **`git checkout` of a recording commit is now the only way to reproduce any
  pre-`976fd0e8` campaign.** Nothing before that commit replays on head.
- **The `/tmp` path is `/private/tmp` on this machine and is off limits.** All
  scratch belongs under `target/smb-completion/`.
- **Long `ssh` commands time out at the tool boundary and get backgrounded**,
  which can look like a failure while the remote job runs on happily. Verify
  remote state directly before concluding anything died.

The successor session takes the program from here. The standing direction,
ruled at take-over: the conquest chain is the program — film, verify, relaunch
from the deeper archive, a resume-rule assumption check before every link, and
one variable of science only when a link stalls.

### C61 live result — the conquest entered the second world's second level

- The live run completed its full 50,000 executions on ten workers, campaign
  seed `0x5eed_c003`, from the H59 pilot archive: 15,755 retained against
  38,642 rejected with 779 deaths, 9,807,356 frames emulated, 183 duplicates
  skipped and 423 probe refusals. The recorded stream hashes to
  `3e969f0f63930e606dbc4edb626455df9a2d639a95d9171a507b9473bb57fa1b`.
- Its extended ladder holds six `(world, level)` pairs — the first world's four
  levels at deepest progress 195, 195, 148 and 144, the second world's first
  level at 197 — and a maximum tuple of **`(world 1, level 1, progress 27)`**:
  the second world's second level, the deepest recorded play in the program's
  history. All six pairs carry first execution zero because the origin archive
  already contained them; the depth inside the new level is this run's work.
- The advance is dense in the C49/H59 sense, not a boundary artifact: the new
  level holds 6,960 entries, with a populated corridor of 794, 857, 775, 803,
  759 and 917 entries across buckets 20 through 25, then 296 at 26 and 446 at
  the frontier bucket 27.
- Selector accounting, reported per the standing honesty gate: 12,538 uniform
  selections, 37,645 tie-class selections, 13,387 productive. Concentration:
  window cap 128, final window size 128, 37,645 window draws over 3,165
  distinct parents — **11.894 draws per parent**, an order of magnitude above
  the pilot's 1.471, because a conquest that stalls inside one level churns
  the window far more slowly than a run sweeping fresh territory.
  `classes_skipped` and `counter_resets` are zero for the fifth panel running;
  the exhaustion machinery has still never fired, now at 11.9 draws per parent
  against the barren threshold of 64.
- The play measurement on the recorded archive answers the next link's
  resume-rule assumption check before anything launches: at the maximum pair
  the recorded, viable and play figures are all **27** — the frontier is
  settled genuine play, not a transition artifact, so the campaign resume rule
  and the play frontier coincide on this archive and the next link may resume
  from it directly. Measurement at
  `target/smb-completion/c61-conquest/play-measurement.json`.
- Film of the furthest trajectory is cut and delivered: the shortest input at
  `(1, 1, 27)`, 510 actions rendered to 511 frames and encoded at sixty frames
  a second, `target/smb-completion/c61-film/`. The play and viable figures
  coincide, so C49 practice calls for the one film.
- The determinism gate is in flight at the time of writing: the serial replay
  of the recorded stream is running and its verdict is recorded below when it
  lands. One live run, no retry taken or permitted.

### C61 replay verdict — verified

- The serial replay of the recorded stream completed all 50,000 executions and
  is byte-identical: `replay_verified` true, replay stream SHA-256
  `3e969f0f63930e606dbc4edb626455df9a2d639a95d9171a507b9473bb57fa1b` equal to
  the live stream, replayed archive SHA-256
  `4d1c28f1ced20c0f62abb94c0aec69176b275c8d9b0b62c792db5240d4d2a176`. C61 is
  complete evidence: recorded, verified, measured and filmed. Verdict at
  `target/smb-completion/c61-conquest/replay-verdict.json`.

## C63 — registered concentrated conquest, third link of the chain

- Observational, in the C49/C61 pattern: no falsifiable claim, no control, no
  acceptance rule. The deliverables are the recorded archive, the extended
  ladder, and film.
- One live concentrated-policy conquest campaign, 50,000 executions, on the
  local machine at its ten cores, campaign seed `0x5eed_c005`, sourced from
  C61's verified archive `target/smb-completion/c61-conquest/archive-live.json`,
  SHA-256 `4d1c28f1ced20c0f62abb94c0aec69176b275c8d9b0b62c792db5240d4d2a176`.
  One live run, no retries, serial replay of the recorded stream as the
  determinism gate.
- Choice of origin under the standing chain ruling — relaunch from the deeper
  archive: C61's ladder tops at `(world 1, level 1, progress 27)`, verified;
  C62's live ladder tops at `(world 1, level 1, progress 25)`, two buckets
  shallower, its replay still in flight on the ARM machine. C61 is the deeper
  archive and the only verified one, so the chain resumes from it. If C62's
  replay later verifies it remains evidence, but it does not seed this link.
- Resume-rule assumption check, run against the recorded artifact before
  launch and recorded in the C61 result above: the play measurement at the
  origin's maximum pair returns recorded 27, viable 27, play 27. The frontier
  is settled genuine play, so `select_frontier_resume_input` — the shortest
  input at the maximum recorded tuple — resumes from the same state family the
  film shows, and no derived origin is needed. The D57/H59 failure mode, a
  resume rule landing on a level-transition artifact, is checked absent here.
- Raw destination: `target/smb-completion/c63-conquest/`.

### C62 result — verified; the independent seed lands two buckets shy

- The ARM machine's conquest, campaign seed `0x5eed_c004`, twelve workers,
  50,000 executions from the same H59 pilot origin as C61: 15,255 retained
  against 36,782 rejected with 1,017 deaths, 9,444,262 frames emulated, 1,088
  duplicates skipped and 544 probe refusals. Live stream SHA-256
  `83c50bb9b625b995b79c5cc0ab50ef88bd506df9c7cc84717f382394fd70e9f6`.
- **The serial replay verified byte-identical**: `replay_verified` true, replay
  stream equal to the live stream, replayed archive SHA-256
  `1f66a92898fffb9dcb850b3734185ab7796f145b7359dbd0b266e9b797646762`. Verdict
  at `target/smb-completion/c62-conquest/replay-verdict.json` on the ARM
  machine.
- Its ladder holds the same six pairs as C61 with maximum tuple
  `(world 1, level 1, progress 25)`. The play measurement returns recorded 25,
  viable 25, play 25 — settled genuine play, so one film. The new level holds
  3,839 entries with its densest buckets at 358 to 413 across 18 through 20
  and 328 at the frontier bucket 25.
- Two independent campaign seeds from the same origin reached 27 and 25 in the
  second world's second level, so the depth is behaviour of the promoted stack
  and not a lucky seed — the C49 replication pattern, one level deeper.
- Selector accounting: 12,919 uniform selections, 38,169 tie-class selections,
  12,665 productive. Concentration: 38,169 window draws over **828 distinct
  parents — 46.097 draws per parent**, the closest approach the barren
  threshold of 64 has ever seen. `classes_skipped` and `counter_resets` are
  zero for the sixth panel running; if the chain keeps concentrating this
  hard, the exhaustion machinery may fire in a real arm for the first time,
  and its first firing should be watched for and reported.
- Film of the furthest trajectory cut from the shortest input at `(1, 1, 25)`
  and delivered; frames rendered on the ARM machine, encoded at sixty frames a
  second, `target/smb-completion/c62-film/` on both machines.

## C64 — registered concentrated conquest, parallel seed of the third link

- Observational, in the C61/C62 pattern: the third link runs as two live
  conquest campaigns from the same verified origin on separate machines,
  differing only in campaign seed and worker count. C63 is the local arm; this
  is the ARM machine's arm.
- One live concentrated-policy conquest campaign, 50,000 executions, twelve
  workers on the ARM machine, campaign seed `0x5eed_c006`, sourced from C61's
  verified archive copied to the ARM machine and hash-verified there:
  SHA-256 `4d1c28f1ced20c0f62abb94c0aec69176b275c8d9b0b62c792db5240d4d2a176`,
  byte-identical to the local original. One live run, no retries, serial
  replay of the recorded stream as the determinism gate.
- The resume-rule assumption check is C63's, unchanged: the shared origin's
  frontier at `(1, 1, 27)` measures recorded 27, viable 27, play 27 — settled
  play, no derived origin needed. The check binds to the origin, not the
  machine, so it is not repeated.
- Raw destination: `target/smb-completion/c64-conquest/` on the ARM machine,
  log `c64.log`.

### C63 live result — the local arm stalls at exactly its origin's frontier

- The live run completed its full 50,000 executions, ten workers, campaign
  seed `0x5eed_c005`, from C61's verified archive: 14,855 retained against
  38,995 rejected with 889 deaths, 9,721,531 frames emulated, 201 duplicates
  skipped and 535 probe refusals. Live stream SHA-256
  `00902c8a455ba3382bcaecc297d13eb47b6827f373ad0e5421156adeb4e68f96`; serial
  replay in flight at the time of writing.
- **Its ladder tops at `(world 1, level 1, progress 27)` — exactly the
  origin's frontier, zero new buckets in 50,000 executions.** The frontier
  densified — 461 entries at bucket 27 against the origin's 446, 447 at 26 —
  but never moved. Selector accounting: 37,477 window draws over 1,962
  distinct parents, 19.101 draws per parent, `classes_skipped` and
  `counter_resets` zero again.
- The film's terrain and the ladder agree on where the program is: the second
  world's second level is the water level, and the stall is in swimming play.

### D65 — measurement: the input-action cap binds at the third link's frontier

- A recorded-artifact measurement in the D57 pattern, run before any ruling on
  the stall: no search, no mechanism change, no model.
- **The stall is not a wall.** The three-mask span walk over C63's deepest
  trajectory in buckets 15 through 27 finds four action boundaries, every one
  admitted by the probe with all three masks surviving the full 120-frame
  horizon — swimming states do not even die to the no-input mask. The frontier
  camera is 435, early in a long level; there is no terrain barrier and no
  retention defect. Evidence at
  `target/smb-completion/c63-diagnosis/span-15-27.json`.
- **The stall is the input-action cap.** The engine's hard ceiling
  `MAX_SMB_COMPLETION_ACTIONS` is 512, every registered campaign has run at an
  action limit of 512, and the executor silently drops any suffix action past
  the limit, so a parent at the cap yields children identical to itself. The
  census against the recorded archives: C63's resume input is 510 actions; at
  its frontier buckets 25 through 27 the median input length is 511 to 512
  with 2,160 entries at exactly 512. The chain walked into the ceiling link by
  link — the H59 pilot resumed a 422-action input and its archive already
  holds 508-action entries; C61 resumed at 498 and reached the cap; C63
  resumed at 510 with two actions of headroom and moved zero buckets. C61's
  own second-half slowdown (twelve actions of extension in 50,000 executions,
  against the pilot's seventy-six in 20,000) was the cap tightening, visible
  in hindsight.
- **Prediction, recorded before the C64 arm lands:** C64 resumes the same
  510-action input under the same 512 cap, so it must stall at bucket 27 or
  within the one or two buckets that two appended actions can buy, regardless
  of seed. If it instead advances well past 27, this diagnosis is wrong and
  the entry says so.
- Consequence, stated for the ruling this measurement feeds: the third link
  stalled on a mechanical ceiling, not on search quality. The one-variable
  change the standing chain ruling licenses at a stall is raising the action
  limit — an engine constant bump plus a registered per-campaign limit, with
  inertness on recorded artifacts unaffected because every recorded header
  carries its own limit of 512 and replay reads the header. The limit value
  and its sizing against archive growth and replay cost are put to the
  integrator rather than chosen here.

### Ruling on the action cap — the ceiling rises to 4096

- The integrator ruled 4096, with the reasoning recorded: the cap is a
  ceiling, not an allocation. Archives grow with actual trajectory depth under
  any cap value, so 2048 and 4096 cost the same until real play exceeds 2048 —
  and at that point the only difference is whether the stall-diagnose-rule-
  re-verify cycle, roughly half a day, recurs in the middle of the endgame. At
  4096 it does not recur. The recurring-stall pattern is precisely the class
  of loss the program has standing instruction to engineer away.
- The change: `MAX_SMB_COMPLETION_ACTIONS` raised from 512 to 4096. The
  per-campaign action limit stays an explicit registered value at every link,
  so the record remains honest about what each campaign ran under.
- One adjacent surface deliberately untouched: the model-host's
  generated-mutator prompt text still reads "at most 512 chords". It is part
  of the recorded-decision machinery, which must rebuild recorded artifacts
  exactly, so it is not edited. The ceiling raise only loosens that path's
  output validator, which is benign; the stale figure in the prompt is noted
  here as a known residue for whenever that machinery is next registered.
- **Inertness re-verification passes.** The standing reference campaign —
  seed `0x5eed_ef00`, 256 executions from the C49 conquest archive, probing
  retention, extended ladder, concentrated selector, action limit 512 — re-run
  on the raised-ceiling tree produces an archive byte-identical to the
  standing reference,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`. The
  reference stands unchanged.
- The four quality gates pass on the changed tree.
- Recorded on the integrator's instruction as standing method: **D57 and D65
  are the first two entries of the diagnostic pattern library** — before any
  mechanism ruling on a stall or an anomaly, a recorded-artifact measurement
  is run and recorded, so the ruling reads evidence rather than hypothesis.
  The pattern's shape: no search, no mechanism change, no model; walk or
  census the recorded artifacts until the failure is mechanical fact.

### C63 replay verdict — verified

- The serial replay completed all 50,000 executions byte-identically:
  `replay_verified` true, replay stream equal to the live stream, replayed
  archive SHA-256
  `624ba5152b4767287cb6ca5faf1243801a48d18a4938cfe9f7cf791db4d8db6f`. C63 is
  evidence: a verified 50,000-execution demonstration that the 512 cap
  freezes this frontier.
- Its film deliverable is cut and is **byte-identical to C61's film** — the
  shortest input at `(1, 1, 27)` is the origin entry itself, unextended. The
  film of a stall is the film of its origin's frontier; recorded as fact
  rather than skipped.

### C64 live result — the prediction confirms out of sample

- The ARM arm's live run completed: 50,000 executions, twelve workers,
  campaign seed `0x5eed_c006`, 15,349 retained against 38,566 rejected, live
  stream SHA-256
  `8635c019a0993c21401fdf8a6d546f9f2bc655031fc8b17cc940a9fec726d4bb`, serial
  replay in flight.
- **Its ladder tops at `(world 1, level 1, progress 27)` — exactly as D65
  predicted before the run landed.** A different campaign seed from the same
  origin under the same 512 limit: zero new buckets, 2,747 entries at exactly
  512 actions, maximum input length 512. Two independent seeds now establish
  the cap as the stall's cause; the diagnosis has survived its out-of-sample
  test. Concentration accounting: 37,718 window draws over 2,815 distinct
  parents, 13.398 draws per parent, `classes_skipped` and `counter_resets`
  zero.

## C65 — registered concentrated conquest, fourth link at the raised limit

- Observational, in the C49/C61 pattern: no falsifiable claim, no control, no
  acceptance rule. Deliverables are the recorded archive, the extended ladder,
  and film. This is the first registered campaign at an action limit above
  512, and its purpose is the one-variable consequence of the D65 ruling: the
  same promoted stack, the same frontier, with the cap no longer binding.
- One live concentrated-policy conquest campaign, 50,000 executions, ten
  workers on the local machine, campaign seed `0x5eed_c007`, **action limit
  4096**, sourced from C63's verified archive
  `target/smb-completion/c63-conquest/archive-live.json`, SHA-256
  `624ba5152b4767287cb6ca5faf1243801a48d18a4938cfe9f7cf791db4d8db6f`. One
  live run, no retries, serial replay of the recorded stream as the
  determinism gate.
- Choice of origin: C63 and C61 tie at `(1, 1, 27)` and both are verified;
  C63 is the later link with the denser frontier — 461 entries at bucket 27
  against C61's 446 — and its recency window at close of run holds the
  youngest frontier population, so the chain resumes from C63.
- Resume-rule assumption check, run against the recorded artifact before
  launch: the play measurement on C63's archive returns recorded 27, viable
  27, play 27 — settled genuine play. The resume input is the 510-action
  frontier entry, now with 3,586 actions of headroom under the registered
  limit.
- Expectation stated for honesty, not as an acceptance rule: if D65's
  diagnosis is right, this arm should move past bucket 27 the way C61 moved
  through the level before it; if it stalls at 27 with headroom available,
  the cap was not the cause and the diagnosis is wrong.
- Raw destination: `target/smb-completion/c65-conquest/`.

## Standing method — replay verification leaves the chain-link critical path

- Ruled by the integrator, effective immediately, and recorded with the
  rationale as given.
- For conquest chain links, the next link launches from the live archive the
  moment its ladder lands. The link's serial replay still runs, but as a
  background **audit** in spare capacity — niced, or on whichever machine is
  idle — and its verdict is recorded when it lands.
- A failed audit **quarantines** that link and everything sourced from it,
  and the chain re-derives from the last audited link. Every recorded stream
  is kept, so nothing is ever lost — only re-verified.
- Replay remains a **gate**, not an audit, for registered experiments —
  panels and pilots — and for the eventual completion claim, which receives
  full verification and film. Chain links are territory, not claims.
- Applied on ruling: C64's replay, in flight on the ARM machine, and C65's
  replay, queued behind its live run, continue as audits. Link five registers
  and launches from C65's live archive the moment C65's ladder lands, on
  whichever machine is free, without waiting for any replay.

### C65 live result — the cap was the stall; the frontier moves thirty-two buckets

- The live run completed its full 50,000 executions in roughly twenty-five
  minutes of wall time, ten workers, campaign seed `0x5eed_c007`, action
  limit 4096, from C63's archive: 26,797 retained against 28,390 rejected
  with 2,854 deaths, 10,769,491 frames emulated, 95 duplicates skipped and
  4,398 probe refusals. Live stream SHA-256
  `03a0b6901b78bc54c9212a9af400a10f053467034f6eba5f859b4662b8c0c0d0`; its
  serial replay runs as a background audit under the new standing method.
- **The registered expectation resolves in the diagnosis's favour: the ladder
  moves from `(1, 1, 27)` to `(1, 1, 59)`** — thirty-two buckets in one run,
  after 100,000 executions at the old cap had moved zero. The progress
  watermark saw 65; the deepest retained bucket is 59. D65's cap diagnosis is
  now confirmed three ways: the census, the C64 out-of-sample stall, and the
  headroom release.
- The water level fights back in the numbers: deaths more than triple C63's
  (2,854 against 889) and probe refusals rise eightfold (4,398 against 535) —
  swimming states near enemies fail admission far more often than running
  states ever did. Retention is nonetheless the largest of any link, 26,797
  entries, and the play measurement returns recorded 59, viable 59, play 59 —
  the frontier is settled genuine play.
- The archive now weighs 1.16 GB — trajectory depth, as the cap ruling
  anticipated: growth follows actual play, not the ceiling.

## C66 — registered concentrated conquest, fifth link

- Observational, in the C49/C61 pattern; the first link registered under the
  audit-not-gate method — its origin is a live archive whose audit is still
  in flight, recorded here plainly.
- One live concentrated-policy conquest campaign, 50,000 executions, twelve
  workers on the ARM machine, campaign seed `0x5eed_c008`, action limit 4096,
  sourced from C65's live archive
  `target/smb-completion/c65-conquest/archive-live.json`, SHA-256
  `64ead1a337921aa357129f11277288d7863303274c4ea569633f713b8d3076b6`, copied
  to the ARM machine and hash-verified there before launch. One live run, no
  retries; its serial replay runs as a background audit.
- Quarantine lineage, recorded for the audit bookkeeping: C66 depends on
  C65's audit and its own. C65 depends on C63's audit, which already passed
  as a gate. A failed C65 audit quarantines C65 and C66 and re-derives the
  chain from C63.
- Resume-rule assumption check, run before launch: the play measurement on
  C65's live archive returns recorded 59, viable 59, play 59 — settled play,
  no derived origin needed. The resume input is the shortest entry at
  `(1, 1, 59)`.
- Raw destination: `target/smb-completion/c66-conquest/` on the ARM machine,
  log `c66.log`.

### C64 audit verdict — passed

- The serial replay of C64's recorded stream completed all 50,000 executions
  byte-identically: `replay_verified` true, replay stream equal to the live
  stream `8635c019…`, replayed archive SHA-256
  `e2c9fe7bef441d83d6d7cbedc5cb68bd533ad349d068ea3d7cb0ddf6cb75c172`. The
  first audit recorded under the audit-not-gate method; C64 is fully audited
  evidence and the third link's book is closed — both its seeds stalled at
  the cap exactly as D65 predicted, and both replays verified.

### C66 live result — the fifth link stalls at its origin's frontier

- The live run completed its full 50,000 executions, twelve workers, campaign
  seed `0x5eed_c008`, action limit 4096, from C65's live archive: 25,403
  retained against 28,906 rejected with 3,137 deaths, 10,714,067 frames
  emulated, 112 duplicates skipped and 5,048 probe refusals. Live stream
  SHA-256
  `47826cfa4110b0e35df7635208162af1da8c78a269f6c188d49e0176b0dd9504`; its
  serial replay runs as a background audit.
- **Its retained ladder tops at `(1, 1, 59)` — exactly the origin frontier,
  zero new buckets — while its progress watermark saw 65.** C65's own run
  showed the same signature: watermark 65, deepest retained 59. Two
  independent seeds now reach buckets 60 through 65 and never retain a single
  state there. The play measurement returns recorded 59, viable 59, play 59.
- Not the cap this time: the action limit is 4096 and the frontier inputs sit
  at 553 actions with thousands of actions of headroom.
- Concentration accounting: 37,572 window draws over 21,066 distinct parents,
  1.783 draws per parent — the heavy retention churns the window fast.
  `classes_skipped` and `counter_resets` zero for the seventh panel running.
- Film deliverable: C66's shortest input at `(1, 1, 59)` is byte-identical to
  C65's — the same 553-action entry, input SHA-256 `9104b954…` in both
  archives — so its film is C65's film and is not re-cut.

### D67 — measurement: the admission probe walls off the Blooper corridor

- Third entry in the diagnostic pattern library: recorded-artifact
  measurement before any mechanism ruling. No search, no mechanism change, no
  model.
- The admission rule, stated exactly from the engine: a candidate state is
  retained only if **at least one of three fixed input continuations — no
  input (0x00), held right (0x01), swim-stroke plus right (0x81) — survives
  120 frames** from the candidate's snapshot.
- **The recorded path is clean.** The three-mask span walk over C65's deepest
  trajectory in buckets 40 through 59 finds twenty-one boundaries, every one
  admitted with all three masks surviving the full horizon. The frontier
  boundary sits at camera 945. Evidence at
  `target/smb-completion/c65-diagnosis/span-40-59.json`.
- **The frontier scene explains what changes past it.** The film's final
  frame at bucket 59 shows a narrow coral corridor guarded by two of the
  water level's homing enemies, with a third above. A fixed 120-frame input
  continuation cannot evade a homing enemy; live play survives there by
  reacting.
- **The refusal census is decisive.** Joining every probe-refused decision in
  C65's stream to its parent's archive key: 4,220 of 4,398 refusals have
  parents in the second world's second level, and 3,609 — eighty-two percent
  of all refusals in the run — have parents at buckets 55 through 59, massed
  exactly against the corridor. C66 repeats the shape with 5,048 refusals.
  Children reaching 60 through 65 are being produced and are being refused
  admission; the watermark sees them and the archive never keeps them.
- The lineage of this rule, recorded for fairness: H45's probing retention is
  promoted machinery that demonstrably helped, and H58 recorded the
  three-mask design "earning its place at the tip" when a single-mask probe
  would have refused the states that reached 145. The masks were built for
  running-and-jumping terrain. The water corridor is the first place the
  fixed repertoire itself is the wall.
- The fork, stated for the integrator's ruling rather than chosen here. All
  variants would register as a **new explicit retention policy value**
  alongside the existing one, which stays the default so every recorded
  artifact keeps replaying byte-exact; the builder pattern is the same as the
  cap's. Candidates, one variable each:
  - **Shorter horizon** — drop the probe horizon (120 frames) for the new
    policy value, on the reasoning that two fixed-input seconds is an
    unreasonable survival demand near homing enemies.
  - **Evasive masks** — add fixed continuations suited to water evasion
    (swim-stroke alone, swim-stroke plus left) to the any-mask-survives set.
  - **Both**, if the integrator prefers one decisive move over two rulings,
    at the cost of confounding which half mattered.
- Execution stays busy during the pause: both machines are running their
  audits, and no chain link launches until the ruling lands, because any next
  link from a bucket-59 origin runs into the same wall.

## D68 — preregistered refused-candidate probe grid

- Ruled by the integrator on the D67 fork: neither probe variant registers
  yet, because a cheaper decisive measurement exists. Fourth entry in the
  diagnostic pattern library — measurement, not mechanism: no new policy
  value, no model, no campaign.
- Mechanism of the measurement, fixed before execution. The refused
  candidates are re-derived from the recorded streams exactly as the workers
  produced them: the parent's recorded input replays from reset to its
  snapshot, the suffix re-derives from the recorded mutation seed, and the
  stream's decision order maps one-to-one onto alive candidates, so each
  probe-refused decision names one concrete re-derived state. Each refused
  candidate at the frontier pair in buckets 60 through 65 is probed under a
  fixed grid: masks {no input, held right, swim-stroke plus right,
  swim-stroke alone, swim-stroke plus left, alternating stroke — pressed 4
  frames, released 12, period 16, the one cadence the fixed-input probe can
  express} × horizons {120, 90, 60, 45 frames}, recording the survival frame
  of every probe.
- Sample rule, fixed before execution: the first 500 refusal jobs in stream
  order whose parent sits at the frontier pair in buckets 55 through 59, per
  stream, both recorded streams — C65's and C66's. Refused candidates landing
  outside 60 through 65 are tallied by key rather than probed, so if the
  60-to-65 states turn out to die rather than be refused, the measurement
  says so instead of assuming.
- Validation, fixed before execution: the promoted probe's own three masks at
  the full horizon must refuse every re-derived candidate — any survival
  there is a re-derivation divergence and voids the measurement. The report
  carries the count.
- Decision rule, as ruled: a mask surviving at 120 for most refused states
  means the repertoire was the problem and the evasive-mask variant registers
  with that mask set; nothing surviving 120 but a mask surviving at a
  measured shorter horizon means the demand length was the problem and the
  shorter-horizon variant registers at the measured value; no fixed
  continuation surviving at any horizon for most of the corridor kills both
  variants at zero registration cost and escalates to a design ruling.
- The diagnostic lives in `diagnose-refused-grid`; the four quality gates
  pass on the tree carrying it.
- Raw destinations: `target/smb-completion/d68-refused-grid/` on each
  machine.

### D68 result — both probe variants dead on arrival, and the wall is not the enemies

- Both grids are valid: 500 refusal jobs re-derived per stream, zero
  derivation mismatches on either — every re-derived candidate is refused by
  the promoted probe's own masks at the full horizon, exactly as the stream
  recorded. C65's grid probed 312 candidates in buckets 60 through 65, C66's
  315; refused candidates landing at 55 through 59 were tallied out of range
  as registered (230 and 224).
- **The registered third branch fires: no fixed continuation survives the
  full horizon for a single refused candidate in either stream — zero of 312
  and zero of 315, across all six masks.** At the shortest horizon of 45
  frames the best mask saves 24 of 312 (7.7 percent) and 46 of 315 (14.6
  percent). Both preregistered variants — shorter horizon and evasive masks —
  are dead on arrival for the corridor's majority, at zero registration cost.
- **The mechanism is not the visible enemies.** Every probe death in both
  grids — 1,872 and 1,890 probes — is the below-play-area clause; not one is
  the kill state. The vertical supplement pins it: refused candidates sit at
  vertical page 1 with vertical low 189 through 255, median 243 — a median of
  thirteen pixels above the page-2 death threshold — and the no-input death
  frame tracks the remaining pixels almost linearly. These are states in free
  descent through an opening in the corridor's floor, past the depth where
  swimming physics applies; no fixed input arrests the fall, and fall physics
  means no reactive input would either. **The probe is refusing the majority
  correctly: they are genuinely dead states that have not finished dying.**
- The recoverable minority is real and is measured: the past-45-frame tail
  clusters at the shallow end, buckets 60 through 63, and is exactly the
  population a 45-frame horizon would admit — along with some slow sinkers
  that die near frame 67, which fixed probes cannot separate from it.
- One candidate mechanism is closed by inspection: the frozen archive key
  already carries a sixteen-pixel vertical band, so altitude diversity is
  already retained per bucket. The wall is admission alone, not key
  granularity.
- Escalated to the integrator for a design ruling, as the registration
  requires. What the data argues: a blanket probe-exempt corridor would
  retain a majority of truly-dead falling states as parents; a 45-frame
  horizon rescues the measured shallow tail but admits doomed sinkers with
  it; and the deeper shape of the problem is that the corridor's viable line
  demands staying out of the floor opening, which no admission rule alone
  steers toward — it is a selection-pressure question as much as a retention
  one. No campaign launches until the ruling lands.
- Raw evidence: `target/smb-completion/d68-refused-grid/` on both machines,
  with the vertical supplement in `c65-grid-vertical.json`.

### Ruling on the corridor — the 45-frame horizon registers; exemption is dead

- The integrator ruled on D68, with the rationale recorded. Blanket
  probe-exemption is dead: the grids proved the refusal majority genuinely
  unrecoverable, and an archive seeded with falling corpses is the failure
  H45 exists to prevent. The 45-frame probe horizon registers as a **new
  policy value beside the 120-frame default** — per-campaign, recorded in
  every header and report, every recorded artifact replaying under its own
  recorded horizon.
- Rationale as ruled: the grids measured a real rescuable population — 7.7
  and 14.6 percent, clustered at the corridor entrance — that only a shorter
  horizon admits. The known cost, slow sinkers dying near frame 67, is
  exactly the load the concentration-plus-exhaustion system was designed to
  carry: doomed admissions become barren parents and starvation is their
  bound. **This is the regime where the exhaustion leg finally bears
  weight**; the counters are reported prominently either way, and if doomed
  parents accumulate while starvation still never fires, the sixty-four-draw
  threshold gets its own ruling with this data.
- The build: `probe_at_admission_45` beside `probe_at_admission`, threaded
  through campaign configuration, worker execution, bootstrap, the stream
  header and replay; `--retention` selects it per run and replay reads the
  header. The four quality gates pass, and the standing inertness reference
  re-verifies byte-identical on the changed tree,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24` — the
  default path is untouched.

## C67 — registered concentrated conquest, sixth link, pilot-and-link at horizon 45

- Per the ruling, pilot and chain link in one: one live concentrated-policy
  conquest campaign at the new retention value. If retention extends past
  bucket 65 the chain continues at horizon 45 and this link counts as
  territory; if it stalls, one link bought the decisive test and
  steering-over-the-gap escalates back to the integrator as
  selection-pressure design, with the track-2 waypoint mechanism the
  motivated candidate. The no-launch hold is lifted.
- **Preregistered question: does retention extend past bucket 65** — the
  watermark ceiling both 120-horizon runs saw and never kept.
- One live run, no retries: 50,000 executions, ten workers on the local
  machine, campaign seed `0x5eed_c009`, action limit 4096, retention
  `probe_at_admission_45`, selector concentrated recency. Serial replay runs
  as a background audit per the standing method.
- Origin, under the standard source rule: C65's archive and C66's tie at
  `(1, 1, 59)` and share the identical shortest frontier input — the same
  553-action entry, input SHA-256 `9104b954…` in both — so the resume state
  is identical either way. C65 is chosen: it is fully audited where C66's
  audit is still in flight, and it is already on the launching machine.
  Archive SHA-256
  `64ead1a337921aa357129f11277288d7863303274c4ea569633f713b8d3076b6`.
- Assumption checks, against recorded artifacts: the measure can move —
  D68 measured 24 and 46 refused candidates per stream surviving past 45
  frames at buckets 60 through 63, exactly the population this horizon
  admits, so admission past 59 is arithmetically possible where under the
  120 horizon it was measured impossible. The frontier is real and settled —
  recorded 59, viable 59, play 59, and the span walk admits every recorded
  boundary. The exposure that cannot be checked for free is stated: whether
  the admitted tail contains states from which the search can climb out of
  the floor opening's pull, which is the pilot question itself.
- **Exhaustion expectation, stated as the ruling requires**: horizon-45
  admission will seed doomed parents — slow sinkers whose children all die —
  and these should become barren and be starved by the sixty-four-draw
  threshold. The selector counters, the concentration accounting, and
  `classes_skipped` and `counter_resets` are reported prominently in this
  link's result **either way**. Zero firings against a corpus of admitted
  corpses is itself a finding and escalates the threshold.
- Raw destination: `target/smb-completion/c67-conquest/`.

### C67 live result — horizon 45 completes the water level and opens the next

- **The preregistered question resolves yes, decisively: retention extended
  past bucket 65** — through it, through the corridor, through the rest of
  the water level to its completion region at bucket 195, and into the
  second world's third level, first retained at execution 45,467 and driven
  to bucket 165 in the remaining 4,533 executions with watermark 173. The
  chain continues at horizon 45 and this link counts as territory.
- The live run completed its full 50,000 executions, ten workers, campaign
  seed `0x5eed_c009`, action limit 4096, retention `probe_at_admission_45`,
  from C65's audited archive: 30,988 retained against 15,659 rejected,
  5,294,979 frames emulated — half the frame cost of the 120-horizon links,
  the shorter probes paying for themselves — 115 duplicates skipped and
  2,605 probe refusals, roughly half the refusal count of either 120-horizon
  run. Live stream SHA-256
  `7fedc068facc2eccafb5d82ba53d460a0d57b208586522db0dfecd313ed86aa7`; serial
  replay running as a background audit.
- **The corpse load arrived exactly as the ruling predicted: 11,274 deaths,
  nearly four times any previous link** — horizon-45 admissions include slow
  sinkers whose children die. The play measurement still returns recorded
  165, viable 165, play 165 at the new frontier: settled genuine play.
- **The exhaustion accounting, reported prominently as the registration
  requires: `classes_skipped` zero, `counter_resets` zero — again, in
  exactly the corpse-seeding regime the threshold was designed for.** The
  structural reason is now measurable: 37,659 tie-class draws spread over
  23,218 distinct parents passing through the recency window is 1.62 draws
  per parent, so no parent can approach sixty-four barren draws before the
  window displaces it. Recency displacement and the barren threshold are
  structurally incompatible at high retention rates: the window churns
  parents out long before starvation can bind. Per the ruling, the
  sixty-four-draw threshold now has its own escalation data; recorded here
  for the integrator's threshold ruling, without pausing the chain.
- Level-ladder note for honest reading: the water level's deepest bucket 195
  sits in its completion region, and the third level's opening at execution
  45,467 means over ninety percent of the budget was spent inside the water
  level; the third level's 165 buckets in 4,533 executions is the fastest
  per-execution advance any link has recorded, consistent with terrain that
  rewards held-right play.

## C68 — registered concentrated conquest, seventh link at horizon 45

- Observational, in the C49/C61 pattern; the chain continues at horizon 45
  per the C67 pilot resolution. One live concentrated-policy conquest
  campaign, 50,000 executions, twelve workers on the ARM machine, campaign
  seed `0x5eed_c00a`, action limit 4096, retention `probe_at_admission_45`,
  sourced from C67's live archive, SHA-256
  `6efb1780d5dfa7f6ac41543a7ac2174482ffe24d987251abbf35ea9f2d0120e6`,
  copied to the ARM machine and hash-verified there before launch. One live
  run, no retries; serial replay as a background audit.
- Quarantine lineage: C68 depends on C67's audit and its own; C67 depends on
  C65's audit, which passed as recorded. A failed C67 audit quarantines C67
  and C68 and re-derives from C65.
- Resume-rule assumption check, run before launch: the play measurement on
  C67's archive returns recorded 165, viable 165, play 165 — settled play at
  the maximum pair, no derived origin needed.
- Raw destination: `target/smb-completion/c68-conquest/` on the ARM machine,
  log `c68.log`.
- Launch note, recorded plainly: the first launch attempt exited at argument
  parsing — the ARM machine's binary predated the retention flag because the
  source sync had covered the origin archive but not the new policy code.
  **No stream was written and zero executions ran**, so the registered arm
  was not consumed and this is not a retry; the tree was synced, rebuilt and
  the same registered arm launched. The pre-launch checklist for cross-
  machine links now includes verifying the remote binary accepts the
  registered flags, alongside the origin hash.

### C68 live result — seven levels in one link, the exhaustion leg's first firing, and a new ceiling

- The live run completed its full 50,000 executions, twelve workers, campaign
  seed `0x5eed_c00a`, action limit 4096, retention `probe_at_admission_45`,
  from C67's archive: 32,768 retained against 14,483 rejected with 10,895
  deaths, 5,438,253 frames emulated, 2,923 probe refusals. Live stream
  SHA-256
  `5e607b1f8506755c30d56efa205b0387674fb3eaa71859a04fd8970a75d92c2e`; serial
  replay running as a background audit.
- **Seven levels of new territory in one link.** The ladder: the second
  world's third level driven to 221 and its castle opened at execution 2,977
  and completed; the third world's four levels crossed in sequence — 206,
  208, 147, 144, first reached at executions 12,631, 18,674, 24,207 and
  29,434 — and **the fourth world opened at execution 40,462**, reaching
  bucket 153 with watermark 156. Half the game's worlds have now been
  entered.
- **The exhaustion machinery fired for the first time in the program's
  history: `classes_skipped` 5,418**, against zero across every prior panel
  and link; `counter_resets` stays zero. The firing is real but it is a
  symptom, not frontier pressure — see the ceiling below: once the archive
  froze, every parent went barren by construction, the deepest bands
  exhausted in turn, and selection correctly fell through band after band.
  The leg works as designed; what it was starving on was a full archive, not
  a hard frontier.
- **The new ceiling: `MAX_ARCHIVE_ENTRIES` is 32,768 and C68 hit it.** The
  last retention happened at execution 42,482; the final 7,518 executions —
  fifteen percent of the budget — could retain nothing, because a full
  archive rejects every candidate including new-key states. At horizon-45
  retention rates every future link will fill the archive at roughly this
  point. Same class as the action-limit ceiling: a compiled bound the chain
  has now outgrown. Flagged for the integrator's ruling; the chain continues
  uncontested meanwhile, carrying roughly fifteen percent tail waste per
  link until ruled.
- **First recorded divergence between the mechanical resume rule and the
  play frontier under horizon 45**: the play measurement returns recorded
  153, viable 150, play 150. The single bucket-153 entry was admitted by the
  45-frame probe but does not survive the measurement's 120-frame no-input
  horizon — precisely the admitted-sinker class the D68 ruling accepted as
  cost. The assumption check caught it before launch, as it exists to do.

## C69 — registered concentrated conquest, eighth link at horizon 45

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, ten workers on the local machine,
  campaign seed `0x5eed_c00b`, action limit 4096, retention
  `probe_at_admission_45`. One live run, no retries; serial replay as a
  background audit.
- Origin, per the H59 derived-origin precedent because the resume rule and
  play frontier diverge on C68's archive: a **single-entry origin holding
  exactly the play-frontier entry** — C68's entry 32,767 at
  `(world 3, level 0, progress 150)`, 1,399 actions, the archive's last
  admitted entry, and the sole entry at its bucket so the choice is forced.
  Derived origin
  `target/smb-completion/c68-conquest/origin-play150.json`, SHA-256
  `82c6b5dea04cb74b3dd7e018c5beaa83c3974d13a15c13b25d2f12656d4fef53`,
  hash-verified identical on both machines. The campaign resume rule
  resolves it trivially.
- Quarantine lineage: C69 depends on C68's audit, C67's audit, and its own;
  a failure anywhere in that chain quarantines everything downstream of the
  failing link and re-derives from the last audited archive (currently C65).
- Exhaustion and concentration accounting reported prominently in the
  result, per standing practice; the archive-cap fill execution is now also
  reported per link while the cap ruling is pending.
- Raw destination: `target/smb-completion/c69-conquest/`.

### C69 live result — world four's first level complete, its second driven deep

- The live run completed its full 50,000 executions, ten workers, campaign
  seed `0x5eed_c00b`, action limit 4096, retention `probe_at_admission_45`,
  archive bound 32,768 as registered, from the derived play-150 origin:
  25,822 retained against 34,021 rejected with 2,696 deaths, 6,290,982
  frames emulated. Live stream SHA-256 recorded in the campaign report;
  serial replay running as a background audit.
- **The fourth world's first level is complete at bucket 224, and its second
  level opened at execution 1,485 and reached bucket 208** — recorded,
  viable and play all 208, no sinker divergence this time, so the next link
  resumes directly. The cap never bound: the last retention landed at
  execution 50,000 exactly, and `classes_skipped` and `counter_resets` are
  zero again — consistent with the C68 diagnosis that the firing was the
  full archive, not the frontier.
- Deaths fell to 2,696, a quarter of the water-level links — running terrain
  suits the suffix repertoire. Concentration: 37,578 window draws over
  15,429 distinct parents, 2.44 draws per parent.

## C70 — registered concentrated conquest, ninth link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c00c`, action limit 4096, retention
  `probe_at_admission_45`, **archive entry bound 131,072 — the first link
  registered at the raised ceiling, recorded in its header**. One live run,
  no retries; serial replay as a background audit.
- Origin: C69's live archive, SHA-256
  `023e2aa14f9e334d2193e9e013fc0ab4856ad331cb009e8f1a9fe20eb651563c`,
  copied to the ARM machine and hash-verified there, with the remote binary
  verified against the registered flags per the C68 launch note. The play
  measurement returns recorded 208, viable 208, play 208 at the maximum
  pair, so the campaign resume rule and the play frontier coincide and no
  derived origin is needed.
- Quarantine lineage: C70 depends on the audits of C67, C68, C69 and its
  own; the last audited link is C66.
- Raw destination: `target/smb-completion/c70-conquest/` on the ARM machine,
  log `c70.log`.

### C70 live result — the ninth link stalls at the warp-zone room

- The live run completed its full 50,000 executions, twelve workers, campaign
  seed `0x5eed_c00c`, action limit 4096, retention `probe_at_admission_45`,
  archive bound 131,072 as registered, from C69's archive: 22,279 retained
  against 40,299 rejected with 943 deaths. **Its ladder tops at
  `(3, 1, 208)` — exactly the origin frontier, zero new buckets**, watermark
  equal, no cap bind, `classes_skipped` and `counter_resets` zero,
  concentration 37,511 draws over 13,856 parents at 2.71 draws per parent.
  Serial replay running as a background audit. A genuine stall.

### D71 — measurement: the frontier is a warp-zone room and the controller cannot press Down

- Diagnostic pattern library, recorded-artifact measurement before any
  mechanism ruling.
- **The frontier scene, from the recorded film**: C69's furthest trajectory
  rides the elevator platforms, runs along the ceiling, and ends standing in
  the level's warp-zone room beside a single downward pipe — the film's
  final frame shows the warp greeting and the pipe. Camera progress ends in
  that room; bucket 208 is its camera position, and C70's archive populates
  every vertical band of it. The only forward move the room offers is
  entering the pipe from above, which requires the Down button.
- **The controller vocabulary has no Down.** From the emulator's own joypad
  bit definitions — A `0x01`, B `0x02`, Select `0x04`, Start `0x08`, Up
  `0x10`, Down `0x20`, Left `0x40`, Right `0x80` — the frozen nine-mask
  campaign vocabulary decodes as none, A, B, Left, Right, Right+A, Right+B,
  Right+A+B, and Up. `0x20` appears nowhere. The search has been unable to
  press Down for the program's entire history; this room is the first place
  the game demands it.
- **Corrigendum, recorded because the record must decode its own masks
  correctly**: D67 and D68 named the probe masks under the wrong bit order —
  "held right (0x01)" is in fact A, "swim-stroke plus right (0x81)" is in
  fact Right+A, and the D68 grid's schedule names are shifted the same way.
  Every hash, count and byte of those measurements stands unchanged; only
  the human-readable mask names were wrong. Correctly decoded, the D68
  result strengthens: the masks that survived longest in the water corridor
  are the A-carrying ones — swim strokes — which is what water physics
  predicts.
- The alternative branch, measured: buckets 205 through 207 hold hundreds of
  floor-band states inside the winning eight-bucket frontier band, so the
  level's normal exit route was populated and drawn from for 100,000
  executions across C69's tail and all of C70 without producing a level
  transition. Whatever the normal exit demands — a down-entry, or a maneuver
  the repertoire lacks — the present vocabulary did not find it at this
  budget from this population.
- **The strategic fact that frames the fork: the game cannot be completed
  without Down.** The final castle's mandatory route passes through pipes
  entered from above. The button is not an optional shortcut key; it is
  required equipment for the program's objective, and this room is merely
  where its absence first binds.
- The fork, stated for the integrator's ruling rather than chosen here.
  Adding Down to the mutation vocabulary is index-visible to suffix
  derivation — an eleventh mask changes every derived suffix — so it must
  register as a **new vocabulary policy value** recorded per run in the
  header, legacy streams replaying under the frozen nine-mask list, same
  doctrine as the retention and ceiling changes. The options:
  - **Vocabulary variant with Down** — unlocks the warp pipe at the current
    frontier immediately (the pipe in the room leads three worlds ahead),
    unlocks every future down-entry including the final castle's, and is
    required for completion regardless. Route consequence: the chain would
    advance by warp, skipping the remainder of the fourth world.
  - **Derived floor origin under the current vocabulary** — no mechanism
    change; resume from the deepest floor-band state and hope the normal
    exit yields to the existing repertoire at a fresh budget. The census
    above prices this: the floor route was already in the winning band for
    100,000 executions and did not exit.
  - **Both**, sequenced by the integrator's route preference — warp for
    depth now, or full clear of world four first.
- Execution holds for the ruling; both machines are running their audit
  backlog meanwhile (C67, C68, C69, C70).

### Ruling on the vocabulary fork — warp-first; Down registers as a policy value

- The integrator ruled route (a), with the rationale recorded: the Down
  vocabulary is required equipment for completion regardless of route — the
  final castle binds on it — so adding it is not the decision, only the
  sequencing, and the sequencing answers itself: the mission is completing
  the game, the warp is legitimate play advancing that mission by several
  worlds in one link, and the alternative was priced by the census at
  100,000 executions of already-failed evidence. Full-clearing the fourth
  world and the skipped worlds becomes optional secondary territory after
  the completion claim — on the map, not on the critical path.
- The build, on the standing doctrine, implemented in-session per the
  horizon-45 precedent: `down_ten_mask` — the frozen nine masks plus Down
  (`0x20`), appended so the shared prefix keeps its order — beside
  `frozen_nine_mask`, selected per run by `--vocabulary`, recorded in the
  stream header and report, with legacy streams carrying no field and
  replaying under the frozen nine masks. The table length is index-visible
  to suffix derivation, which is exactly why the vocabulary is
  header-recorded like the retention policy and both ceilings.
- The four quality gates pass; the standing inertness reference re-verifies
  byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.

## C72 — registered concentrated conquest, tenth link, pilot-and-link with Down

- Per the ruling, pilot and chain link in one: the first campaign under the
  Down-inclusive vocabulary. **Preregistered question: does retention pass
  through a down-entered pipe** — concretely, does the ladder record a pair
  beyond the fourth world reached from the warp-zone room.
- One live run, no retries: 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c00d`, action limit 4096, retention
  `probe_at_admission_45`, archive bound 131,072, vocabulary
  `down_ten_mask`. Serial replay as a background audit.
- Origin: C70's live archive — the later and denser of the two tied
  warp-room frontiers, resident on the launching machine — SHA-256
  `b893671ae9553b6aeb433f397961192f283978405df45c39e937ea5542da5cfc`. The
  play measurement returns recorded 208, viable 208, play 208, so the
  resume rule lands on settled play; the resume entry is a warp-room state,
  which is precisely where the new button matters.
- Quarantine lineage: C72 depends on the audits of C67 through C70 and its
  own; the last audited link is C66, with four audits in flight.
- Raw destination: `target/smb-completion/c72-conquest/` on the ARM machine,
  log `c72.log`.

### C72 live result — Down alone does not enter the pipe

- The live run completed its full 50,000 executions, twelve workers, campaign
  seed `0x5eed_c00d`, vocabulary `down_ten_mask` confirmed in the recorded
  header, from C70's warp-room archive: 22,176 retained against 40,576
  rejected with 938 deaths. **Its ladder tops at `(3, 1, 208)` — zero new
  pairs — and the watermark never moved.** The preregistered question
  resolves no: retention did not pass through a down-entered pipe.
- The unmoved watermark is itself evidence: the watermark merges every
  action's observations, so a warp transition that ever began would have
  surfaced the loaded area's world byte. **No pipe entry ever started** —
  the button was in the table and the pipe was never entered, which points
  at positioning, not at the button.
- Serial replay as a background audit. A stall of the pilot, handled by the
  standing method: measurement before mechanism.

## D73 — preregistered Down-press census

- Fifth entry in the diagnostic pattern library. Question, fixed before
  execution: **where does the player stand when Down is pressed, and does
  any press occur on top of the warp pipe?** The census re-derives, from
  C72's recorded stream in order, the first 600 jobs whose parent sits at
  the frontier pair in bucket 208 and whose derived suffix contains a Down
  chord; for each press it records the player's level-x and screen-x (the
  camera is frozen in the room, so screen-x locates the player against the
  pipe), vertical bytes, and the engine state and world byte across the
  hold; it also records every sampled parent's position.
- Expected under the stall: zero world-byte changes — consistency check
  against the watermark — and a screen-x distribution that says whether the
  room's population ever stands on the pipe at all.
- The diagnostic lives in `diagnose-down-census`; gates pass on the tree
  carrying it.
- Raw destination: `target/smb-completion/d73-down-census/` on the ARM
  machine.

### D73 execution note — first launch killed; root cause a cost blowup, not a loop

- The first launch computed for twelve hours with no output and was killed
  by exact process id on the integrator's instruction. Root cause,
  established from recorded artifacts rather than guessed: **the census
  replayed every distinct parent from power-on, and at this depth a parent
  input is roughly 146,000 frames** — the 1,547-action resume prefix
  averages 94.6 frames per action, an order of magnitude above the naive
  frames-per-action estimate, and bucket 208 holds 5,309 distinct parents,
  so the 600 sampled jobs were nearly all distinct parents: about 88
  million frames on one core. The process was computing, not looping;
  the defect was cost, plus a tool that printed nothing until completion,
  which made slow indistinguishable from hung.
- The fix, gated before relaunch: the shared resume prefix — which every
  frontier parent extends, the median parent being eight actions past it —
  is emulated **once** and verified against the recorded header's resume
  input hash; each parent then replays only its tail, collapsing the cost
  roughly twenty-thousand-fold. Per the ruling the census now emits a
  progress line per job and enforces a hard twenty-million-frame budget
  that fails loudly instead of silently grinding. The same
  full-replay cost shape exists in the refused-candidate grid and inherits
  this pattern if that tool runs again at depth; noted here so it is not
  rediscovered.
- **A second cost defect surfaced on the relaunch and was profiled to
  ground, on the integrator's instruction, before a second fix**: the
  progress lines showed one job per 180 seconds at roughly 250 emulated
  frames per job — emulation provably not the cost. One instrumented job
  answered it: parents that descend from **mid-bootstrap entries** — the
  campaign bootstrap retains a prefix of the resume input at every action
  boundary — fail the whole-prefix match and fell into the full power-on
  replay fallback, 123 seconds for the profiled parent, and most lineages
  at the frontier are such descendants. The fix exploits a measured fact
  the profile also surfaced: an emulator snapshot is about 1.3 KB, so the
  one-time bootstrap now snapshots **every boundary** of the base input —
  about two megabytes total — and each parent restores from its longest
  common prefix with the base, replaying only its true tail. The profiled
  worst parent fell from 123 seconds to 183 milliseconds; the full census
  runs in minutes. Cost shape recorded beside the first: per-parent work
  must be proportional to lineage divergence, never to lineage depth.
- **Two hygiene defects caught by the integrator and folded into the
  diagnostic checklist.** First, the killed second attempt was not in fact
  dead — its census child survived the wrapper kill, burned a core for
  fifty-eight minutes, and was writing toward the **same output path** as
  the completed census, which it would eventually have overwritten; the
  integrator killed it by exact process id and verified the artifact of
  record intact. The checklist gains: when replacing a running diagnostic,
  confirm the worker process id is dead — not just the wrapper — and
  version every diagnostic output path so a stale process can never clobber
  an artifact a verdict was read from. Second, the first x-transit launch
  paired a thirty-minute backstop with a run whose own measured per-job
  cost projected twenty-seven to sixty-seven minutes, and discarded the
  per-job progress lines; relaunched with a versioned output, a two-hour
  backstop sized from the measured cost, and progress visible.

### D73 result — Down is inert because the population never stands over the pipe

- The census re-derived 600 Down-carrying jobs from C72's stream — 610
  presses — with the promised validation: **zero engine-state changes and
  zero world-byte changes across every press**, consistent with the
  unmoved watermark. Every press ran from engine state 8 to engine state 8.
- **The positional verdict**: press screen-x masses at the room's right
  wall — 345 of 610 presses in the 208-to-223 band, median 209 — while the
  warp pipe's horizontal window, roughly screen-x 64 through 96 from the
  film frame, received **zero presses at any height**. The population piles
  right and never stands over the pipe; vertical spread is wide, so jumping
  is not the gap — horizontal steering is.
- Why the archive cannot help, stated mechanically: inside a scroll-frozen
  room the progress bucket is constant, and the player's horizontal
  position appears nowhere in the retention key — every x-position competes
  for the same cells, so retention exerts no pressure to hold left-side
  states, while four of the ten vocabulary masks carry Right against one
  Left. The room's equilibrium is the right wall.
- **Escalated to the integrator as selection-pressure design**, per the
  standing prediction that this class of problem would arrive: the
  motivated candidate named in advance is the waypoint mechanism from the
  track-2 strategic-calls ladder, which registers only on the integrator's
  dispatch. The in-doctrine alternative the measurement suggests: a key
  policy variant carrying a player-x term inside scroll-frozen rooms, which
  would give retention distinct cells across the room's width — H51-class
  key surgery, a registered experiment either way. No campaign launches
  from the warp room until ruled; the audit backlog continues on both
  machines.
- Raw evidence: `target/smb-completion/d73-down-census/c72-census.json` on
  the ARM machine.

## H75 — preregistered positional key term for scroll-frozen rooms

- Ruled by the integrator on the D73 escalation: the pipe-entry defect is a
  missing diversity dimension, and the cheapest in-doctrine mechanism
  supplies it. The track-2 waypoint mechanism stays in reserve, not
  dispatched.
- **Assumption checks, run against recorded artifacts before any build, as
  the ruling requires. Both hold.**
  - *The collapse is real (check one).* From the D73 census's recorded
    parent positions: 517 distinct retained frontier parents at bucket 208;
    282 of them — fifty-five percent — stand in the single 16-pixel band
    3536 through 3551 at the room's right wall, and the pipe band 3376
    through 3423 holds **zero** retained parents while both neighbouring
    bands are populated. A literal retention hole.
  - *The defect is retention, not reachability (check two).* The x-transit
    census re-derived 4,000 frontier jobs — 5,021 candidate boundaries —
    and histogrammed candidate player-x against the recorded admission
    decisions: **seven candidates stood in the pipe band, and retention
    rejected all seven**, against healthy retain ratios in every
    neighbouring band; the right-wall band shows the competition doing it,
    2,282 rejections against 838 retentions in one 16-pixel cell
    population. Transit into the band happens and is discarded; with no
    retained parent in the band, no children spawn there, and the hole
    self-sustains. Raw evidence:
    `target/smb-completion/d74-x-transit/c72-transit-v2.json`.
- Mechanism, per the ruling's requirements: a new archive key policy value
  carrying a positional term **only inside the registered scroll-frozen
  room**. The room is identified by its `(world, level, progress)` tuple —
  derived from the recorded watermark stall of the origin archive, a
  registered value, nothing NES-shaped in core — and the positional bucket
  width is a registered parameter, sixteen pixels here. States matching the
  room tuple gain a player-x bucket term in the key; every other state keys
  exactly as the frozen policy does, so the variant is provably byte-inert
  outside the room. Header-recorded like the retention, vocabulary and
  ceiling values; legacy streams carry no field and replay under their
  recorded key.
- Gates before the pilot: the four quality gates and the standing inertness
  reference byte-identical.
- **Pilot before fleet**: one live campaign on the ARM machine from C72's
  archive, 50,000 executions, twelve workers, campaign seed `0x5eed_c00e`,
  action limit 4096, retention `probe_at_admission_45`, vocabulary
  `down_ten_mask`, archive bound 131,072, key policy
  `frozen_room_x_16` at room `(3, 1, 208)`. Preregistered pilot question:
  **does the archive retain standing cells in the pipe x-band** — decided
  from the recorded archive's keys, plus the ladder for anything more.
- **Escalation criterion, fixed now as ruled**: if the pilot retains
  pipe-x-band cells and a budgeted campaign still produces no scene change
  from any Down press in that band, the track-2 waypoint mechanism gets
  dispatched — reported the moment the condition is met.
- Raw destination: `target/smb-completion/c76-conquest/` on the ARM
  machine.

### H75 pilot result — the pipe opens, and world five falls behind it

- **The preregistered pilot question resolves yes, and the run went far past
  its own question.** The room-x key populated the warp room across every
  16-pixel bucket — the pipe band that held zero retained states under the
  frozen key now holds roughly a thousand entries across its two buckets —
  and at execution 9,919 a Down press on the pipe entered it: the warp
  fired into the fifth world. The run then cleared the fifth world's four
  levels in sequence — 196, 198, 149, 144, first reached at executions
  9,919, 18,423, 23,870 and 35,672 — and **opened the sixth world at
  execution 44,177**, reaching `(5, 0, 166)` with watermark 168.
- The live run: 50,000 executions, twelve workers, campaign seed
  `0x5eed_c00e`, the full promoted stack plus `frozen_room_x_16:3,1,208`:
  39,129 retained against 12,691 rejected with 8,219 deaths, ceiling never
  bound. Serial replay as a background audit. Quarantine lineage now runs
  C67 through C76; the audits grind in spare capacity.
- **The escalation criterion is moot**: the condition — pipe-x cells
  retained but no scene change — cannot fire, because the scene change
  happened. The track-2 waypoint mechanism stays in reserve, undis­patched.
- The missing-diversity diagnosis is confirmed end to end: one added key
  dimension in one registered room converted a two-link stall into a
  two-world advance. The chain stands at the sixth of eight worlds.

## C77 — registered concentrated conquest, eleventh link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c00f`, action limit 4096, retention
  `probe_at_admission_45`, vocabulary `down_ten_mask`, archive bound
  131,072, key policy `frozen_room_x_16:3,1,208` carried unchanged — the
  registered room is behind the frontier and the term is inert everywhere
  else, so the stack stays exactly the pilot's. One live run, no retries;
  serial replay as a background audit.
- Origin: the resume-rule assumption check on C76 returns recorded 166,
  viable 166, **play 165** — a one-bucket divergence, so per the standing
  precedent the link launches from a derived single-entry origin at the
  play frontier: the shortest input among the nine entries at
  `(5, 0, 165)`, entry 39040, 2,064 actions, chosen by the resume rule's
  own ordering. Derived origin
  `target/smb-completion/c76-conquest/origin-play165.json`, SHA-256
  `01e2e25b13b87e98b046b7cc3a0ff0c9228c393367341242f5c6526ce33c3535`.
- Raw destination: `target/smb-completion/c77-conquest/` on the ARM
  machine, log `c77.log`.
- Launch note: the first attempt exited at file-not-found — the launch
  script's global rename had clobbered the derived-origin path — with zero
  executions and no stream, so the registered arm was not consumed; the
  path was corrected and the same arm launched. Films: C76's furthest
  trajectory through the warp and the fifth world is cut and delivered.

### C77 live result — two more levels of the sixth world

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c00f`, the full promoted stack, from the derived
  play-165 origin: 29,942 retained against 16,366 rejected with 11,995
  deaths, ceiling never bound. **The sixth world's first level completed at
  bucket 213, its second opened at execution 739 and completed at 213, and
  its third opened at execution 22,537, driven to bucket 49** with
  watermark 58. Serial replay as a background audit.
- The assumption check returns recorded 49, viable 48, play 49: the
  frontier answers the controller — genuine play — while needing active
  input to survive the no-input horizon, which is combat, not a sinker.
  Play equals recorded, so the next link resumes directly.

## C78 — registered concentrated conquest, twelfth link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c010`, the full promoted stack unchanged, sourced
  from C77's live archive, SHA-256
  `9b3fbba8c5f7936ea856b1a9a63ae26d9ad0225445c80c00386ba0c4da49a805`. One
  live run, no retries; serial replay as a background audit.
- Raw destination: `target/smb-completion/c78-conquest/` on the ARM
  machine, log `c78.log`.

### C78 live result — the sixth world falls and the seventh opens

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c010`, the full promoted stack, from C77's archive:
  31,760 retained against 13,871 rejected with 12,810 deaths. **The sixth
  world's third level completed at 163, its castle opened at execution
  30,322 and completed at 144, the seventh world opened at execution
  43,174 with its first level driven to 177, and its second level opened
  at execution 47,585, reaching bucket 27** — watermark equal, play equals
  viable equals recorded at 27, settled play, direct resume. Serial replay
  as a background audit.

## C79 — registered concentrated conquest, thirteenth link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c011`, the full promoted stack unchanged, sourced
  from C78's live archive, SHA-256
  `02ffdfa8a464d632b6dad6e0db6430c9ac6696f80858f7efc25237868d60981f`. One
  live run, no retries; serial replay as a background audit.
- Raw destination: `target/smb-completion/c79-conquest/` on the ARM
  machine, log `c79.log`.

### C79 live result — the seventh world's castle gate

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c011`, the full promoted stack, from C78's archive:
  36,361 retained against 16,224 rejected with 7,933 deaths. **The seventh
  world's second level completed at 195, its third at 221 — opened at
  execution 38,611 — and its castle opened at execution 45,988, reaching
  bucket 18** with watermark 20; play equals viable equals recorded at 18,
  settled play, direct resume. Serial replay as a background audit.

## C80 — registered concentrated conquest, fourteenth link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c012`, the full promoted stack unchanged, sourced
  from C79's live archive, SHA-256
  `04eee8652065cc1a0059a2927bbf463e21ddb6d34ae5aa3a531d345f8d92f3e7`. One
  live run, no retries; serial replay as a background audit.
- Raw destination: `target/smb-completion/c80-conquest/` on the ARM
  machine, log `c80.log`.

### C80 live result — half the castle maze

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c012`, the full promoted stack, from C79's archive:
  31,747 retained against 32,419 rejected with only 884 deaths. **The
  seventh world's castle advanced from bucket 18 to 73** — watermark equal.
  The number shape is the castle's looping maze rather than combat: deaths
  collapsed while rejections doubled, wrong routes snapping back into
  already-keyed states. Not a stall — fifty-five buckets gained — and the
  maze diagnosis stands ready if the next link flattens. Serial replay as a
  background audit.
- The assumption check returns recorded 73, viable 73, play 72; per
  precedent the next link launches from a derived single-entry origin at
  the play frontier — 1,124 entries sit at bucket 72, the loop's
  concentration point, and the shortest is entry 21063 at 3,082 actions.
  Derived origin `target/smb-completion/c80-conquest/origin-play72.json`,
  SHA-256
  `bb6a0a808ef574c3c3ab2baac81d44695c23ab82b425e8b12cb29933f86f522c`.
- Headroom flag, recorded ahead of need: the resume input is 3,082 of the
  4,096-action limit, and links consume roughly 250 to 300 actions; the
  action ceiling may approach again near the eighth world's end, and a
  raise there would be a ruling, not a surprise.

## C81 — registered concentrated conquest, fifteenth link

- Observational, in the C49/C61 pattern. One live concentrated-policy
  conquest campaign, 50,000 executions, twelve workers on the ARM machine,
  campaign seed `0x5eed_c013`, the full promoted stack unchanged, from the
  derived play-72 origin above. One live run, no retries; serial replay as
  a background audit.
- Raw destination: `target/smb-completion/c81-conquest/` on the ARM
  machine, log `c81.log`.

### C81 live result — the maze flattens the chain at bucket 73

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c013`, the full promoted stack, from the derived
  play-72 origin: 24,231 retained against 39,788 rejected with 1,189
  deaths. **Its ladder tops at `(6, 3, 73)` — exactly the origin frontier,
  zero new buckets — and the watermark never moved**, which carries the
  diagnosis's first fact: no execution ever observed bucket 74, so nothing
  forward was ever produced and discarded — the loop gates forward progress
  itself. The maze fork's flatten branch fires; the standing plan runs.
- The film locates the frontier precisely: the trajectory crosses the
  castle's lava span and ends in the three-lane brick section — the maze
  fork where the game checks the player's lane at page crossings and snaps
  wrong routes back. Serial replay as a background audit.

## D82 — preregistered loop differential

- Sixth entry in the diagnostic pattern library. Question, fixed before
  execution: **which work-RAM state separates maze states that advance from
  states that loop?** The probe: up to 400 archive entries at the maze pair
  in buckets 60 through 73, each restored via the shared-prefix boundary
  machinery, then driven forward under held Right for ten sixty-frame
  chords; an entry whose observed bucket exceeds 73 classifies as advanced,
  one that snaps more than four buckets below its own bucket as looped.
  The starting work RAM of the two classes is then differenced byte by
  byte, reporting perfect separators first and the strongest imperfect
  discriminators after, twenty-four in all.
- Decision shape, per the standing maze plan: if a clean route-state
  variable emerges and route-correct states exist in the archives'
  collapsed cells, the pre-authorized room-x-class key term registers for
  the maze variable without a further ruling; a surprising diagnosis
  escalates instead.
- Raw destination: `target/smb-completion/d82-loop-diff/` on the ARM
  machine, versioned output, progress lines on.

### D82 findings so far — no route variable; the correct lane is unoccupied

- **First probe, buckets 66 through 73: four hundred of four hundred states
  loop under held Right, zero advance.** No advancing class existed, so no
  differential was possible. Joined with the archive keys, the entire
  frontier population sits in vertical bands 8 through 11 — the lower
  corridors — and bands 0 through 7 hold zero retained states anywhere in
  buckets 50 through 73.
- **Corrigendum, recorded because a wrong reading nearly launched a link**:
  the second probe over buckets 50 through 69 reported 89 states
  "advanced", and a derived origin was drafted from one before its own
  numbers were checked — the tool classified *advanced* relative to the
  sampled range's top, not the checkpoint, and the true crossing count was
  **zero**; every "advance" was a walk from 69 to the wall at 70 to 72. The
  drafted origin was deleted unregistered, and the tool now takes an
  explicit advance threshold instead of inferring one.
- The weak discriminators the second probe surfaced are positional and
  timing bytes, none separating — consistent with the loop check being a
  positional test at the crossing rather than a stored route flag.
- The census upstream: upper-band states exist only in buckets 0 through
  29 — 86 entries — and vanish from 30 on; the population loses altitude
  early in the section and never regains it. The third probe, registered
  with the corrected explicit threshold of 73, samples 500 entries from
  buckets 0 through 29 under thirty held-Right chords to answer whether
  any retained entrance state crosses the whole maze — the last free
  question before this escalates as selection-pressure design.
- The exhaustion note for the eventual ruling: the loop keeps retention
  fertile — 24,231 entries retained inside it — so the barren-threshold
  leg, which measures retention rather than progress, cannot see this trap.

### D82 verdict — a steering problem the probe repertoire cannot cross

- The third probe closes the measurement: from 500 entrance states in
  buckets 0 through 29, **zero crossed the checkpoint**, six looped, 38
  died in the castle's hazards and 456 stalled against mid-castle
  obstacles — held Right alone cannot traverse the section, so lane
  verification beyond this point requires play-quality input, which is
  search, not measurement. The pattern library has extracted everything a
  recorded-artifact probe can.
- The complete measured picture for the ruling: the checkpoint at bucket
  73 snaps back every state the archive holds there — all of which sit in
  the lower vertical bands; the upper bands are empty from bucket 30
  onward across roughly 150,000 maze executions; no stored route variable
  exists, the check is positional at the crossing; the winning selection
  band sits forty-plus buckets above the entrance region where the only
  upper-band states live, so that region receives no draws; and the loop
  keeps retention fertile, so the barren-threshold leg — which measures
  retention, not progress — structurally cannot see the trap.
- **Escalated to the integrator as selection-pressure design, the fork the
  standing plan reserved.** The candidate arms, stated with their
  characters: the track-2 waypoint mechanism, twice named in reserve, now
  with a measured target — occupy the upper corridor across the crossing;
  or a backward-retention-refusal policy value — refuse candidates whose
  bucket snaps well below their parent's within a pair — which starves the
  loop of retention, lets the existing barren-threshold fall-through walk
  selection back to the entrance, and generalizes to any future loop trap,
  at the cost of being slower and indirect. A pinned selection window is
  the third shape, waypoint-lite without the track-2 machinery. The chain
  holds until ruled; audits continue.

### Ruling on the maze — backward-retention refusal, gated on free censuses

- The integrator ruled option two — the backward-retention-refusal policy —
  plus a companion vertical key term for the maze section, both gated on
  free censuses first, with the rationale recorded: generality pays twice
  because the eighth world's maze needs the same immunity; refusal treats
  the structural blindness — fertile-loop invisibility — rather than
  overriding it per instance; and it is this program's analog of pruning
  dead branches. Track-2 waypoints stay in reserve a third time under a
  preregistered trigger, to be applied unsoftened: entrance draws working
  and vertical retention holding but no checkpoint crossing within the
  pilot's budget dispatches waypoints immediately.
- **Census one — snap-delta distribution, from the recorded archives, no
  emulation.** Backward same-pair parent-to-child deltas across every
  archive on the machine are bimodal: none at all between 1 and 30, then
  the maze snap massing at 31 through 50-plus — 12,068 entries at the cap.
  A refusal threshold of sixteen has enormous margin on both sides;
  sixteen is registered.
- **Census two — false-positive scan of the refusal predicate, same
  sources.** Retained entries with backward delta above eight outside the
  maze pair: **twelve, program-wide** — seven in the water level, five in
  the sixth world's first level — against 12,812 inside the maze trap. The
  predicate's measured collateral is negligible, and the policy is
  header-recorded per campaign in any case.
- **Calibrating fact for the companion term**: the frozen key already
  carries a global sixteen-pixel vertical band — altitude is keyed
  everywhere, unlike the H75 case where x was genuinely absent. Whether a
  further maze-scoped vertical term has any work to do is exactly what the
  third census decides: the y-transit over C81's stream, asking whether
  upper-band candidates at buckets 30 through 73 were produced and
  rejected — a cell or probe defect — or never produced at all, a pure
  production bottleneck that refusal-plus-fall-through addresses alone.

### D83 result — a pure production bottleneck; the companion key term has no work

- The y-transit census over C81's stream re-derived 4,000 frontier jobs —
  5,019 candidate boundaries — and found **zero upper-band candidates
  anywhere in buckets 30 through 73**, against 3,471 lower-band candidates
  there. Upper states are never produced past the entrance, not produced
  and rejected, so the collision the companion vertical key term would
  prevent cannot occur — and the frozen key already carries a global
  sixteen-pixel vertical band in any case. **Per the ruling's gate, the
  companion term does not build; backward-retention refusal is the whole
  fix.** Raw evidence:
  `target/smb-completion/d83-y-transit/c81-ytransit-v2.json`.

### The snapback-refusal build — gated, inert, ready

- `probe_at_admission_45_snapback_16`: the 45-frame admission probe plus
  refusal of any candidate whose progress lands more than sixteen buckets
  below its immediate parent's within the same `(world, level)` pair — the
  threshold the snap-delta census registered, with its measured
  twelve-entry program-wide false-positive footprint. Refusals are recorded
  in the stream as their own decision kind and counted in the report as
  `snap_refused`, omitted when zero so every legacy artifact serializes
  byte-identically. Header-recorded like every policy before it; replay
  reproduces refusals deterministically from the recorded policy.
- The four quality gates pass; the standing inertness reference re-verifies
  byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.

## C84 — registered snapback pilot, sixteenth link

- Pilot and chain link in one, per the maze ruling. One live
  concentrated-policy conquest campaign, 50,000 executions, twelve workers
  on the ARM machine, campaign seed `0x5eed_c014`, action limit 4096,
  retention `probe_at_admission_45_snapback_16`, vocabulary
  `down_ten_mask`, archive bound 131,072, key policy
  `frozen_room_x_16:3,1,208`. One live run, no retries; serial replay as a
  background audit.
- Origin: C81's play measurement reads recorded 73, viable 73, play 72, so
  per precedent the pilot launches from a derived single-entry origin at
  the play frontier — the shortest of 1,056 entries at `(6, 3, 72)`, entry
  3073, 3,082 actions. Derived origin
  `target/smb-completion/c81-conquest/origin-play72b.json`, SHA-256
  `aa28ca4b3c176babde811ac0cde27e90731116d2a6ff38a40e59dd757f58168a`.
- **Preregistered pilot question: does retention cross bucket 73?**
  Expected mechanics, stated for honest reading: refusal starves the loop
  band, the barren threshold finally binds, fall-through walks selection
  toward the entrance's 86 upper-band states, and new high cells at bucket
  30 onward become fresh draw targets as they appear.
- **The waypoint trigger, verbatim from the ruling and unsoftened**: if the
  recorded stream shows entrance draws working and vertical retention
  holding — upper-band cells appearing at bucket 30 onward — but no
  checkpoint crossing within this run's budget, the track-2 waypoint
  mechanism dispatches immediately, reported the moment the condition is
  read from the ladder and archive.
- Raw destination: `target/smb-completion/c84-conquest/` on the ARM
  machine, log `c84.log`, sentinel discipline in force.

### C84 pilot result — refusal fires, the cascade does not

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c014`, the snapback-refusal stack, from the derived
  play-72 origin. **The preregistered question resolves no: the ladder
  tops at `(6, 3, 73)`, watermark equal.** Serial replay as a background
  audit.
- The mechanism accounting, reported prominently: `snap_refused` 22,541 —
  the refusal fired on nearly half of all candidates, exactly as designed —
  yet `classes_skipped` and `counter_resets` are **zero**, 18,618 entries
  still retained, and zero upper-band cells appeared at bucket 30 onward.
  The cascade died at its first joint: the loop band never went barren,
  because the trap is fertile through **local** diversity — within-band
  wiggles retained through fingerprint and vertical-cell churn in buckets
  57 through 73, which the snapback predicate deliberately does not touch.
  The barren threshold of sixty-four draws per parent remains
  arithmetically unreachable at measured draws-per-parent, the same
  shortfall recorded since the selection panels, now binding in the one
  regime the leg was expected to serve.
- **The waypoint trigger's premise was not reached**: entrance draws never
  happened and vertical retention never appeared, because fall-through
  never engaged. The trigger as registered — entrance draws working,
  vertical retention holding, no crossing — cannot fire on this record, and
  the condition the ruling ordered reported-on-sight is therefore reported
  as failed-upstream-of-premise: the pilot did not reach the state the
  trigger guards. Escalated to the integrator with the measured mechanics;
  the chain holds. The threshold question the C67-era note called moot —
  lowering the barren threshold to a value real draw rates can reach —
  returns to the table with this pilot as its evidence, beside the waypoint
  dispatch it was meant to gate.

### Ruling on the cascade gap — the pinned window builds; threshold surgery becomes debt

- The integrator ruled on the C84 escalation, opening with a recorded
  concession: the cascade carried an unpriced dependency — the barren
  threshold's arithmetic was already refuted by the selection panels'
  measured draws-per-parent — and the report drawing that line is the
  record working. The ruling: the **pinned selection window** — the
  original escalation's third shape — builds as a registered policy value
  scoped to the maze section; refusal stays in the stack and carries to the
  final castle; **neither threshold surgery nor waypoint machinery builds
  today**, and global barren-threshold surgery is declined mid-endgame,
  recorded as standing mechanism debt for the instrumentor era with census
  (a) below as its evidence.
- **Census (a) — wiggle-novelty arrival curve, C84's archive, free.**
  Retentions in buckets 57 through 73 per five-thousand-execution bin:
  1,598, 916, 609, 405, 402, 306, 426, 293, 202, 161. Geometric decay with
  a long tail — the trap manufactures novelty ever more slowly but would
  stay marginally fertile for hundreds of thousands of executions. The
  doctrine point, recorded as the debt's evidence: exhaustion machinery
  keyed on total barrenness cannot see a trap with a long novelty tail.
- **Census (b) — entrance integrity, C84's archive.** The entrance region
  survives in the pilot's own archive: 63 upper-band states across buckets
  0 through 19 plus lower-band population through 29. Pin parameters
  registered from it: pair `(6, 3)`, window buckets 0 through 29.
- **Census (c) — draws per parent, C84's stream.** 11,414 distinct parents
  drawn, mean 4.39 draws, lifetime maximum 36 — no parent has ever come
  within half of the sixty-four threshold.
- **The build**: `pinned_window_128:6,3,0,29` — every selection draw,
  uniform and tie-class alike, narrows to active entries of the registered
  pair inside the registered window, with the concentrated recency draw
  applied within the pin and fall-back to promoted behaviour only when the
  pin is empty. Header-recorded in `parent_scheduler`; legacy identifiers
  parse unchanged; the four quality gates pass and the standing inertness
  reference re-verifies byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.

## C85 — registered pin pilot, seventeenth link

- Pilot and chain link in one. One live conquest campaign, 50,000
  executions, twelve workers on the ARM machine, campaign seed
  `0x5eed_c015`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, archive
  bound 131,072, key policy `frozen_room_x_16:3,1,208`, selector
  `pinned_window_128:6,3,0,29` — **the pin is the single variable changed
  from C84**, launched from the identical derived play-72 origin, SHA-256
  `aa28ca4b3c176babde811ac0cde27e90731116d2a6ff38a40e59dd757f58168a`. One
  live run, no retries; serial replay as a background audit.
- **Preregistered pilot question, unchanged: does retention cross bucket
  73?**
- **The waypoint trigger transfers verbatim and unsoftened**: pin
  demonstrably working — entrance draws happening and upper-band cells
  appearing at bucket 30 onward — but no crossing within this budget
  dispatches track-2 waypoints immediately, reported the moment the
  condition is read.
- Raw destination: `target/smb-completion/c85-conquest/` on the ARM
  machine, log `c85.log`, sentinel discipline in force.

### C85 pilot result — the pin works, the maze holds, the waypoint trigger fires

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c015`, the pinned stack, from the same derived
  play-72 origin as C84: 27,538 retained against 37,098 rejected with 523
  deaths, `snap_refused` zero — selection never left the entrance region
  far enough to snap, exactly as a working pin predicts.
- **The pin demonstrably worked**: upper-band cells appeared at bucket 30
  onward for the first time in the program's history — 1,904 across buckets
  30 through 39 and 181 across 40 through 49, where every previous campaign
  recorded zero. Entrance draws happened throughout; vertical retention
  held. The upper route's population thins out by bucket 49 and **the
  checkpoint did not cross**: the ladder tops at `(6, 3, 72)`, watermark
  equal. The preregistered question resolves no.
- **The transferred waypoint trigger's condition is met on its exact
  terms** — pin working, entrance draws happening, upper-band cells at 30
  onward, no crossing within the budget — and per the ruling it is applied
  unsoftened: **the track-2 waypoint mechanism dispatches immediately**,
  reported the moment this record was read from the ladder and archive.
  The track-2 specification receives its first read under this dispatch;
  registration follows its ladder. Serial replay of C85 continues as a
  background audit; the chain holds pending the waypoint registration.

### D86 — the descent diagnosis: the upper route starved in the pin's shadow

- Pattern-library measurement on C85's recorded artifacts, run as the
  session lane of the waypoint dispatch; its output parameterizes the
  declared region.
- **The fine census**: the upper population splits into two segments —
  strong at buckets 30 and 31, a near-empty physical gap at 32 through 34,
  a second segment at 35 through 45 peaking at 104 — and goes extinct at
  46. The film of the deepest upper trajectory shows stepped ledges with
  gaps, ending mid-descent between platforms, no enemy and no death.
- **The child census overturns the terrain reading**: of 564 upper parents
  at buckets 35 through 45, **467 — eighty-three percent — have zero
  retained children**, and the reason is structural, not terrain: the pin's
  static window at buckets 0 through 29 excluded every one of them from
  every draw. The 97 retained children all came from single long-action
  suffixes reaching out of pinned parents — one 120-frame hold covers
  roughly eleven buckets — which also sets a hard reach ceiling near bucket
  50 for the entire pilot. **C85 could not have crossed the checkpoint by
  construction**; recorded plainly because honesty requires it, and the
  waypoint dispatch stands regardless by the ruling's letter.
- Cause ranking for the record: starvation by static window first; forced
  descent through ledge gaps second — 33 descended against 48
  upper-forward among the retained children; deaths a non-factor at 523
  for the whole run.
- **The declared region, parameterized from this diagnosis**: pair
  `(6, 3)`, buckets 30 through 73, vertical bands 0 through 7 — the upper
  corridor from past the entrance gap to the checkpoint. Unlike the static
  pin, the mechanism's preference follows the region's population wherever
  it deepens, which is exactly what the measured starvation calls for:
  iterated draws on the deepest upper states, extending chains across the
  ledge gaps the one-shot reaches could not.
- The waypoint registration is drafted against this region and waits, per
  the ruling, on the builder's mechanism; the preregistered next
  escalation is stated now — if waypoint steering also fails, the fork
  goes to the program's owner as strategy, not to more mechanism.

### The waypoint mechanism lands — built, reviewed, merged

- The waypoint steering mechanism, built by a separate builder on its own
  branch and independently reviewed and approved by the integrator, is
  merged: a registered policy value
  `waypoint_4:world,level,low,high,band_low,band_high` granting a declared
  region auxiliary retention — four entries per cell against the base two —
  and selection preference through the existing concentrated recency draw,
  with per-member exhaustion at the standing threshold so a sterile region
  falls through rather than livelocking. Waypoint draws, auxiliary
  retentions and waived snapbacks are all stream-annotated and counted.
  Header-recorded; legacy streams carry no field and replay byte-identical.
- The integrator's rulings on the builder's open questions are recorded as
  confirmed-as-built: auxiliary capacity stays a compiled constant with a
  new identifier if ever changed; the in-region snapback exemption stands,
  guarded by integrator review of every declared region plus capacity caps
  and the `waypoint_snap_exempt` counter; preference strength and the
  per-run lifecycle stand as built.
- **Question five verified for this registration**: C86 runs the
  `frozen_room_x_16:3,1,208` key policy, under which states at pair
  `(6, 3)` carry the frozen vertical term — the raw sixteen-pixel bucket —
  and D86's census read exactly that key field from C85's archive under the
  same policy, so bands 0 through 7 in the registration mean precisely what
  the diagnosis measured.
- Gates on the merged tree: all four pass with 92 tests; the standing
  inertness reference re-verifies byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`; the
  ARM machine's release build — the aarch64 compile gate — passes on sync.

## C86 — registered waypoint pilot, eighteenth link

- The waypoint mechanism's debut, per the standing dispatch: an
  integrator-approved, measurement-declared region. The model-declared
  rung arrives later through other machinery; recorded for honesty.
- One live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c016`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  waypoint `waypoint_4:6,3,30,73,0,7`. **The pinned window is deliberately
  absent**, per the integrator's composition warning: the pin outranks the
  waypoint in selection and its window lies entirely outside the declared
  region, so a stacked registration would leave the waypoint
  selection-inert and reproduce C85's starvation. One live run, no
  retries; serial replay as a background audit.
- Origin: C85's live archive — which already holds the upper-corridor
  cells at buckets 30 through 49 that the waypoint preference draws
  directly — SHA-256
  `a3b0994aff6c1ac7241711aeaa1d68d26653e30af4f0de59f504c6d96e343d63`. The
  play measurement returns recorded 72, viable 72, play 72; direct resume.
- **Preregistered question, unchanged: does retention cross bucket 73?**
  The preregistered next escalation stands: if waypoint steering also
  fails, the fork goes to the program's owner as strategy, not to more
  mechanism.
- Raw destination: `target/smb-completion/c86-conquest/` on the ARM
  machine, log `c86.log`, sentinel discipline in force.

### C86 pilot result — the waypoint performs; the maze reveals its full shape

- The live run completed its full 50,000 executions, twelve workers,
  campaign seed `0x5eed_c016`, the waypoint stack without the pin, from
  C85's archive: 29,734 retained against 21,136 rejected with 1,167
  deaths. Mechanism accounting, all live: 37,395 waypoint selections,
  3,485 auxiliary retentions, 2 snapback exemptions, 13,034 ordinary snap
  refusals. Serial replay as a background audit.
- **The mechanism performed**: the upper corridor at the checkpoint itself
  is occupied for the first time in the program's history — 5,716
  upper-band cells at buckets 65 through 69 and 7,133 at 70 through 73,
  against zero past bucket 49 in every prior campaign. **And the
  checkpoint held**: the ladder tops at `(6, 3, 73)`, watermark equal. The
  preregistered question resolves no.
- **D87, the closing probe**: 600 states at buckets 65 through 73 driven
  forward under held Right — zero advanced. The lower band loops, 123 of
  123, replicating D82; the newly occupied upper band fails too, 234
  looping and 243 holding against terrain. With the upper population's
  leapfrog origin — long-action reaches over a near-empty 50-through-64
  upper gap — the maze's full shape is now measured: **a lane-sequence
  test across checks in the middle section**; states that skip the middle
  carry failed check-state regardless of their lane at the final crossing.
- **The preregistered escalation fires as registered: the fork goes to the
  program's owner as strategy, not to more mechanism.** The strategy
  options, stated with their measured characters:
  - **Reroute through the fourth world's vine warp.** The 4-2 warp zone
    reached by the hidden vine offers passage to worlds six, seven and
    eight directly — bypassing this castle entirely. The route uses only
    existing machinery: the frozen warp-room-era archives still hold that
    frontier, the vocabulary already carries Up for climbing, and a
    declared waypoint region can steer at the vine. The remaining game
    would then be the eighth world's four levels.
  - **Break the maze in sequence with the built mechanism**: declare the
    middle-section upper region — buckets 50 through 64 — as the next
    waypoint link so the check sequence is walked rather than leapfrogged,
    then re-declare the crossing region. Costs at least two more links
    with no guarantee the sequence semantics are lane-only.
  - **Wait for the model rung**: the instrumentor loop's diagnosis and
    macro stages land after the branches merge, and this obstacle is
    their natural first case.
- Raw evidence: `target/smb-completion/c86-conquest/` and
  `target/smb-completion/d87-loop-diff/` on the ARM machine; the chain
  holds pending the strategy ruling.

### Ruling on the maze strategy — routes are discovered, never imported

- The program's owner ruled on the C86 escalation, relayed by the
  integrator, and the first part is a standing principle recorded
  permanently: **the vine-warp reroute is rejected, hard.** The vine's
  existence is out-of-band game knowledge no search ever discovered;
  leveraging it would smuggle imported knowledge into the claim. Routes
  must be discovered by the machine. The option is struck.
- The path forward instead, from the owner's framing — what would a
  budget-spread explorer do — and C86's own data: the middle gap exists
  because the waypoint's recency draw concentrated on the newest tip
  cells and starved the population whose incremental extensions would
  fill the middle — the pin's starvation failure repeated one level up.
  Census first; on confirmation, a bucket-uniform draw allocation inside
  the region; on refutation, stop and escalate the numbers.
- **The census confirms, decisively.** Joining C86's 37,395
  waypoint-annotated draws to their parents' recorded keys: **87.8
  percent — 32,834 draws — sat on tip cells at buckets 65 through 73**,
  the middle at 50 through 64 received exactly two draws, and the base at
  30 through 49 received 4,559. Tip concentration is the gap's mechanism.
- **The build**: `waypoint_4_bucket_uniform` — the same declared region
  and preference, with the draw allocated bucket-uniformly: every
  occupied progress bucket in the region equally likely, the concentrated
  recency draw applied only within the chosen bucket, so gap-adjacent
  buckets earn turns instead of the newest cells absorbing the draw.
  Identifier in the existing family; the vocabulary rule is satisfied by
  construction — draw, bucket, region, uniform, no new names. Gates pass
  with 92 tests; the standing inertness reference re-verifies
  byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.

## C87 — registered bucket-uniform waypoint pilot, nineteenth link

- One live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c017`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  waypoint `waypoint_4_bucket_uniform:6,3,30,73,0,7` — **the draw
  allocation is the single variable changed from C86**. One live run, no
  retries; serial replay as a background audit.
- Origin: the resume check on C86's archive reads recorded 73, viable 73,
  play 72, so per precedent the pilot launches from a derived single-entry
  origin at the play frontier — the shortest of 1,284 entries at
  `(6, 3, 72)`, entry 3073, 3,082 actions, derived origin
  `target/smb-completion/c86-conquest/origin-play72c.json`, SHA-256
  `27cfc43c9d6a405efe942f0128bc48ce7b0b487e113cf8e0ad468ecd46f0186a`. The
  campaign's own bootstrap and the region preference then regenerate and
  spread over the occupied buckets.
- **Preregistered question, unchanged: does retention cross bucket 73?**
  The preregistered next escalation stands unchanged: failure goes to the
  program's owner as strategy.
- Raw destination: `target/smb-completion/c87-conquest/` on the ARM
  machine, log `c87.log`, sentinel discipline in force.

### C87 result — void as a steering verdict: the waypoint idled all run

- The live run completed its full 50,000 executions, campaign seed
  `0x5eed_c017`, header confirming `waypoint_4_bucket_uniform:6,3,30,73,0,7`
  — and every waypoint counter is zero: no selections, no auxiliary
  retentions, no exemptions, not one state in the region across the whole
  run. The ladder tops at `(6, 3, 73)`. **This is not a steering null; the
  mechanism never engaged**, and the result is recorded void for the
  registered question rather than counted as evidence against the draw
  variant.
- The investigation, in order, before any interpretation: the recorded
  header verified; the binary timeline verified fresh; the origin
  hash-verified against its registration; then a unit-scale A/B from
  C87's exact origin — both waypoint variants nucleate within 2,000
  executions at one schedule (999 and 983 selections) — followed by a
  seed-by-workers bisect in which **every other schedule cell is zero**,
  and a rerun of the nucleating cell which nucleates again (659
  selections). The matrix's meaning: **the waypoint as built amplifies an
  occupied region but cannot seed an empty one.** Nucleation — the first
  candidate landing in the region's bands and retaining — is left to
  unsteered base dynamics, which concentrate on the lower tip and are
  schedule-chaotic about ever producing it.
- **The origin choice was the session's error, recorded plainly**: the
  play-frontier derivation, applied mechanically per precedent, produced a
  single-entry origin that discarded C86's twelve thousand in-region
  states, so C87's region started empty and stayed empty. C86 itself
  nucleated from its shortest-input resume at the recorded maximum bucket
  73; C87's resume one bucket back never did.
- **A precedent conflict is hereby surfaced for ruling**: the play-frontier
  derivation exists to avoid resuming from an admitted sinker, and it
  serves ordinary links well; under a waypoint registration it can strip
  the origin of exactly the population the mechanism needs. The immediate
  zero-mechanism option is a re-registration from C86's full archive with
  the resume at its recorded maximum — the configuration that empirically
  nucleated — with the derivation waived and the waiver reasoned in the
  registration. The mechanism-shaped alternative is origin seeding —
  bootstrap walking the origin's in-region entries alongside the resume
  input — which is builder-class work. Escalated with the matrix; the
  chain holds.

### Ruling on the origin conflict — direct resume is the standing rule under region policies

- The integrator ruled option one, and promoted the waiver to a **standing
  rule, recorded here**: while a region policy is registered, links resume
  **direct from the full archive** — the single-entry play-frontier
  derivation is incompatible with region-preference mechanisms because it
  discards the population the preference exists to draw, which is exactly
  what C87 measured. The sinker risk the derivation guarded against is
  mostly mooted under a region policy: selection preference goes to region
  members, not the resume anchor.
- Origin seeding is **not queued** — cheapest-first: it builds only if a
  genuine sinker pathology appears under direct resume while a waypoint is
  registered, on that evidence, not speculatively. Recorded as the known
  fallback with that trigger.
- C87's disposition: **quarantined as a mechanism-incompatibility
  casualty, not a search result.** The draw-allocation question transfers
  intact to C88.

## C88 — registered bucket-uniform waypoint pilot, twentieth link

- One live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c018`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  waypoint `waypoint_4_bucket_uniform:6,3,30,73,0,7`.
- Origin: **C86's full archive, direct resume at its recorded maximum**,
  per the standing rule above — the configuration that empirically
  nucleated in C86 — SHA-256
  `11525604417956ad646e2742a3467922dd11447d5b6654df5329dfaae6b78d24`. The
  play-frontier derivation is waived under the standing rule; the waiver's
  reasoning is the C87 record.
- One live run, no retries; serial replay as a background audit.
  **Preregistered question, transferred intact: does retention cross
  bucket 73?** The escalation path is unchanged: on failure the numbers go
  to the program's owner.
- Raw destination: `target/smb-completion/c88-conquest/` on the ARM
  machine, log `c88.log`, sentinel discipline in force.

### C88 result — the draw variant succeeds completely; the maze refuses completely

- The live run completed its full 50,000 executions, campaign seed
  `0x5eed_c018`, bucket-uniform waypoint from C86's full archive under the
  direct-resume rule: 37,395 retained with 875 deaths, 37,641 waypoint
  selections, 5,171 auxiliary retentions, 8,750 snap refusals. Serial
  replay as a background audit.
- **The mechanism achieved its entire design goal.** The upper corridor is
  occupied in every five-bucket bin from 30 through 73 — 1,244 / 4,784 /
  3,533 / 714 / 586 / 1,778 / 400 / 2,996 / 3,994 — including 2,764 cells
  across the 50-to-64 middle that received two draws under the recency
  allocation. The check section was walked continuously, incrementally,
  in the upper lane, with the draw spread exactly as registered.
- **And the ladder tops at `(6, 3, 73)`, watermark equal: zero crossings.**
  The preregistered question resolves no, on a run where nothing about the
  mechanism underperformed. The simple lane-sequence hypothesis is refuted
  at scale: continuous upper-lane occupancy through the middle and at the
  wall does not satisfy the checkpoint, on top of D82's lower-lane
  refusals and C86's mixed-lane refusals. Whatever the maze's sequence
  semantics are, blind lane occupancy — lower, upper, or mixed — does not
  meet them, across roughly 250,000 executions and five links of
  mechanisms that each did what they were built to do.
- **Per the preregistration, the numbers go to the program's owner as
  strategy.** The mechanical facts for that decision: every steering
  mechanism built for this castle worked as designed and the checkpoint
  refused them all; the game's loop semantics are the one thing no
  recorded-artifact measurement can read, because they are the ROM's
  logic, not the archive's; and the model-facing diagnosis rung — built
  as the instrumentor loop's operator view on its own branch — is the
  standing candidate for reading them, on the integrator's prior note.
  The chain holds.

### Ruling on the maze numbers — let the search try all heights

- The program's owner ruled, relayed by the integrator: the upper-only
  band restriction encoded the falsified constant-upper hypothesis and is
  removed. The region widens to every vertical band; the archive then
  holds every lane at every bucket, and extensions compose lane changes
  between checks by pure trial and error — no new mechanism, no outside
  knowledge, no model call.

## C89 — registered all-heights waypoint pilot, twenty-first link

- One live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c019`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  waypoint `waypoint_4_bucket_uniform:6,3,30,73,0,15` — **the band range
  is the single variable changed from C88**, widened to all vertical
  bands.
- Origin: C88's full archive, direct resume at its recorded maximum per
  the standing rule, SHA-256
  `0fe031c278a5dd2acb591ca98705019fc68dc41b70fbeed66de88aae75a1eb84`.
- One live run, no retries; serial replay as a background audit.
  **Preregistered question, unchanged: does retention cross bucket 73?**
  Escalation path unchanged: the numbers go to the program's owner.
- Raw destination: `target/smb-completion/c89-conquest/` on the ARM
  machine, log `c89.log`, sentinel discipline in force.

### C89 result — the maze breaks

- **The preregistered question resolves yes: retention crossed bucket 73
  and ran to `(6, 3, 87)`, watermark equal.** The live run completed its
  full 50,000 executions, campaign seed `0x5eed_c019`, the all-heights
  region: 43,960 retained with 599 deaths, 37,621 waypoint selections,
  7,884 auxiliary retentions, 12 snapback exemptions, 5,781 refusals.
  Serial replay as a background audit.
- The mechanism story, recorded because it is the finding: five mechanisms
  and six links each did exactly what they were designed to do and the
  checkpoint refused them all — until the owner's ruling removed the
  band restriction that encoded the falsified constant-upper hypothesis.
  With every lane at every bucket held in the archive, bucket-uniform
  draws composed lane changes between checks by pure trial and error, and
  the sequence was satisfied with no new mechanism, no outside knowledge,
  and no model call. Routes are discovered.
- The resume check reads recorded 87, viable 84, play 84. **The waypoint
  is dropped for the next link** — the maze is crossed and the region now
  lies behind the frontier, where its preference would pull three
  quarters of draws backward — so the region-policy standing rule no
  longer applies and the play-frontier derivation returns: the next link
  launches from the derived single-entry origin at `(6, 3, 84)`, the
  shortest of four entries, entry 31547, 3,088 actions,
  `target/smb-completion/c89-conquest/origin-play84.json`, SHA-256
  `02f6e49a9daabac2f80b8f32b1dcd8887b9bfcfa1cb4ed480a2493acb1a9886a`.

## C90 — registered concentrated conquest, twenty-second link

- Observational, the chain resuming its ordinary shape past the maze. One
  live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c01a`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  no waypoint. One live run, no retries; serial replay as a background
  audit. From the derived play-84 origin above.
- Raw destination: `target/smb-completion/c90-conquest/` on the ARM
  machine, log `c90.log`, sentinel discipline in force.

### C90 result — the castle's back half

- The live run completed its full 50,000 executions, campaign seed
  `0x5eed_c01a`, the promoted stack without waypoint, from the derived
  play-84 origin: 29,591 retained with 1,843 deaths. **The castle advanced
  from 84 to `(6, 3, 136)`**, watermark 137 — the back half crossed, the
  axe section within a link's reach. Play equals viable equals recorded at
  136; direct resume. Serial replay as a background audit.

## C91 — registered concentrated conquest, twenty-third link

- Observational, ordinary chain shape. One live conquest campaign, 50,000
  executions, twelve workers on the ARM machine, campaign seed
  `0x5eed_c01b`, the promoted stack unchanged, sourced from C90's live
  archive, SHA-256
  `369cdf7017519faf95e1a08edbaee37c1345b75632b335631e6ba08abd06cd33`.
  One live run, no retries; serial replay as a background audit.
- Raw destination: `target/smb-completion/c91-conquest/` on the ARM
  machine, log `c91.log`, sentinel discipline in force.

### C91 result and D92 — a second check section before the axe

- C91 completed its full 50,000 executions, campaign seed `0x5eed_c01b`,
  the promoted stack: 27,636 retained with 1,278 deaths, and **the ladder
  advanced one bucket to `(6, 3, 137)`** — an effective stall. Play equals
  viable equals recorded at 137; 7,631 snap refusals; the frontier film
  shows the descending staircase structure. Serial replay as a background
  audit.
- **D92, the forward probe**: 400 frontier states at buckets 130 through
  137 driven under held Right for twenty chords — **zero advanced, zero
  died, 337 held against terrain, 63 snapped backward.** No combat
  gauntlet: nothing dies. The snap-backs at a page crossing plus the
  staircase geometry mirror the mid-maze shape exactly — the castle's
  loop system has a **second check section** between the first checks and
  the axe.
- The proven cure is the C89 recipe re-scoped: the all-heights
  bucket-uniform waypoint over the new check section. **Proposed region,
  submitted for the integrator's registration review as the waypoint
  ruling requires**: pair `(6, 3)`, buckets 120 through 144, all vertical
  bands — `waypoint_4_bucket_uniform:6,3,120,144,0,15` — from C91's full
  archive under the direct-resume standing rule, question: does retention
  cross 137 to the castle's completion. The chain holds for the review.

## C92 — registered all-heights waypoint pilot, twenty-fourth link

- The integrator's region review passed as submitted, with the
  classification recorded: D92's signature — zero deaths, terrain-held
  plus page-crossing snapbacks — matches the first check section's shape,
  so this is **the same obstacle class with the same cure, second
  instance**; the repetition is the class signal the altitude doctrine
  watches for, and it strengthens the expectation of a third instance in
  the final castle.
- One live conquest campaign, 50,000 executions, twelve workers on the ARM
  machine, campaign seed `0x5eed_c01c`, action limit 4096, retention
  `probe_at_admission_45_snapback_16`, vocabulary `down_ten_mask`, key
  policy `frozen_room_x_16:3,1,208`, selector `concentrated_recency_128`,
  waypoint `waypoint_4_bucket_uniform:6,3,120,144,0,15`.
- Origin: C91's full archive, direct resume per the standing rule, SHA-256
  `41b5417f20ebab62847a539a2dbf1ce7ccf9ef517250ff2928ccd4b6c7901ee5`. One
  live run, no retries; serial replay as a background audit.
- **Preregistered question: does retention cross 137 to the castle's
  completion?**
- Raw destination: `target/smb-completion/c92-conquest/` on the ARM
  machine, log `c92.log`, sentinel discipline in force.

### C92 result — the final check refuses every reachable lane

- The live run completed its full 50,000 executions, campaign seed
  `0x5eed_c01c`, the approved all-heights region over buckets 120 through
  144: 29,185 retained, 37,519 waypoint selections, 4,152 auxiliary
  retentions — fully engaged — **and the ladder tops at `(6, 3, 137)`
  unchanged.** The C89 recipe did not transfer. Serial replay as a
  background audit.
- The joined evidence revises the obstacle's classification: the D92
  snap-backs land at buckets 72 through 77 — **the maze sequence's own
  start** — so the barrier at 137 is not a second check group but the
  first sequence's final check, resetting failures to its beginning. The
  crossing geometry, from C92's own archive: the region's top bands were
  populated mid-section — 969 states in bands 0 through 4 across buckets
  128 through 135 — but the terrain funnels all lanes down approaching
  the crossing; at buckets 136 and 137 only bands 5 through 8 exist.
  Bands 9 through 11 hold at the wall without crossing; bands 5 through 8
  cross and fail; bands 0 through 4 cannot reach the crossing at all.
- **Every lane the game permits at the final check fails it**, across
  roughly 100,000 executions of all-lane composition. The satisfying
  condition is therefore not a lane at this page producible by
  composition — it lives in the loop logic's sequence semantics, which no
  recorded-artifact measurement can read. Escalated on the standing path:
  the numbers go to the program's owner. The recorded facts for that
  ruling: the same-class judgment is revised by the snap-target evidence;
  the all-heights cure engaged and failed; and the model-facing diagnosis
  rung remains the standing candidate for reading route semantics the
  archive cannot express. The chain holds.

### H75 registration corrigendum — the pilot's origin was C72's archive

- Surfaced while freezing the stall fixtures, from the recorded stream
  header, which is the ground truth: **the C76 pilot launched from C72's
  archive**, path and SHA-256
  `6040043ce5be7aa0671ab8cffe8b24fa68eff3352114cd02ee7cc22509556bc2`
  recorded in its header — while the H75 registration text named C70's
  archive and cited C70's hash. The registration prose was wrong; the run,
  its gates and its result are unaffected in substance because C70 and C72
  tie at the same frontier with the byte-identical shortest resume entry,
  1,547 actions, which is what the campaign actually resumes from. The
  quarantine lineage as written — C76 depending on C70's audit — transfers
  to C72's audit, which also passed. Recorded rather than edited in place,
  per the standing practice for corrections.
- The two held-out stall fixtures are frozen on the integrator's
  instruction for the instrumentor-loop mechanism's validation: the
  warp-room stall (C72 pre-room-x, tarball SHA-256
  `e109c3f4835e45b86e29699ff11049365f1e9414a86b7ae1ea9a595960d7f341`) and
  the maze stall (C81 pre-refusal, tarball SHA-256
  `e55b5559954bc615bc528d0b5a3459f5a342370b2bfdee0521234f62da7998e5`),
  with manifest and provenance README at the neutral fixtures location
  outside this tree; box copies stay in place, excluded from cleanup.

### D83 execution note — a launch crash sat undetected behind a blind waiter

- The first y-transit launch died in its first second — the census
  reconstructs the resume input from the produced archive's frontier rule,
  and C81 launched from a **derived origin**, so the reconstruction
  mismatched the recorded resume hash: the D73 defect class again, caught
  by the integrator after hours of idle box because the waiter was watching
  for an output file that a launch-time error never creates. Two fixes,
  both now checklist: the census takes the run's origin archive explicitly
  for derived-origin lineages; and **every remote launch now emits a
  completion sentinel on success or failure that the waiter matches on
  either branch**, with the first progress line confirmed in the log before
  parking. The relaunch's first progress line was confirmed before this
  entry was written.

### Ruling on the action ceiling — raised to 8192 ahead of need

- The integrator ruled on the standing headroom flag before any link could
  wait on it: the compiled action ceiling rises from 4096 to 8192, same
  doctrine as both precedents — a ceiling is not an allocation. The
  arithmetic breaches before the final castle: 3,082 actions consumed at
  the C81 origin plus roughly 250 to 300 per link across four or more
  remaining links.
- This is the light case of the three ceilings: the per-run action limit
  has been recorded in every stream header since campaign mode began, and
  replay already retains and validates under the recorded value, so the
  change is the validation ceiling alone. Every recorded stream replays
  under its recorded limit unchanged.
- The four quality gates pass; the standing inertness reference
  re-verifies byte-identical,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.
  Landed at a natural pause with C81 in flight untouched; C81's audit
  replays under its recorded 4096.

## Completion-claim protocol — registered ahead of the final link

- Registered on the integrator's ruling so the endgame's evidence standard
  exists before any link needs it. **The completion claim is gated, not
  audited.** Preregistered pass criteria:
  - **Replay gate (a)**: the winning link's campaign replay must be
    byte-exact — stream, archive and report — *and* a full from-power-on
    serial replay of the completion lineage's input must reproduce the
    identical extended ladder and final state hash on the ARM machine,
    before any claim is made.
  - **Film (b)**: the full-game film is rendered from the recorded stream,
    power-on through the final castle's axe, and is the deliverable.
  - **Evidence set (c)**: the winning stream, the origin archive chain back
    to genesis, the recording commit hashes, and the gate outputs are
    preserved together so the claim re-derives from git plus artifacts
    alone.
  - **Cross-machine replay (d)**: a Mac replay of the same stream is
    desirable as determinism evidence and is run and reported — divergence
    included — but does not gate the claim.
- On the maze fork the standing plan holds as stated, no new ruling unless
  the diagnosis surprises.

### Ruling on the archive ceiling — raised to 131,072, recorded per run

- The integrator ruled on the C68 ceiling finding, same doctrine as the
  action cap: a ceiling is not an allocation, memory cost tracks actual
  retention, and at roughly 37 KB per entry even a full ceiling stays under
  5 GB on either machine. Sized so a future doubled-budget link still fits
  and this ruling class dies.
- The build follows the action-cap pattern exactly, because replay
  correctness demands it: **the entry bound is now recorded per run in the
  stream header and report, and replay retains under the recorded bound.**
  Streams recorded before the field existed — including C68's, which hit the
  old cap, and C69's, launched under the old binary — carry no field and
  default to 32,768, so every recorded stream replays byte-exact on the new
  tree without depending on which binary runs the audit. New runs record
  131,072. The compiled constant becomes the validation ceiling.
- C69, live at the time of this ruling, runs and audits at 32,768 by
  construction and its registration stands unmodified; C70 onward registers
  at the raised bound.
- The four quality gates pass; the standing inertness reference re-verifies
  byte-identical on the changed tree,
  `fa1f9aaf0279523ec46c3fe68022a1c5eb5da0aeb0268afef843fc4b4f04ea24`.

### Ruling note — the exhaustion incompatibility is deferred by design

- Recorded on the integrator's instruction. The C67 measurement is accepted:
  recency displacement and the sixty-four-draw barren threshold are
  structurally incompatible at high retention, and displacement itself
  bounds corpse waste in that regime — which is why the corpse-heavy link
  still broke through. The chain is uncontested.
- The ruling point arrives at the next genuine stall. The escalation menu
  fixed now, so it is not invented then: **(a)** lower the barren threshold
  to a value the stall regime can actually reach, justified by this data, or
  **(b)** remove the exhaustion leg outright — provably byte-inert on every
  recorded artifact precisely because it has never fired, per the standing
  no-dead-code preference. Neither registers now.
- **Withdrawn by the integrator after C68, recorded beside the original as
  instructed**: the menu above is overtaken by evidence. The leg fired for
  the first time against C68's full archive and behaved exactly as designed
  — bands exhausted in sequence, fall-through clean — so removal is off the
  table, and the threshold question is moot while displacement handles the
  productive regime. The escalation menu is void; no exhaustion change is
  pending under any condition currently registered.

### C66 audit verdict — passed

- The serial replay of C66's recorded stream completed all 50,000 executions
  byte-identically: `replay_verified` true, replay stream equal to the live
  stream `47826cfa…`. Every chain link through C66 is now fully audited;
  nothing is in quarantine and the audit backlog is empty.

### C65 audit verdict — passed

- The serial replay of C65's recorded stream completed all 50,000 executions
  byte-identically: `replay_verified` true, replay stream equal to the live
  stream `03a0b690…`, replayed archive SHA-256
  `64ead1a337921aa357129f11277288d7863303274c4ea569633f713b8d3076b6` — the
  exact hash C66's registration launched from. The chain is fully audited
  through C65; no quarantine attaches to C66's origin.
