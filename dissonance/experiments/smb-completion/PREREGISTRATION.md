# SMB completion experiment preregistration

## Immutable setup

- `BASE_COMMIT`: `8f2b522c26c6f192f2db45a430bec03ed447cad7`
- Task branch: `codex/smb-completion`
- External ROM path: `/Users/phemberger/workspace/roms/Super Mario Bros. (World).nes`
- Required ROM SHA-256: `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`
- Target start: the checked-in deterministic gameplay-genesis snapshot reached from a clean emulator reset.
- Success: a search-produced input reaches the mechanically decoded final victory/credits state and an artifact-only, no-model replay reproduces the complete terminal trace exactly.

The experiment uses only this worktree at `BASE_COMMIT`. It does not inspect or import sibling worktrees, branches, later commits, transcripts, routes, trajectories, ROM disassembly, maps, walkthroughs, TAS data, or human/model-authored corpus inputs.

## Frozen baseline reproduction

The first run reproduces the checked-in M12 foundation before adding search machinery:

1. Verify the external ROM hash.
2. Run the frozen M5 six-seed, 500-execution baseline (`0x5eed_d700..=0x5eed_d705`) and require the recorded ratchet maxima `[3, 9, 7, 5, 7, 5]` under the then-current 64-pixel metric.
3. Reproduce the M10/M12 16-pixel restart control at seed `0x5eed_dc00`, 500 executions, from the autonomously generated source corpus.
4. Record the best retained input as the starting champion, replay it from gameplay genesis, and hash the input and replay trace.

Raw reports, corpora, films, generated source, and model transcripts live under ignored `dissonance-v2/target/smb-completion/`. Checked-in documents contain configurations, hashes, aggregate results, and paths only.

## Champion–challenger protocol

Every challenger is proposed only after inspecting the current champion's deterministic progress curve, retained testcase distribution, lineage, terminal-death transitions, frontier inputs, snapshots, and film. Each plateau gets exactly one written, falsifiable bottleneck hypothesis and the smallest generic implementation that tests it.

Unless a hypothesis preregisters stricter criteria, comparisons use:

- Development seeds: `0x5eed_e000..=0x5eed_e005`.
- Held-out seeds: `0x5eed_e100..=0x5eed_e105`.
- Paired arms receive the same seed, initial corpus, generated artifacts, and target-execution budget.
- Primary ordering: final credits success, then furthest mechanical milestone success, then lower executions-to-milestone, then larger integer progress-curve area.
- A challenger is retained only if it improves at least one primary measure on development seeds, does not regress any already-reliable milestone, and repeats the improvement on held-out seeds. Every promoted champion must replay exactly with no model process.
- Compute increases are permitted only while at least one paired progress curve is still rising, or after a retained material search change.

Each accepted generic change is committed separately. Rejected hypotheses remain in `LAB-LOG.md` and their raw evidence remains in the ignored output tree.

## Initial plateau hypothesis H1

The M12 search loses reusable near-frontier states because corpus novelty is global and endpoint-oriented: descendants that make local progress but collide with an existing global feature are discarded, forcing later mutations to reconstruct long prefixes.

Smallest test: add a deterministic snapshot-backed state archive that retains a bounded quality-diversity set keyed only by mechanically decoded, route-agnostic state features (world, level, coarse progress, player state, and a bounded state fingerprint), then run short seeded suffix search from archived testcases. No world-, level-, or absolute-position-specific action rule is permitted.

Development A/B budget: 5,000 target executions per seed. Acceptance: the challenger must improve milestone success count or integer progress-curve area on at least four of six paired seeds, with no replay failure and no median max-progress regression. If accepted in development, repeat unchanged on the six held-out seeds. If rejected, inspect the new plateau before proposing H2; do not increase the budget merely because H1 is flat.

## Model routing and evidence boundary

- Triage: GPT-5.6 Luna, low reasoning, existing wrapper and schema.
- Instrumentation: GPT-5.6 Luna, xhigh reasoning, existing wrapper and schema.
- No model-routing comparison and no default-model change.
- Models may label evidence or emit bounded generic detector/mutator code. They may not emit or insert an input, route, trajectory, known action sequence, or level-specific rule.
- Every request, prompt, response, generated file, validation result, seed, and budget is preserved before the next invocation.

