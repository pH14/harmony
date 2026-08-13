# dissonance

A from-scratch rebuild of the dissonance search loop on LibAFL. This
directory is its own Cargo workspace, deliberately outside the harmony root
workspace. The `differential-lineage/` subdirectory is a separate standalone
workspace (the incremental consistency oracle) — not a member here and not
part of this rebuild.

## The only two design docs that apply here

- `docs/DISSONANCE-FROM-SCRATCH.md` — the design
- `docs/LIBAFL-PLAN.md` — the verified LibAFL surface and the phased plan

## Do not read the old stack

When working in this directory, do NOT read or take vocabulary, abstractions,
or patterns from:

- the v1 dissonance crates (explorer, campaign-runner, resolution, etc.) —
  deleted from the tree, but reachable in git history
- `docs/GLOSSARY.md`, or the legacy design docs (DISSONANCE, RESOLUTION,
  DISSONANCE-STRATEGY — also deleted, also reachable in git history)

They describe a different decomposition with different names. Reusable ideas
were already carried over into the two docs above; anything not there is out.

## Rules

- Vocabulary: LibAFL's own terms only — testcase, corpus, input, executor,
  observer, feedback, scheduler, stage, metadata. Do not coin new terms.
- `libafl` stays pinned at 0.15.4 until phase 5.
- No dependencies on harmony crates (`consonance/*`, `differential-lineage`)
  before phase 5.
- No LLM calls in unit tests. Model quality is measured in A/B campaigns,
  never in CI (see "Determinism and testing" in docs/LIBAFL-PLAN.md).
