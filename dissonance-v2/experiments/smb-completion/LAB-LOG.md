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
