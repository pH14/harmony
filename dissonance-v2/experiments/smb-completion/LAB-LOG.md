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
