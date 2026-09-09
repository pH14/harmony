# Replay-safe milestone stopping qualification

This is engineering qualification for [#290](https://github.com/pH14/harmony/issues/290),
within the existing continuation-yield tranche. It is not a fresh performance
panel and does not extend the tranche or establish boss-level progress.

The workload names and versions an observation. The generic coordinator stops
new reservations after its first admitted occurrence and finishes the already
reserved window. If admission `e` first observes the event, its cost is bootstrap
work plus complete jobs 1 through `e`; at most `workers * window - 1` further
jobs drain. This measures observable first-event cost, not the pickup's exact
emulator frame. With a fixed seed, pre-event selection and retention are
unchanged. Resource measurements from such runs belong to an event-stopped
protocol, not a fixed-horizon throughput comparison.

## Frozen native checks

Before dispatch, publish a registration with exact source/build/runner hashes.
Use msr1 cores 8–11, four workers, an 8 GiB archive, a 12 GiB process envelope,
and the existing D02 Metroid candidate fixture (seed 2026090902). Preserve the
registered candidate, suffix, actions, terminal policy and observation features.
Each cell has at most 2,000 executions, 1M admitted frames, 240 seconds of search,
120 seconds finishing, a 390-second outer timeout and 4 GiB output. All cells
require full campaign report/checkpoint replay and twice-replayed witnesses.
Stop on the first failure. No retries or new seeds are implicit.

The existing fixture first observes Morph Ball at execution 148, with 22,586
admitted frames including bootstrap. These values were read before building the
new binary. Its full stream SHA-256 is
`0fcaad970e2a219874bdc7c92c523bff482b03e2e832da61d7ce9bc35caee13f`.

1. Default settings: reproduce the old 2,000-execution stream, report and
   checkpoint byte for byte.
2. Stop at Morph Ball with one physical result slot per worker: preserve the
   recorded prefix, report the expected first event and drain no more than seven
   jobs. Full replay must reproduce the report and checkpoint.
3. Repeat with two result slots: identical stream, report and checkpoint.
4. Stop at energy tank: the existing fixture does not reach it, so complete the
   registered execution budget with no event and unchanged stream body.
5. Stop at Morph Ball with frame cap 22,585: retain the observed event during
   drain, mark it outside budget, and report a frame-limit stop.
6. Stop at Brinstar: the observed genesis already satisfies the criterion;
   complete zero jobs and verify the origin and full replay.

`qualify_native.py` independently checks the frozen artifacts and accounts all
completed qualification, inferred campaign replay and twice-replayed witness
frames. Allocate at most 20M additional measured auxiliary frames and finish
qualification by 20:15 UTC on 2026-09-09. The prior ledger remains immutable;
search stays at 489,962,470 admitted frames. Setup, unadmitted execution and
physical reconstruction costs without counters remain unknown. The original
tranche's 50M auxiliary ceiling and 21:49:08 UTC deadline still apply.

Run the checker on msr1 with `--experiment` pointing at the experiment root,
`--registration` at the published registration and `--out` at a new JSON path.
Any qualification result must retain its raw panel and provenance beside this
document. Do not use this reused seed as performance evidence.
