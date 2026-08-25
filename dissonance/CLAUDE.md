# dissonance

A from-scratch rebuild of the dissonance search loop on LibAFL. This
directory is its own Cargo workspace, deliberately outside the harmony root
workspace.

## The only three design docs that apply here

- `docs/DISSONANCE-FROM-SCRATCH.md` — the design
- `docs/LIBAFL-PLAN.md` — the verified LibAFL surface and the phased plan
- `docs/MODEL-IN-THE-LOOP-PLAN.md` — the current SMB Step 3 execution plan;
  completed M0–M7 evidence is in git history (`NOTES.md`)

## Do not read the old stack

When working in this directory, do NOT read or take vocabulary, abstractions,
or patterns from:

- `dissonance/` (the v1 crates — explorer, campaign-runner, resolution, etc.)
- `docs/GLOSSARY.md`, `docs/RESOLUTION.md`, `docs/DISSONANCE.md`,
  `docs/DISSONANCE-STRATEGY.md`, or other legacy design docs

They describe a different decomposition with different names. Reusable ideas
were already carried over into the three docs above; anything not there is
out.

## Rules

- Vocabulary: LibAFL's own terms only — testcase, corpus, input, executor,
  observer, feedback, scheduler, stage, metadata. Do not coin new terms.
- No dependencies on harmony crates (`consonance/*`, `dissonance/*`) before
  phase 5.
- No LLM calls in unit tests. Model quality is measured in A/B campaigns,
  never in CI (see "Determinism and testing" in docs/LIBAFL-PLAN.md).
- No disabled-by-default features. Per-run recorded switches exist to run
  experiments; when an experiment concludes, the new behavior either
  becomes the default or its switch is deleted. Never land a feature that
  ships turned off.
