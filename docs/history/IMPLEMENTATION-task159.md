<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# tasks/159 — W1: every grader must be able to fail (work order `hm-537`)

Review record for the PR from `task/negative-control-fixtures`. Closes `hm-7q0`, `hm-cte`,
`hm-6sj`, `hm-gmt`, `hm-9zy`, `hm-e1n`, `hm-5a6` (Mac half), `hm-pex` (Mac half). This document is
the PR body; lift it verbatim.

## The deliverable that outlives the ten fixes: the planted-failure harness

**A grader without a negative-control fixture proving it can go red is not a gate.** The discipline
is one; it has two incarnations, one per runtime, because the graders are in two runtimes:

- **Rust (`floor-check`)** — `schemas/floor-check/src/fixtures.rs`. A fixture is *the accept
  fixture with exactly one field mutated*, generated from the oracle model, byte-committed under
  `schemas/fixtures/`, and asserted by `tests/accept_reject.rs` to fail *which* check. A drift
  guard (`fixtures_match_committed`) and a schema-conformance guard keep the committed bytes honest.
- **Python / C (everything with a CLI)** — `spikes/negcontrol.py`, the shared harness, new here.
  It drives the ARM determinism comparators *and* the AMD floor checker *and* is ready for any
  future spike gate with a CLI.

### Arming a new gate is five lines

```python
good = [record(), record()]                                   # a fixture the grader accepts
write_records_json(tmp / "bad.json", mutate(good, 0, count=999))   # one field wrong
r = run_grader(GRADER, "subcmd", "--records", tmp / "bad.json")
self.assertNotEqual(r.returncode, 0)                          # the gate MUST go red
```

`write_run_set` builds a run-set *directory* (manifest + `records.jsonl`, sha256 pinned) for
graders that take a directory; `write_records_json` writes a bare JSON array for graders that take
a records file; `mutate`/`drop` apply the single change; `run_grader` returns the exit code, the
streams, and parsed JSON stdout. For the Rust checker, the equivalent is one `Fixture` entry + one
`assert_single_failure` line. Every negative control below runs in CI (ARM job
`spikes/arm-altra`, new AMD job `spikes/amd-epyc`, and the Rust `nextest` gate) and was observed to
go red on the planted failure.

## The ten fixes

Each child ships **its fix plus the fixture that would have caught it.**

### ARM `floor-check` (Rust) — `hm-gmt`, `hm-7q0`, `hm-9zy`

- **`hm-gmt`** — `check_counts`' `step.is_some() → continue` exempted *every* step record from
  count-exactness, so a non-AA-2 step run bypassed the payloads' semantic gate. The exemption is
  now **stage-scoped to AA-2**; a step record at any other stage is rejected. Negative control
  `reject-aa3-step-bypasses-counts` (the AA-2 matrix mislabelled AA-3) → count-exactness FAILs
  (it read PASS before). Sole FAIL (mechanism-attestation correctly exempts step Debug exits).
- **`hm-7q0(a)`** — `trips` is the payload's input constant, graded by count-exactness — but the
  step-`continue` skipped it, so a corrupt `trips` on a step record rode through ungraded. `trips`
  is now graded on **every** record, stepped or not. Negative control `reject-aa2-step-bad-trips`
  (a step record's `trips` corrupted to 0) → count-exactness sole FAIL (was all-green before).
- **`hm-7q0(b)`** — `floor-check --scope <check-id>…` rests the exit code on ONLY the named checks
  (out-of-scope FAILs reported, not gating; labelled `[SCOPED]`, never a full stage acceptance;
  fail-closed on an empty scope or a never-ran check). This makes
  `results/aa-6/live-20260720/MANIFEST.txt`'s prose `DEMONSTRATED` **machine-checkable** — the
  manifest now cites the exact `--scope rep-floor …` command. Verified exit-0 over the committed
  8000-record `live-20260721/aa6-minigate`. Negative control: pull a FAILing check (`aa6-matrix`)
  into the scope → the CLI exits non-zero (`the_scoped_cli_…` subprocess test); a unit test covers
  the `scoped_verdict` logic; `every_check_id_round_trips` keeps every check nameable.
- **`hm-9zy`** — `--exclude-payload` admitted any class and left no manifest trace. `RunSet` gains
  an additive `excluded_payloads` field (schema + harness), so the manifest **proves what ran**;
  `floor-check`'s new `aa3-payload-matrix` check enforces at AA-3 that only the ruled carve-outs
  (`wfi-idle`, `llsc-atomics`) may be excluded, that each name is real, and that the claim agrees
  with the records. It binds the recorded exclusion set — **not** full class presence — so honest
  subset run-sets stay green. Negative control `reject-aa3-excludes-nonruled` (excludes
  `straight-line`) → `aa3-payload-matrix` sole FAIL.

### ARM determinism comparators (Python) — `hm-cte`, `hm-6sj`

- **`hm-cte`** — `aa1c-determinism-check.py` read `state_digest` / `measured_taken` /
  `overflow.deliveries` with `.get()`, so records omitting all three on **both** lanes compared
  `None == None` and reported MATCH having compared nothing. All three are now **required and
  type-checked** per record (mirroring `aa3`, which was already immune). Negative control:
  symmetric omission → INVALID_INPUT, not MATCH.
- **`hm-6sj`** — neither comparator attested **lane provenance**. Both now require the solo
  reference and every co-tenant lane to be **distinct run-sets under distinct conditions**, so
  passing a directory against itself — the single most embarrassing false green in the backlog —
  or a copied/mislabelled lane is refused. Negative controls: `aa1c`/`aa3` self-comparison, and a
  copied-lane-same-condition case.

### AMD (`hm-5a6`, `hm-e1n`, `hm-pex`)

- **`hm-5a6`** (Mac half) — `check-floors.py`'s docstring claimed it recomputes every floor from
  raw records "never from a summary line the harness asserted", but `check_overflow` reads the
  harness's `overflow_summary` tally. The docstring now says so honestly (the exactness and AE-3
  halves *are* independent; the overflow half reads the summary, cross-checked against the anomaly
  records). **Box residual on `hm-5a6`:** retain per-arm overflow rows so the overflow half
  recomputes like the others.
- **`hm-e1n`** — `ae0-probe.c` returned 0 unconditionally, so it could not be an automated
  stage-stop. It now exits non-zero when a **load-bearing** capability is absent (SVM; the pinned
  `ex_ret_brn_tkn` event open/count/non-multiplexed; trivial-overflow delivery). The emitted JSON
  rows are unchanged; the rest of the truth table is reported, not gated.
- **`hm-pex`** (Mac half) — `singlestep-driver.c` reads `guest_tf_kept` *before*
  `KVM_SET_GUEST_DEBUG`, so `tf_kept=0` in mode `tf` is 0 by construction and establishes nothing
  about guest-transparency; `IMPLEMENTATION.md:23` cited it as "guest-transparent". Both are
  relabelled to what the code actually establishes (AE-2's GO rests on the sound
  `#DB`-count-vs-oracle exactness, **not** on transparency, and the relabel says so). **Box
  residual on `hm-pex`:** the genuine guest-`PUSHF`-observes-TF test, from inside the guest, after
  single-step is armed.

The AMD floor checker had **no test path** before; `spikes/amd-epyc/host/tests/test_check_floors.py`
adds nine negative controls (exactness count-mismatch / missing-rep, overflow lost-PMI, inexact and
stock-masquerade AE-3 landings, gate-RC propagation) on the shared harness, gated by the new AMD CI
job.

## Un-compiled C (foreman's syntax check)

`spikes/amd-epyc/harness/ae0-probe.c` and `singlestep-driver.c` are Linux/KVM C that does not build
on the Mac; the edits are small and kept obviously correct. Please run the syntax check at review.
Wiring the AMD C harness into a Linux CI runner is a separate bead (`hm-l82`), not done here.

## Gates

- Rust: `cargo fmt --check`, `cargo clippy --all-targets -D warnings` (native **and**
  `aarch64-unknown-linux-gnu` cross), `cargo nextest run` (297 pass), `cargo deny check` (from repo
  root with the root `deny.toml`: advisories/bans/licenses/sources ok) — for both the `arm-altra`
  workspace and `oracle-model`.
- Python: `python3 -m unittest discover` over `spikes/arm-altra/host/tests` (19) and
  `spikes/amd-epyc/host/tests` (9).
- Every negative control runs in CI and was observed to go red on its planted failure (outputs
  pasted in the commit messages).

No existing gate was weakened to make a fixture pass. No fix turned a currently-green gate red.
