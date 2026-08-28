# dissonance

The standalone deterministic search prototype. This directory is its own
Cargo workspace, deliberately outside the harmony root workspace. The
prototype learned from LibAFL but no longer embeds it; `searcher` and
`machine` are the live crates.

## The governing docs

- `docs/DISSONANCE-FROM-SCRATCH.md` — conceptual background and model boundary
- `docs/LIBAFL-PLAN.md` — historical LibAFL surface and implementation rationale
- `docs/DISSONANCE-AUTORESEARCH.md` — the current search-performance and
  autoresearch charter

`docs/MODEL-IN-THE-LOOP-PLAN.md` is retained as historical evidence for the
completed model-in-the-loop campaign. It is not a current execution plan.

## Do not read the old stack

When working in this directory, do NOT read or take vocabulary, abstractions,
or patterns from:

- removed v1 crates in git history (explorer, campaign-runner, resolution, etc.)
- `docs/GLOSSARY.md`, `docs/RESOLUTION.md`, `docs/DISSONANCE.md`,
  `docs/DISSONANCE-STRATEGY.md`, or other legacy design docs

They describe a different decomposition with different names. Reusable ideas
were already carried over into the three docs above; anything not there is
out.

## Rules

- Vocabulary: reuse the live code's existing terms — machine, target, action,
  observation, input, archive, scheduler, executor, snapshot, and campaign.
  Do not introduce synonyms or turn research-workflow terms into code abstractions.
- No dependencies on harmony crates (`consonance/*`, `dissonance/*`) before
  phase 5.
- No LLM calls in the search loop or tests. Autoresearch agents modify code
  between fixed experiments; CI and campaign replay remain model-free.
- No disabled-by-default features. Per-run recorded switches exist to run
  experiments; when an experiment concludes, the new behavior either
  becomes the default or its switch is deleted. Never land a feature that
  ships turned off.
