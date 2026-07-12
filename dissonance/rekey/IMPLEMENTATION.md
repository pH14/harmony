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

## Round-1 review (PR #94)

Three blocking P1s in `score.rs`, all real, all fixed; the report was regenerated from the frozen
corpus (the re-key is free by design) and **the top-three menu did not change**.

1. **Axis (a) unioned cell keys across seeds**, which `docs/SCORING.md` R2 forbids outright — "per-seed
   codebooks are independent; cell keys are never compared across seeds". A template species id is
   minted in per-campaign first-seen order, so the same key bytes name different behaviour in two
   campaigns. Each campaign's archive is now keyed in its own namespace: `total_cells` sums
   per-campaign counts and `coverage` normalizes the per-campaign *mean* by `|K|`. The twin-control
   evidence moved with it — on the steered slice the trigger-*aligned* candidate now shows more
   cells, not fewer, and the blind twin's apparent breadth advantage there was an artifact of the
   cross-seed union. The conclusion is unchanged and sharper: on the *unsteered* ablation slice, the
   only slice free of the exploit's confound, the twins are indistinguishable on every axis.
2. **The menu's collapse fingerprint omitted a reported axis.** `v1-shipped` (0.310), `quant-identity`
   (0.233) and `lastnew-only` (0.931) were folded together as "identical on every axis" while their
   coverage differed. The fingerprint is now a **partition digest**: every arrival's cell, relabelled
   by first-claim order, hashed campaign by campaign. Equal digests mean the two candidates sorted
   every arrival into the same equivalence classes, so no *measured* axis can disagree. They may
   still differ in `|K|` — which counts the cells a config *could* key, not the ones it did — and the
   menu now says exactly that where it collapses them.
3. **Disqualified candidates could fill a menu slot.** `menu()` now skips any row failing axis (c)
   before filling or collapsing: a chain-breaking candidate is a gate failure, not a tie-break.

Three P2 hardening items, all fixed rather than rebutted: the inflater now refuses output past a
256 MiB cap (a gzip stream's CRC lives in its *trailer*, so it cannot bound the allocation its body
drives); `Corpus::load` rejects any manifest whose `version` is not `MANIFEST_VERSION`; and
`rekey manifest` without `--write` propagates a read error instead of passing vacuously.

## Foreman review, round 1 (PR #94)

Four blocking items, all fixed rather than rebutted.

1. **The `rekey` binary was mutation-gated with nothing exercising it** (`src/bin/rekey.rs`).
   `.cargo/mutants.toml` excludes `**/main.rs`, which does not match `src/bin/rekey.rs`, so the
   `cargo mutants --in-diff` job mutates `run`/`main` — and every mutant was *missed*, because every
   test called library functions and nothing drove the CLI dispatch. Fixed per the config's own bar
   ("exercised by a smoke test"), not with an exclusion glob: `tests/cli.rs` drives the built binary
   (via `CARGO_BIN_EXE_rekey`, no new dependency) through **every** `Command` branch and asserts a
   *distinct observable effect* for each — `score --stdout` vs `--out FILE` (so a `*stdout` flip is
   caught), `manifest` (prints + freshness-checks) vs `manifest --write` (writes, into a private
   corpus mirror so the committed tree is untouched), `verify` on the real corpus (success + stderr),
   and a missing corpus (non-zero exit, pinning `main`'s `Err → FAILURE` arm). The whole file is
   `cli`-gated so it compiles away when the binary is not built.
2. **Valid DEFLATE streams were rejected** (`src/gz.rs`). `hdist > 30` refused dynamic blocks
   declaring 31 or 32 distance code lengths, but RFC 1951 permits `HDIST+1 ∈ 1..=32`; symbols 30/31
   are merely *reserved when used*, which the decode-time `dsym >= 30` guard already handles. The
   count check is now `hlit > 286` alone — the HDIST clause is dropped rather than raised to a
   `hdist > 32` that a 5-bit field can never satisfy (which would leave a provably-equivalent
   surviving mutant, `hdist > 32 → false`, failing the mutation gate). The tests move with it:
   `every_legal_code_count_passes_the_header_check` pins that HDIST 31 and 32 — the values the old
   bound wrongly rejected — get past the check, and the too-many-codes test now attributes the
   rejection to HLIT alone.
3. **Unknown-slice exclusions silently skipped verification** (`src/manifest.rs`). Exclusions are
   hash-checked inside the per-slice loop via `.filter(|e| e.slice == slice.slice)`, so an exclusion
   naming a misspelled or stale slice was visited by no iteration — never hash-checked — yet `load`
   succeeded and the report called it verified, breaking the crate's core guarantee. `Corpus::load`
   now calls `validate_exclusions`, which requires every exclusion to name **exactly one** loaded
   slice (a factored, unit-tested free function; the test covers the zero-match and the duplicate-id
   cases, so both sides of `!= 1` are constrained).
4. **The axis-(c) branch-0 claim was asserted prose, not computed** (`src/report.rs`). "every one of
   them is branch 0" was a fixed sentence with only the counts interpolated. It is now derived:
   `score::ancestry_stats` measures, over the reconstructed ancestry, both how many finding-chain
   proper ancestors sit on branch 0 **and** how many of the search's exploit branches descend from a
   *non*-genesis parent. On the primary slice those are `4/4` and `1 524 of 7 660` respectively — so
   the report states the all-branch-0 property and immediately scopes it to the shallow first-finding
   chains, noting that the search at large *does* select non-genesis parents (a find enters the
   frontier; a later exploit step picks it). The corpus test pins both figures, so the claim can
   never drift back to prose.

One earlier hardening item also rides in this round: **manifest paths could escape the corpus root**
(`src/manifest.rs`). `root.join(rel)` is not a containment primitive — it discards the root for an
absolute path and walks `..` upwards, and content-addressing cannot save it (the manifest supplies
both the path *and* the sha256 it is checked against). Every archive, trace-log and reference-log
path now goes through `resolve()`: reject any non-`Normal` component before touching the filesystem,
then canonicalize and require containment beneath the canonicalized root — which also closes the
symlink case.

## Foreman review, round 2 (PR #94)

Round-1's four items verified fixed; the clean-pass rerun found three more in the fix-round code.

1. **[blocking] The alternate-target sensitivity ranking bypassed the chain gate** (`src/report.rs`).
   The report shows the ranking at a second target (`TARGET_SENSITIVITY`) to expose its dependence on
   `T`, but that `alt` sort ordered purely by `objective_alt_q32` — `chain_preserved()` reached only
   the verdict column. On a corpus where axis (c) is non-vacuous, a chain-breaking candidate could
   surface in the reported `T = 256` top three, the exact outcome the mandatory gate exists to
   prevent. Fixed by factoring `rank` into `rank_by(scores, objective)` — the chain-preservation gate
   and every tie-break are identical; only the objective differs — and having the alt ranking call it.
   The gate is not a property of the target. (On *this* corpus axis (c) is vacuous, so the report is
   byte-identical; the fix matters for any future corpus with real chain depth, and a unit test pins
   that `rank_by` disqualifies a chain-breaker even when handed the best `T = 256` curve.)
2. **[P2] Duplicate campaign entries would double-weight scoring** (`src/manifest.rs`). A repeated
   `(slice, config, seed)` `TraceEntry` loads twice and passes every hash and ancestry check while
   biasing every axis — the same harm as counting a `-solo` re-run. `Corpus::load` now calls
   `validate_trace_uniqueness` before scoring: per slice, a `(config, seed)` may appear at most once
   (the same `(config, seed)` in a *different* slice — e.g. seed 1 signal in both the campaign and the
   ablation — is legal). Unit-tested on both the collision and the cross-slice-is-fine cases.
3. **[P2] The hand-written gzip/DEFLATE/ustar parser had no property test.** `proptest` was declared
   but unused. Added a ≥256-case (512) malformed-input **totality** property for `gunzip` and `untar`:
   over arbitrary bytes each must return `Ok`/`Err` and never panic, read out of bounds, or loop
   unboundedly (proptest fails on a panic or a hang, so completing every case *is* the property). A
   third property feeds gunzip a valid gzip prefix (`1f 8b 08`) + arbitrary flag byte and body, so the
   optional-header skips, block-type dispatch, Huffman decoders, and trailer are reached where pure
   noise stops at byte 0. (`untar`'s size field is octal, capped at ≤ 2³⁶, and every read is a bounded
   `get` before `to_vec`, so there is no attacker-controlled allocation for the property to trip.)

## Foreman review, round 3 (PR #94)

The round-2 fixes were shallow; this round takes each finding to its root. (A recurring lesson: a
gate is only real if its **display/consumer** path enforces it, not just an intermediate ordering —
and a check on faith-trusted metadata must compare against the self-describing artifact, and a
declared count must be recomputed, or none of them actually constrain anything.)

1. **[blocking] The chain gate was still bypassed in the *displayed* top three** (`src/report.rs` +
   `src/score.rs`). Round 2 made `rank_by` order chain-breakers last, but the prose used `.take(3)`,
   which still surfaces a disqualified row whenever fewer than three candidates qualify — the
   non-vacuous case the gate exists for. Fixed with `score::top_eligible`, which `filter`s on
   `chain_preserved()` **before** `.take(n)`: fewer than `n` eligible ⇒ fewer shown, never a
   chain-breaker. The report now also states `{eligible} of {total}` preserve every chain and that
   any breaker is omitted — a factual clause interpolated from counts, not an inert `if` (which would
   be a report-invariant equivalent mutant on this vacuous corpus). Unit-tested with a fixture where
   only two of four rows qualify and the best-ranked one is a breaker.
2. **[P2] Dedup keyed on unverified identity** (`src/manifest.rs` + `src/observe.rs`). `observe`
   scores under `log.config` but keyed the seed and deduped on the *manifest* strings, so the same
   member relabelled with a different `config` would dedge as distinct and double-score. Now
   `observe::check_identity` cross-checks the manifest `(config, seed)` against the self-describing
   `CampaignLog` and errors (`IdentityMismatch`) on any disagreement — making the manifest label
   trustworthy, so the existing `(slice, config, seed)` dedup is now on *verified* identity. Both
   spoofed labels (caught by the cross-check) and exact duplicates (caught by the dedup) are refused.
   Factored and unit-tested (matching passes; wrong config and wrong seed each fail).
3. **[P2] Declared counts were echoed, not verified** (`src/manifest.rs`). `TraceEntry.branches` and
   `manifest.totals` were hash-adjacent but never recomputed, so a stale count could report a
   512-branch trace as a verified zero-branch corpus. `Corpus::load` now recomputes every trace's
   branch count and the four aggregate totals from the actual bytes and refuses any drift via
   `check_count` (a factored, unit-tested comparison — both directions of drift covered).
4. **[P2] The ustar totality property was vacuous** (`src/gz.rs`). Uniform random bytes hit ustar's
   40-bit magic with p≈2⁻⁴⁰, so all 512 cases returned at the magic check and the size/padding
   arithmetic was never reached. Added `untar_is_total_over_valid_headers_with_fuzzed_fields`: a
   *syntactically valid* ustar header (magic at offset 257) with a fuzzed octal size / typeflag /
   body, truncated at an arbitrary point — so `at + size`, `size.div_ceil(512) * 512`, and the bounded
   `get(at..at + size)` are actually exercised (a property that cannot reach the code certifies
   nothing). The arbitrary-bytes ustar property is kept but documented as only covering the early
   return.

## Foreman review, round 4 (PR #94)

The chain-gate P1 held; two final small P2s.

1. **[P2] The equal-objective tie-break used raw cell count** (`src/score.rs`). `docs/SCORING.md`
   3(a) normalizes breadth because raw QD counts scale with resolution, so a finer candidate could
   win an objective tie for minting more bins. The review offered two remedies — normalize the
   tie-break, or drop breadth from it — and **drop is the correct one here**: among equal-objective
   candidates, normalized `breadth_q32` differs *only* when partitions differ, and the equal-objective
   equal-partition candidates (v1's `fold_k`/`Quant` knob variants) differ solely in `|K|`, where
   normalizing would just reward the smaller key-space, i.e. *dropping a channel* — backwards for
   generalization, and the menu already collapses those on their identical partition. So the
   tie-break is now objective → declaration order, keeping the control the representative a knob
   variant can never displace. (Verified: normalized breadth would have floated `lastnew-only`, a
   channel-dropped variant, above the `v1-shipped` control and made it the menu's group
   representative — the exact regression `drop` avoids. Ranking unchanged from before; only the
   tie-break prose moved.)
2. **[P2] Slice `bug`/`explore_period` were not cross-checked against the member logs**
   (`src/observe.rs`). The identity cross-check covered `(config, seed)` only, so a manifest that
   mislabels a slice's bug (3 → 1) or explore_period passed every gate while the report copied the
   wrong identity from the slice. `observe::check_slice_membership` now requires each member log's
   `bug` and `explore_period` to match its slice, erroring (`SliceMismatch`) otherwise —
   the difference, for an evidence crate, between a *verified* corpus and a merely *labelled* one.
   Factored and unit-tested (truthful passes; wrong bug and wrong explore_period each fail).

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
  section.** The primary slice's 29 finding chains hold 4 proper ancestors *in total*, and all 4 are
  branch 0 — a direct consequence of the NO-GO's diagnosis (a first-finding chain is at most
  genesis → find). This is scoped, and measured (`ancestry_stats`): it is a fact about the shallow
  first-finding chains, **not** about the search's ancestry at large, where 1 524 of the slice's
  7 660 exploit branches descend from a non-genesis parent. Branch 0 claims a cell under every
  candidate, so even the one-cell `no-channels` floor "preserves" every chain. The playbook's one
  bug-based axis therefore crowns nothing and kills nothing here. It is still computed and reported (it is mandatory, and it *does* fail candidates
  on a corpus with real chain depth — `score.rs`'s unit tests exercise exactly that). The
  consequence is that the ranking rests on the two curve axes law 6 disqualifies as sole evidence,
  which is why the deliverable is a menu and not a winner.
- **The ranking is a function of the stated target `T`, not of the corpus.** At `T = 64` the order
  is `draw-top-64 → v1-shipped → foldk-16`; at `T = 256` it is
  `draw-top-256 → draw-top-only-256 → draw-low-256`. Go-Explore's penalty `√(|n/T−1|+1)` is
  asymmetric (undershoot costs at most `√2`, overshoot is unbounded). Both targets are reported.
  `T` is a human judgment; `TARGET_CELLS`/`TARGET_SENSITIVITY` in `score.rs` name it.
- **Normalized breadth saturates.** `mean cells / |K|` is QD coverage, so the coarsest candidate
  scores a perfect `1.000000` on its own trivial grid (`no-channels` covers its one cell). Raw and
  normalized are both printed; neither is a ranking key on its own.
- **`|K|` is analytic, coverage is not a partition property.** Two candidates can induce an identical
  cell partition and still report different coverage, because `|K|` counts the cells a config *could*
  key. That is why the menu collapses on the partition digest and names the `|K|` difference rather
  than hiding it.
- **`cell_id_of` mirrors a private `conductor` function.** The FNV-1a fold is duplicated (conductor
  is outside this task's surface and pulls the whole live plane). Drift would be loud, not silent:
  the control gate compares this function's output against the committed campaign logs on all 60
  campaigns.
- **Skipped branches are unsupported.** A branch the backend rejected as inadmissible consumed PRNG
  draws but recorded no environment, so its selection stream cannot be reconstructed. Bug 3 has
  none (`RareEntropy` mints no fault). `observe_campaign` refuses such a corpus loudly rather than
  reconstructing fiction.

## For the integrator

- **Gates.** `build` / `nextest` (92 tests, incl. ≥256-case gunzip/untar totality proptests) /
  `clippy -D warnings` / `fmt` / `deny` / `cargo mutants --in-diff` all green on macOS, plus
  `cargo check --target x86_64-unknown-linux-gnu --all-targets` (the crate has **no `unsafe`**, no
  `cfg(target_os)` fork, and no platform API — so no Miri job entry is needed and the
  `ci-cfg-linux-review-gap` failure mode does not apply). The `rekey` binary is inside the mutation
  gate (the `**/main.rs` exclusion does not match `src/bin/rekey.rs`) and is killed by
  `tests/cli.rs`, per the config's smoke-test bar.
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
