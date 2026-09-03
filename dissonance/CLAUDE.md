# dissonance

Dissonance is a deterministic, from-scratch searcher over a machine boundary.
It does not use LibAFL. The workspace is deliberately separate from the
Harmony root workspace.

## Current architecture

- `machine/` owns snapshot/branch/replay/run/read and the pinned QuickNES edge.
- `searcher/src/search/` owns only game-neutral archive, selection, mutation,
  worker scheduling, recording, and exact replay.
- `searcher/src/smb/` and `searcher/src/nova/` are observation adapters. Game
  addresses, setup walks, progress, milestones, and state preferences stay in
  those adapters.

The relevant design/evidence documents are:

- `docs/DISSONANCE-AUTORESEARCH.md` — the governing charter, target boundaries,
  benchmark ladder, and experiment protocol
- `docs/DISSONANCE-SEARCHER-SCALING.md` — searcher scaling
- `docs/QUICKNES-BACKEND.md` — the NES/QuickNES workload backend
- `dissonance/NOVA.md` — the Nova workload, progress model, and campaign

## Do not read the old stack

Git history before `d09d9d38` describes a retired LibAFL implementation. Do
not copy its corpus, scheduler, executor, feedback, or phase abstractions into
the current searcher. Source-grounded game observations and reproducible
external build pins may be migrated when they fit the current interfaces.

## Rules

- The generic search layer must not name or branch on a game concept.
- Game policies cross that boundary only as opaque ordered keys, comparisons,
  observations, actions, snapshots, and recorded identifiers.
- A same-seed run and its serial replay must produce byte-identical reports
  and checkpoints.
- Long campaigns use the recorded stream plus final whole-tree checkpoint;
  do not reintroduce synchronous whole-tree rewrites during live search.
- Follow the root `AGENTS.md` determinism, unsafe/Miri, licensing, and quality
  requirements.
