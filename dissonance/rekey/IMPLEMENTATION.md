# IMPLEMENTATION — task 97, the E-fails re-key harness

`dissonance/rekey/` implements `docs/SCORING.md`'s E-fails playbook, steps 2–4, over the frozen
GO/NO-GO #2 trace corpus. Deliverables: the corpus manifest
(`dissonance/benchmark/campaign-data/rekey-corpus.json`) and the ranked ratification menu
(`dissonance/benchmark/REKEY-REPORT.md`). The harness promotes nothing; Paul ratifies (bead
`hm-5h7`).

**Surface.** A new crate, not a `benchmark` module — the spec's ~500-line threshold was passed
several times over (the in-crate gzip reader alone is ~330). Read-only deps: `explorer`,
`logtmpl`, `benchmark`. No sibling crate was modified.

## What the report found

Two results, both stronger than the spec anticipated, both gated by tests:

1. **v1's fourth cell *is* the crash.** In every one of the 29 finding bug-3 campaigns, the fourth
   template species debuts *exactly on the finding branch* — it is the kernel's `traps: … general
   protection fault` line, which rides behind the `UUID_BUG` marker the campaign does filter. In
   every one of the 11 non-finders, the archive freezes at 3 cells from branch 0 for all 512
   branches. So the shipped cell function discovers **zero** cells while the search is still
   searching, and `CORRELATION-REPORT.md`'s ρ = −0.671 is a restatement of "did this campaign find
   the bug before branch 256?" This does not disturb the NO-GO (the signal lost on find rate
   regardless); it explains it. Escalated as bead `hm-mcx`.
2. **The entire R2 knob space is inert on this corpus** — a proof, not a sampling result. With a
   three-species pre-crash vocabulary, every `fold_k` in the sweep exceeds the largest species id
   (so every fold is the identity) and `Quant::Identity` separates only counts `Log2` already
   separates. `foldk-{16,32,128,256}`, `quant-identity` and `lastnew-only` score *identically* to
   the control on every axis. Only a new channel moves anything.

## Deviations considered

- **Bug 1 as the degenerate control (spec §corpus) — impossible, and it is not a scoping
  judgment.** Bug 1's campaign predates the `--record` retention amendment, so no `RunTrace`s
  exist for it; `docs/SCORING.md` R1 makes retained traces the substrate, so it cannot be re-keyed
  at all. `CORRELATION-REPORT.md` flags this as a "Known gap". It appears as a recorded-log
  reference row (2 cells/campaign, 20/20 finds), and the retention discipline is bead `hm-5sv`.
  **Rejected:** re-running bug 1 with retention (box work, fenced by the spec).
- **The replacement noise control (foreman-approved, spec amended on main).** Bug 3 fires exactly
  when `draw >> 56 == 0xA5`, and the guest prints that draw. So `draw-top-256` is a *maximally
  trigger-aligned* chosen state channel and `draw-low-256` — the same draw's low byte, 256 values,
  identical arrival pattern, read by no trigger in the benchmark — is its statistically identical,
  trigger-blind twin. It is a sharper control than bug 1 would have been: it isolates
  trigger-alignment as the *only* difference between two otherwise-identical descriptors, and no
  offline axis separates them. Law 6 (Böhme–Szekeres–Metzman, ICSE 2022) reproduced on our own
  corpus.
- **Chain reconstruction by PRNG replay rather than by env-distance matching.** The campaign logs
  record each find's `path_len`/`novel_on_path` but not its parent branches, and axis (c) needs
  ancestry. `replay::reconstruct` re-derives the campaign's selection stream (draw counts are fixed
  by the config; frontier admission comes from the recorded `CampaignLog`). It is **checked, not
  trusted**: every one of the 10 240 reconstructed branch seeds must equal the seed its recorded
  environment carries, and every reconstructed chain must reproduce the recorded `FindRecord`. It
  does. **Rejected:** inferring parentage from seed Hamming distance (ambiguous, unverifiable).
  Scoped to bug 3's fault-less exploit kernel; any other bug is refused rather than guessed at.
- **Hand-rolled gzip/DEFLATE/ustar rather than `flate2` + `tar`.** The traces are committed as
  `.tar.gz` and neither crate is on the rule-5 whitelist. `src/gz.rs` decodes in-process (~330
  lines, RFC 1951/1952 + ustar). Not trusted on faith: every extracted member is checked against
  the manifest's sha256, plus the gzip CRC-32 and length, so a decoder bug fails a hash rather than
  producing plausible bytes. **Rejected:** shelling out to `tar` (a hidden binary dependency in the
  gates, and scratch writes during tests) and spending the justification bar on two dependencies.
- **Fixed-point, not `f64`.** Axis (b) needs `ln` and `√`. `f64::ln` is a libm call whose last bits
  are not guaranteed identical across platforms, so a macOS-rendered report could differ from a
  Linux-rendered one in its final digit — and the determinism gate demands byte-identity.
  `src/fixed.rs` is Q32.32 with `u128` intermediates: `log2` by repeated squaring, `ln` by a pinned
  `ln 2`, `√` by `u128::isqrt`. Rendering rounds half-up by integer division; no float ever exists.

## Known limitations

- **Axis (c) has no discriminating power on this corpus, and the report says so in its own
  section.** The primary slice's 29 finding chains hold 4 proper ancestors *in total*, and every
  one is branch 0 — a direct consequence of the NO-GO's diagnosis (the frontier never exceeds two
  entries). Branch 0 claims a cell under every candidate, so even the one-cell `no-channels` floor
  "preserves" every chain. The playbook's one bug-based axis therefore crowns nothing and kills
  nothing here. It is still computed and reported (it is mandatory, and it *does* fail candidates
  on a corpus with real chain depth — `score.rs`'s unit tests exercise exactly that). The
  consequence is that the ranking rests on the two curve axes law 6 disqualifies as sole evidence,
  which is why the deliverable is a menu and not a winner.
- **The ranking is a function of the stated target `T`, not of the corpus.** At `T = 64` the order
  is `draw-top-64 → v1-shipped → foldk-16`; at `T = 256` it is
  `draw-top-256 → draw-top-only-256 → draw-low-256`. Go-Explore's penalty `√(|n/T−1|+1)` is
  asymmetric (undershoot costs at most `√2`, overshoot is unbounded). Both targets are reported.
  `T` is a human judgment; `TARGET_CELLS`/`TARGET_SENSITIVITY` in `score.rs` name it.
- **Normalized breadth saturates.** `pooled / |K|` is QD coverage, so the coarsest candidates score
  a perfect `1.000000` on their own trivial grids (`no-channels` covers its one cell). Raw and
  normalized are both printed; neither is a ranking key on its own.
- **`cell_id_of` mirrors a private `conductor` function.** The FNV-1a fold is duplicated (conductor
  is outside this task's surface and pulls the whole live plane). Drift would be loud, not silent:
  the control gate compares this function's output against the committed campaign logs on all 60
  campaigns.
- **Skipped branches are unsupported.** A branch the backend rejected as inadmissible consumed PRNG
  draws but recorded no environment, so its selection stream cannot be reconstructed. Bug 3 has
  none (`RareEntropy` mints no fault). `observe_campaign` refuses such a corpus loudly rather than
  reconstructing fiction.

## For the integrator

- **Gates.** `build` / `nextest` (56 tests) / `clippy -D warnings` / `fmt` / `deny` all green on
  macOS, plus `cargo check --target x86_64-unknown-linux-gnu --all-targets` (the crate has **no
  `unsafe`**, no `cfg(target_os)` fork, and no platform API — so no Miri job entry is needed and
  the `ci-cfg-linux-review-gap` failure mode does not apply).
- **The committed artifacts are gated.** `tests/corpus.rs` fails if `rekey-corpus.json` or
  `REKEY-REPORT.md` is stale. Regenerate with `cargo run -p rekey -- manifest --write` then
  `cargo run -p rekey -- score`. `cargo run -p rekey -- verify` runs the corpus and
  harness-correctness gates alone (~1s release).
- **Determinism.** Two `score` runs in separate processes produce byte-identical
  `REKEY-REPORT.md`; there is no generated-date line, and a test asserts the report embeds no date.
- **The corpus is loaded only through the manifest**, every archive/member/log re-hashed on load,
  and a mismatch is an `Error::HashMismatch` — never a warning (the `hm-xdp` lesson). The five
  excluded `-solo` re-runs are pinned by hash too, so an exclusion names a *known* artifact rather
  than an absent one.
- **Beads filed:** `hm-5h7` (PAUL: ratify or decline — this task is done when the menu is in his
  hands), `hm-5rt` (the bounded box confirmation, blocked on `hm-5h7` **and** task-95 M2's
  `hm-lld`), `hm-mcx` (the marker-filter hole), `hm-5sv` (trace-retention discipline). `hm-b3h` is
  this task.
- **Not touched:** the search/Selector, `explore_period`, `logtmpl`, `runtrace`, `explorer`,
  anything box, and the NO-GO ruling.
