# dissonance

A from-scratch rebuild of the dissonance search loop on LibAFL. This
directory is its own Cargo workspace, deliberately outside the harmony root
workspace.

## The only three docs that apply here

- `docs/DISSONANCE-AUTORESEARCH.md` — the governing charter: target
  boundaries, benchmark ladder, experiment protocol
- `docs/QUICKNES-BACKEND.md` — the SMB/QuickNES workload backend
- `docs/DISSONANCE-SEARCHER-SCALING.md` — searcher scaling

The superseded plans (the from-scratch design sketch, the LibAFL phase plan,
the model-in-the-loop plan) are in git history; completed milestone evidence
is in `NOTES.md`.

## Do not read the old stack

When working in this directory, do NOT read or take vocabulary, abstractions,
or patterns from:

- the v1 crates (explorer, campaign-runner, resolution, etc. — git history)
- `docs/GLOSSARY.md` or the deleted legacy dissonance design docs

They describe a different decomposition with different names. Reusable ideas
were already carried over into the docs above; anything not there is out.

## Rules

- Vocabulary: LibAFL's own terms only — testcase, corpus, input, executor,
  observer, feedback, scheduler, stage, metadata. Do not coin new terms.
- No dependencies on `consonance/*` crates; plugging the consonance machine
  in as a target is a separate, deliberate decision.
- No LLM calls in unit tests. Model quality is measured in A/B campaigns,
  never in CI.
- No disabled-by-default features. Per-run recorded switches exist to run
  experiments; when an experiment concludes, the new behavior either
  becomes the default or its switch is deleted. Never land a feature that
  ships turned off.
