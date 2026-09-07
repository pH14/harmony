---
name: nes-game-integration
description: Integrates a new NES game or repairs its workload adapter for Dissonance search evaluation. Covers ROM identity, deterministic setup, observations, minimal archive policy, replay, and qualification without encoding a game solution. Use for adding a game to the evaluation library, not ordinary gameplay or searcher-only tuning.
---

# Integrate an NES game with Dissonance

Use games to improve the searcher generically. A useful integration makes a
new search problem reproducible and measurable; it need not solve the game.
Do not fit the searcher, or hide a solution in the adapter, to obtain a win.

Paths below are relative to the Harmony repository root unless linked as a
skill reference. Read the current `AGENTS.md`, `dissonance/README.md`,
`dissonance/searcher/README.md`, and the chosen example's nearby README.

## 1. Establish the task and implementation boundary

- Inspect `git status`, the active branch/worktree, and current interfaces.
  Preserve concurrent game work. Old transcripts explain experiments; they
  do not establish today's API or authorize their recorded commands.
- Identify the supplied ROM/source, revision, native core, desired starting
  point, completion scope, and available compute. Use direct QuickNES first
  unless another backend is requested. Ask only for missing necessary inputs.
- Read [the implementation map](references/implementation.md) for files,
  contracts, and the validation ladder. Follow existing interfaces rather
  than creating a new search loop or undertaking a package migration.
- Choose a normal, self-contained game mode when available. Record difficulty,
  opponent behavior, and seeds as part of the workload identity. Preserve built-in
  autonomous opposition. A passive opponent or disabled hazard is a reduced
  fixture: useful for probes, but not the primary game evaluation unless requested.
  Do not weaken the challenge just to make the pilot succeed.
- Record whether this is a normal new game, an independently initialized
  level/stage, or a continuation from a searched input. Fixed menu navigation
  is setup; traversing gameplay is part of the search. Disclose any save edits,
  passwords, imported inputs, or scripted stage choices.

Output: a short adapter contract in the game's README, using the template in
[the implementation map](references/implementation.md). Fill it as evidence
arrives; mark unknowns rather than inventing them.

## 2. Make execution and observations trustworthy

Pin the ROM identity and emulator build/options. Use a supplied commercial
ROM locally; do not commit it or place it in public CI artifacts. For a
source-built game, pin and verify the build inputs and observation symbols.
Follow the repository's artifact policy and the game's documented permissions.

Reach a bounded, deterministic gameplay genesis with controller release frames
where presses are edge-triggered. Verify readiness from observations. Fail
clearly if setup does not reach it; do not silently continue from a menu.

Build a small probe that accepts recorded chords and emits decoded state and
only the relevant RAM fields. Ground addresses, bit order, units, and terminal
conditions in source/disassembly or controlled traces. Test both positive and
negative examples. In particular:

- Verify left/right, A/B, and hold duration against the emulator's encoding.
- Preserve gameplay controls such as weapon menus and missile toggles. Exclude
  a button only for documented semantics, not because it slows the pilot.
- Distinguish world position, screen position, camera scroll, and transitions.
  A wrapped coordinate is not proof of a fall or death.
- Observe events inside held actions. Preserve a reproducible endpoint or an
  exact shortened tape for a claimed event; a watermark alone is not a witness.
- Distinguish death, level clear, full ending, and infrastructure failure.
  An unknown ending means progress-only qualification, not completion.

Verify snapshot/restore with the same continuation twice and after taking a
different branch. Restore adapter caches, pending input, and observation state
as well as emulator bytes. If a retention probe is used, it must restore the
complete candidate state before normal execution resumes.

## 3. Define the smallest useful search signal

Read [signal design and failure examples](references/signals.md) before
implementing the archive key. Maintain a field table in the adapter README:
source, meaning, role, ordering, and evidence that the field is needed.

Keep these roles distinct:

| Role | Question it answers |
|---|---|
| Observation/report | What happened? It need not affect search. |
| Location/diversity | Which mechanically different states should coexist? |
| Progress | What objectively advanced? Coordinate/room IDs need not rank it. |
| Same-location preference | Which comparable state preserves more capability? |
| Terminal condition | What exact event ends this branch or completes this task? |

Start with spatial/topological identity, observed durable changes, and verified
terminal conditions. Add a field only to correct demonstrated aliasing or
represent a mechanically relevant capability. Expose minimal sufficient state,
not the full RAM as new independent archive dimensions. Do not use one game's
16-pixel buckets, preference order, or controller exclusions without checking
their meaning for this game.

Start with observed terminal conditions and ordinary admission, without extra
admission lookahead where the interface permits. Copying a survival probe also
copies input policy and compute cost. Add one only for an evidenced admission
problem; record its masks and actual horizon, inspect the simulated future for
its verdict, and verify complete restoration before enabling it.

Routes, waypoints, boss weakness tables, recommended weapon/item orders,
location-conditioned button choices, prerecorded solutions, and rewards for
following a walkthrough are strategy. Keep them out of the evaluation policy,
even inside the adapter. Mechanical input encoding and verified state decoding
belong there. Generic changes belong in search mechanisms and must be justified
without game nouns, then evaluated on other games.

Freeze the observation/key/input policy for a search comparison. Version a
semantic change and retain its prior baseline; an added hint is not an engine
improvement. Do not cherry-pick seeds or redefine success after seeing results.

## 4. Qualify before spending large compute

Use the validation ladder in [the implementation map](references/implementation.md):
decoder checks, deterministic execution, a small recorded campaign and replay,
then a bounded pilot. Headless search comes first; film the retained witness
separately and compare its endpoint with headless replay.

When a pilot stalls, inspect the tape, observations, admissions, and resource
usage. Classify the cause as execution/decoder error, insufficient state
identity, an unavailable control, a generic search limitation, or an exhausted
budget. State the evidence and make one relevant change. Do not hand-solve the
obstacle and turn that solution into input weights or a richer reward.

A generic search change needs a fixed-policy before/after comparison on this
game and another existing game, plus a meaningful synthetic invariant test.
Record executions, frames, elapsed search time, memory, and achieved outcomes.
Treat a single-game win as provisional. Follow the searcher README's performance
goals; do not launch an unbounded campaign or materialize huge reports by default.

## 5. Deliver a library entry with an honest status

Include the adapter, probe/campaign entry point, README field table and exact
local commands, relevant tests, and reproducible evidence. Record code and
artifact identities, setup/prefix provenance, policy versions, seeds, limits,
workers, search results, and replay verdicts. Keep copyrighted binaries out of
the entry. Add CI using the existing workload pattern where inputs permit it.

Report these separately:

- **Execution qualified:** setup, controls, observations, and restored branches work.
- **Search qualified:** bounded campaigns and recorded replay work, with a useful
  measured progress signal and all encoded guidance disclosed.
- **Completion demonstrated:** the declared ending has a verified witness from
  the declared origin. Independent stage wins do not establish one continuous run.

An unsolved game can be a valuable library entry. Missing terminal evidence or
an unimplemented backend stays explicit. Summarize checks actually run and
remaining limits; record follow-up work and implementation history through the
repository's issue/commit/PR workflow within the task's authorization.

For maintaining this skill, use [the evaluation cases](references/evaluation.md).
Do not load that reference during ordinary game integration.
