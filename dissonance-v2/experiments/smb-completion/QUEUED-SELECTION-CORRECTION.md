# Queued registration — selection correction

This is a **draft, not in force**. It was written on the integrator's
instruction while the H54 arms were still running, and nothing in it has been
executed. It becomes a registration only when it is appended to `LAB-LOG.md`
after H54 concludes, including any held-out repeat. It is kept out of the
running record until then so that no entry appears in the log before the result
it follows.

## Motivating evidence, measured before drafting

The diagnosis was supplied by the integrator and independently checked here
against `phase4c` and against the recorded conquest archive
`target/smb-completion/c49-conquest-local/archive-live.json`.

- The frozen selector — the one every promoted campaign uses — sorts the active
  entries by `(milestone_key, archive key, id)` and then takes the **last 128**
  as its frontier window. `milestone_key` is the four frozen rungs: reached
  onward, reached the second level, reached the first level's end task, and the
  greatest first-level scroll bucket.
- That signal is dead at the current depth. **21,756 of 23,248 recorded entries
  hold the identical maximal `milestone_key`** `(true, true, true, 195)`. The
  primary sort key distinguishes nothing among them, so ordering falls through
  to the archive key, whose fields after progress are the vertical bucket, the
  engine-state byte and a six-bit fingerprint.
- The window that results is not a sample of the frontier. Its 128 entries are
  **all at vertical bucket 11** and all at engine state 8. Of the 1,276 entries
  at the deepest play bucket, **127 are in the window** — those that happen to
  sort last by vertical bucket and fingerprint. This is the same defect family
  as the bucket-15 artifact: a key field that was never meant to rank anything
  is deciding what the search expands, three draws in four.
- The remaining draw in four is uniform over all active entries, which at this
  depth is roughly twenty-three thousand states.
- Nothing records what a parent has produced. `ArchiveEntry` carries a report, a
  snapshot and its observations, and no counter of any kind. A parent that has
  been mutated thousands of times with no retained descendant is sampled exactly
  like one never tried — the energy idea that a coverage-guided fuzzer would
  supply and this custom cell archive never reimplemented.

## Mechanism

Three changes, and the registration states plainly that they are three rather
than pretending they are one, because they are not separable: correcting the key
without correcting the tie handling would concentrate every draw on a single
state, and correcting both without accounting would leave that state unable to
yield the frontier when it stops producing.

1. **Selection key.** The frozen `milestone_key` is removed from selection and
   replaced by the corrected `(world, level, progress)` tuple with M52's ladder
   semantics: lexicographic, so a later pair always outranks an earlier one and
   a larger progress inside an earlier pair never outranks a later pair. The
   four named rungs remain in every report and are not touched; they stop being
   a ranking signal, which is the only thing they had saturated at.
2. **Tie handling.** The 128-entry sorted slice is removed. The frontier becomes
   the best surviving **tie class**: all active entries at the maximal
   `(world, level)` whose progress lies within a fixed band of the deepest
   progress in that pair, sampled uniformly. Classes are considered in
   descending order and a class every member of which is exhausted is skipped,
   so the frontier falls through rather than dead-ending. The band is fixed at
   the recorded `FRONTIER_PROGRESS_BAND` of 8 and is not a new parameter.
3. **Per-entry accounting.** Each entry records the number of times it has been
   selected as a parent and the number of selections that produced at least one
   retained descendant. An entry is exhausted when its selections since its last
   retained descendant reach a fixed threshold of 64. A retained descendant
   resets the counter. If every active entry is exhausted, all counters reset to
   zero and selection proceeds; the search must not deadlock, and the reset is
   deterministic and recorded in the report.

The falling-through rule is what makes the first two safe together. On the
recorded conquest archive the maximal tuple `(1, 0, 144)` holds exactly **one**
entry, and it is the scripted level-completion sequence. Under the corrected key
and strict tie handling that single state would take three draws in four until
it exhausts, at which point its class is skipped and the frontier becomes the
band below it, which is the play frontier at 124 with 1,276 members. That
behaviour is intended, is stated here before execution, and is the reason the
accounting is part of this registration rather than a later one.

## Deliberately not changed

The one-in-four uniform draw over the whole archive is diagnosed as dilution and
is **left alone**. It is a fourth variable, the accounting already concentrates
the frontier path, and the uniform draw is the archive's only remaining source
of diversity if the frontier path is wrong. It is recorded here as the natural
follow-up rather than folded in, on the same one-variable-at-a-time discipline
that governed the terminal condition, the retention rule and the key term.

## Gates fixed before execution

- **G1, inertness.** With the frozen selector selected, a resumed arm reproduces
  a recorded arm byte for byte, so the correction is inert when it is not asked
  for. Every earlier recorded campaign continues to replay exactly.
- **G2, determinism.** One corrected arm replays byte-identically from its
  recorded seed with no model.
- **G3.** `cargo fmt --check`, `cargo clippy --all-features` with `-D warnings`,
  `cargo nextest run --all-features`, and `cargo deny check`.
- **G4, accounting honesty.** The selection and novelty counters, and any
  counter reset, are reported per campaign, so the claim that exhausted parents
  were starved is checkable from the record rather than asserted.

## Acceptance

Paired against the frozen selector, which is the discipline the terminal-condition
correction and H54 both used and the one whose absence made H51's rule
uninformative. Controls run the frozen selector and challengers the corrected
one, from the same source, on development seeds `0x5eed_e000..=0x5eed_e005` at
5,000 executions each. Acceptance requires the challenger's **play progress** to
be strictly greater than its paired control's on at least 4 of 6 seeds. Play
progress is M52's measurement and is primary; viable and recorded progress are
reported alongside for every arm. If it accepts, repeat unchanged on held-out
seeds `0x5eed_e100..=0x5eed_e105` with the same paired threshold, and any
promotion must replay exactly with no model.

The source is fixed by the same rule H54 used: the recorded archive with the
greatest play progress at the time of execution, ties by fewer retained entries
and then the smaller seed, resumed from the shortest input at its deepest play
bucket.

## Supersession lineage

- Supersedes the frontier-window behaviour of the parent scheduler frozen at H3
  and carried unchanged through every panel since. That scheduler is not
  withdrawn from the record; it remains the control in this panel and the
  executor of every result already recorded.
- Supersedes any reading of D27, D28 and the H21 through H27 panels that treats
  their frontier as a sample of the deepest states. It was a key-order slice,
  and at their depth the saturation had not yet occurred, so their recorded
  results stand as recorded while the inference about what was being expanded
  does not.
- Depends on M52 for its ordering semantics and on H45 for the retention rule
  that makes the tie classes live states rather than falls.
