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
