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
