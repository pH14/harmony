# Concentration correction lab log

## 2026-08-15 — setup

- Authority is the H59 registration and its builder requirements at the end of
  the SMB completion lab log, recorded at this branch's base commit `af287230`.
  This experiment implements the mechanism and its gates only; the registered
  pilot and any fleet return to the search experiment after the merge.
- The registration exposes a fork — recency-displacement against
  exhaustion-eviction — and requires the builder specification to say which is
  being built. The dispatch settles it: **recency-displacement**. Within the
  winning tie class, sample uniformly from only its `K = 128` most recently
  retained members, retention order taken as entry creation order, membership
  recomputed per draw, a member leaving when 128 newer class members exist.
  Band, fall-through, and the sixty-four-selection barren threshold are
  unchanged.
- Integrator requirements beyond the H59 text, received as instructions: the
  new policy threads through both execution modes and is recorded in every
  report and campaign stream header exactly as the corrected policy is;
  inertness evidence covers both existing selector paths against published
  recorded artifacts; determinism of the new path is shown in both modes;
  construction is additive with minimal edits to shared files.
- Worktree `/Users/phemberger/workspace/harmony-concentration`, branch
  `exec/concentration-correction`, cut from `af287230`. Nothing here touches
  any other worktree.
- Inputs verified before use. The external ROM's SHA-256 is
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`, matching
  the recorded M4 value. The published conquest source archive at
  `steers/c49-world2-origin/archive-live.json` matches its manifest at
  `b57e72e42c2c87942b02947ba43014d18c88ee70fdadeebb63d7d0251f9f1273`. The
  published inertness references at `steers/h56-inertness-references/` all
  verify against `MANIFEST-SHA256.txt`: the recorded M53 gate report at
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360` and the
  recorded gate-2 campaign stream, live archive and live report at
  `689ef0ee…`, `5613ab35…` and `03210261…`.
- Read before design: the H59 registration with its assumption checks; the H56
  registration, builder requirements and result; the H58 result whose dilution
  arithmetic this mechanism answers; the corrected selector
  (`Archive::select_parent`, `choose_parent_corrected`,
  `best_unexhausted_class`, the accounting types in `fuzzer/src/phase4c.rs`);
  the campaign coordinator and replay (`fuzzer/src/campaign.rs`); the
  selection-correction builder's lab log for the additive-policy pattern this
  build repeats.

## CC0 — design, registered before code

### The policy, in the established additive pattern

- A new enum value `SmbArchiveSelectorPolicy::ConcentratedRecency` beside
  `Frozen` and `CorrectedTieClass`, default unchanged at `Frozen`, threaded
  through the serial engine and the campaign configuration exactly as the
  corrected value is. No existing entry point changes its policy.
- Header identifier `concentrated_recency_128`, carrying the window size the
  way `frozen_frontier_128` does, used in the campaign stream header's
  `parent_scheduler` field, the campaign report, and the experiment-mode
  summary. Campaign replay derives the policy from the recorded header, so a
  recorded stream can never replay under the wrong selector.
- The frozen selector and the corrected algorithm are not edited. The
  concentrated policy reuses the corrected path — same one-in-four uniform
  draw in the same random-stream position, same corrected key, same descending
  pair order and progress bands of width 8, same fall-through with skip
  counting, same deterministic all-exhausted reset, same barren threshold of
  64 — and differs in exactly one place: the final uniform draw within the
  winning tie class.

### The mechanism, recency-displacement as dispatched

- Entry ids are creation order: ids are assigned at insertion and entries are
  never renumbered, so "most recently retained" is "greatest entry id",
  deterministic and free of key-order accident, exactly as the registration
  requires.
- At a concentrated tie-class draw, the sampled set is the
  `CONCENTRATION_WINDOW = 128` greatest-id members of the winning tie class;
  the draw is uniform over that window. Membership is recomputed at every
  draw; a member leaves when 128 newer class members exist. When the class
  holds 128 or fewer members the window is the whole class and the draw is
  byte-for-byte the corrected draw in behaviour (the random stream still
  differs only in the draw's modulus).
- One reading is settled here and flagged for the integrator's review rather
  than paused on, because the registration's own constraints force it: the
  winning tie class in the corrected selector is by construction its
  **unexhausted** members — H56's requirement that an exhausted parent "is not
  sampled while any unexhausted member of its class remains" — and H59 keeps
  barren accounting and fall-through unchanged. The window is therefore taken
  over the class's unexhausted members: an exhausted member leaves the sampled
  set immediately, and the window refills from the next-most-recent
  unexhausted member below it. The alternative — a window over all class
  members intersected with the unexhausted — leaves the draw undefined when
  every window member is exhausted while the class is not, and any resolution
  of that case would silently change either fall-through or the starvation
  invariant. In every recorded regime the two readings coincide, because no
  parent has ever reached the barren threshold in a real arm; a unit test pins
  the chosen behaviour.

### Accounting, shaped so the assumption-check figure is in the record

- The H59 assumption check turned on draws per parent, so the record reports
  it directly rather than leaving it to hand arithmetic.
- Per concentrated tie-class draw, the selector annotation gains a
  concentration record: the sampled-set size at that draw, and how many of the
  set's members had never been members before. Uniform draws carry none, and
  the corrected and frozen policies never carry one, so every recorded stream
  keeps its bytes.
- Per campaign, the selector accounting gains a concentration block, present
  only under the concentrated policy: the fixed window cap (128), the
  sampled-set size at the most recent concentrated draw, the number of
  concentrated tie-class draws, the number of distinct parents that ever
  passed through the sampled set, and the draws-per-parent figure those two
  produce, recorded in thousandths (`window_draws * 1000 /
  distinct_window_parents`, floor) so the report stays in exact integers.
  H58's measured 2.6 draws per parent reads back as 2600.
- "Passed through the sampled set" means was a member at some concentrated
  draw, which is the denominator the H59 arithmetic used — not merely was
  sampled. Membership entry is counted at draw time on the coordinator, rides
  the stream in the per-draw record, and is accumulated at the same two
  stream-write points the existing counters use, so campaign replay reproduces
  the accounting from the stream without recomputing selection-time state,
  exactly as the corrected policy's counters already do. The serial engine
  accumulates at selection time and re-runs identically by construction.
- The existing skipped-class and counter-reset counts are unchanged and
  continue to be reported; the existing per-entry selected and productive
  counters appear under the concentrated policy exactly as under the
  corrected one.
- Replay verification tightens correspondingly: a corrected stream carrying a
  concentration record is rejected, as is a concentrated tie-class draw
  missing one or a concentrated uniform draw carrying one.

### Command-line surfaces

- `smb-completion` gains one mode,
  `archive-resume-play-viable-ladder-concentrated`, identical to the
  corrected acceptance-arm mode in every argument and policy except the
  selector. Every existing mode keeps its exact current call path.
- `smb-campaign run --selector concentrated_recency_128` works through the
  existing identifier parser; `replay` needs nothing, the header decides.

### Gate plan, from the registration and the integrator requirements

- **G1, inertness, both existing paths.** Frozen, experiment mode: rerun the
  recorded M53 gate — `archive-resume-frontier-viable-ladder` on the verified
  conquest archive, seed `0x5eed_ef00`, 256 executions, 512-action bound —
  and the report's SHA-256 must equal the recorded
  `88f8ace7a5322813eea000a0b08792b41107a06048ca8ae8d58f35eca0ad6360`.
  Corrected, experiment mode: rerun the H56 corrected arm —
  `archive-resume-play-viable-ladder-corrected` on the same source at play
  bucket 124, seed `0x5eed_e000`, 5,000 executions, 512-action bound — and
  the report must equal the recorded
  `97cfb6700fcb0e0b717adf7818e974bbe547349133874d00a16fcf14ab273a47`.
  Frozen, campaign mode: replay the recorded gate-2 stream from genesis and
  reproduce the recorded archive and report hashes `5613ab35…` and
  `03210261…` byte for byte.
- **G2, determinism of the new path, both modes.** One concentrated
  experiment-mode arm from the conquest archive at play bucket 124 runs twice
  from one seed with no model and must be byte-identical. One small
  concentrated live campaign on the real ROM must replay byte-identically
  from its recorded stream.
- **G3, quality.** `cargo fmt --check`, `cargo clippy --all-features` with
  `-D warnings`, `cargo nextest run --all-features`, `cargo deny check`.
- **G4, accounting honesty.** Every concentrated report carries the
  sampled-set size, the distinct parents that passed through it, the
  draws-per-parent figure, and the existing skipped-class and counter-reset
  counts; the G2 arms' figures are read back and stated here with numbers.
- Evidence destination: `target/perf-evidence/concentration-correction/`.

### Milestones

- CC1 — the mechanism in `phase4c.rs` and the campaign threading in
  `campaign.rs`, with unit tests, plus the one command-line mode.
- CC2 — gate evidence: G1 on both existing paths, G2 both modes, G4 readback,
  G3 throughout; recorded here with numbers.
