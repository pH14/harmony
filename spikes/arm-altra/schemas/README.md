# `schemas/` — canonical evidence formats + the floor-checker

> **UNTESTED ON SILICON.** Nothing here has judged evidence produced by real
> hardware. The schemas are the *shape* stages AA-0..AA-6 will fill; the fixtures
> are synthesised from the oracle model, not measured. This directory is offline
> apparatus built so that Altra arrival day is `scp + run`, not scaffolding
> (`docs/ARM-ALTRA.md` §Execution constraints, task 109 deliverable 3).

This is the part of the ARM spike that makes `docs/ARM-ALTRA.md` §Evidence integrity
**mechanical**. That section was written after the nested-x86 review (2026-07-12,
PR #98) found harnesses that reported green on failed gates, dispositions whose
acceptance floors the retained evidence did not meet, and an existential-stage
harness that silently exercised the *stock* fallback while claiming the *patched*
mechanism. The floor-checker exists so none of those can recur on ARM: it recomputes
every acceptance floor **from the retained per-sample records** and never believes a
summary — the manifest deliberately carries no result totals to believe.

**The checker's verdict — not any harness's done-marker — is what a stage
disposition may rest on.** A run that "reached the end" has proved nothing; a run
whose records the checker accepts, under an explicit floor, has.

## What is here

| File | What it is |
| --- | --- |
| `run-set.schema.json` | The `run-set.json` **manifest**: environment, mechanism attestation, image pins, perf config, pinning, the measured weights + skid margin, and the *attempted* sample count. Mirrors `arm_harness::evidence::RunSet` field-for-field. Carries **no** result totals by design. |
| `run-record.schema.json` | One line of `records.jsonl`: one attempted sample. Mirrors `arm_harness::evidence::RunRecord`. Every attempted sample gets a record — a missing sample is a failure to account, not a pass. |
| `truth-table.schema.json` | The AA-0 capability truth table: MIDR/SoC/core count, the standing core-assignment topology, and **thirteen mandatory** expect-vs-found rows (ECV **expect absent**, LSE **expect present**, PMUVer, SVE **expect absent**, BR_RETIRED-in-`PMCEID0`, **`perf_event_open` of raw 0x21 pinned SUCCEEDS**, **a host-side overflow test DELIVERS**, `/dev/kvm`, VHE-vs-nVHE, `KVM_CAP_SET_GUEST_DEBUG`, vGICv3 creatable, **the writable-ID-register surface**). The two work-clock rows are existential: AA-1 — the whole point of AA-0 — rests on them, so a schema-valid table cannot leave them unmeasured. Deviations must carry an explicit recorded disposition, including a *favourable* one. |
| `floor-check/` | The Rust crate: the `floor-check` binary (the checker), the `gen-fixtures` binary (regenerates the fixtures from the model), and the accept/reject integration tests. |
| `fixtures/` | Eighteen checked-in run-sets — one the checker must accept, seventeen it must reject (one per failure mode). Generated from the oracle model by `gen-fixtures`; committed as files because they are the evidence the tests read. |

The schemas are JSON Schema **draft 2020-12**, hand-written, no codegen. They match
`spikes/arm-altra/harness/src/evidence.rs` field-for-field: every generated fixture
validates against them under `additionalProperties: false`, so a drift between the
schema and the Rust struct would fail validation.

### The two-file split, and why the manifest has no totals

A run-set is two files in one directory:

- **`run-set.json`** — the manifest. States what was *attempted* and under what
  conditions.
- **`records.jsonl`** — one `RunRecord` per line, one line per attempted sample.
  States what *happened*.

A checker that trusted the manifest's own summary of the records would be checking
the harness's opinion of itself — the exact pathology §Evidence integrity #2 forbids
("recomputed from the raw per-sample data, not read from a summary line the harness
itself asserted"). So the manifest carries **no** `mismatches: 0` field to believe.
The only numbers the checker uses are the ones it derives from `records.jsonl`, whose
sha256 the manifest pins so a swapped or truncated record file is caught.

## Invoking the checker on arrival day

Build it (Mac-local; the checker is pure logic, no box, no `unsafe`):

```sh
cd spikes/arm-altra
cargo build -p floor-check
```

Check a retained run-set. The real acceptance floors are passed **explicitly**, so
the number a disposition rests on is visible in the command that produced the verdict
— never buried as a default:

```sh
# AA-1 / AA-3: at least 10^6 armed overflows, every floor recomputed from the records.
cargo run -p floor-check --bin floor-check -- \
    results/aa-1/<run-set-id> --min-armed-overflows 1000000

# AA-6 mini-gate: at least 1,000 same-seed repetitions.
cargo run -p floor-check --bin floor-check -- \
    results/aa-6/<run-set-id> --min-reps 1000
```

Exit status is **0 only if every check passed**; a load failure (unreadable or
malformed evidence) exits `2`. The per-check `PASS`/`FAIL` summary prints to stdout;
the detail behind every failure prints to stderr. The output is **deterministic** —
no timestamps, no wall-clock, no map-iteration order — because the checker's output
is itself retained evidence (§Evidence integrity #2), so `> verdict.txt` captures a
stable summary you can pin and diff:

```
floor-check <run-set-id> stage=aa3
  [PASS] schema-version
  [PASS] records-sha256
  ...
  [FAIL] mechanism-attestation
RESULT: FAIL (1 of 13 checks failed: mechanism-attestation)
```

The floor a disposition rests on must be re-passed at check time; the checker will
not invent it. Two values are stage deliverables the manifest must carry and the
checker **refuses to default**: the measured `weights` (count offsets) and the
measured `skid_margin`. Handed `null` for either, the checker fails the run rather
than substituting a guess — task 109's "no invented constants" rule made mechanical.

## The failure modes it catches

Every check maps to a §Evidence-integrity countermeasure. Each has a reject fixture
under `fixtures/` that the integration tests assert is caught by *that specific*
check (not merely that something failed):

| Fixture | The check that catches it | Countermeasure |
| --- | --- | --- |
| `reject-short-count` | `armed-overflow-floor` | #2 machine-checked floors — below `--min-armed-overflows` |
| `reject-missing-sample` | `totality` | #6 a gap in `sample_id 0..attempted` is a failure to account |
| `reject-duplicate-overflow` | `multiplicity` | #6 an armed overflow with `deliveries: 2` |
| `reject-lost-pmi` | `multiplicity` | #6 an armed overflow with `deliveries: 0` (lost PMI) |
| `reject-count-mismatch` | `count-exactness` | #5 `measured_taken` disagrees with the oracle |
| `reject-overshoot` | `skid` | AA-3 late-only-stop — `skid > 0` is an immediate fail |
| `reject-skid-exceeds-margin` | `skid` | AA-1 — `\|skid\| > skid_margin` |
| `reject-stock-mechanism` | `mechanism-attestation` | #4 the PR-98 failure — records carry `SignalKick` under a `Preempt` claim |
| `reject-unverified-image` | `image-pins` | #3 an `ImagePin` with `verified_before_boot: false` |
| `reject-no-weights` | `weights-present` | no-invented-constants — `weights: null`, so counts are *refused*, not defaulted |
| `reject-tampered-records` | `records-sha256` | the `records.jsonl` sha256 does not match the manifest |
| `reject-self-seeded-params` | `params-mode` | in-band attestation — a record ran `self-seeded`, not `managed` |
| `reject-aa3-claims-stock` | `mechanism-attestation` | #4, the *self-consistent* form: an AA-3 run-set declaring `kvm_patched: false` + `signal-kick`, with matching records. Everything agrees; what they agree on is AA-3's forbidden fallback. The **stage tuple** — not the internal consistency — is what refuses it |
| `reject-migration-probe-outside-aa1` | `pinning` | one manifest field exempting an *unpinned AA-3 landing run* from a correctness condition. The bounded migration probe is AA-1's alone (rr #3607) |
| `reject-perf-attrs` | `perf-config` | the recorded event is not the work clock: wrong raw event, host-inclusive, guest-EXCLUDING, unpinned (so multiplexed, so scaled) |
| `reject-clockpage-self-seeded` | `clockpage-mode` | an AA-5 run whose guests published their own static clock page — the fallback, not the harness-maintained work-derived page AA-5 certifies |
| `reject-divergent-digests` | `replay-identity` | two repetitions of the same input that landed on **different** `state_digest`s. Every count matches and any rep floor is met — because a rep floor counts rows. This is the axis it exists for |

### A floor nobody asked for is not a floor that passed

The checker's output *is* retained evidence (§Evidence integrity #2), so a verdict
that is **silent** about a floor the evidence needs cannot be read as accepting one.
A third status exists for exactly that: `NOT-REQUESTED`. Check an overflow-bearing
run-set without `--min-armed-overflows`, or an AA-6 run-set without `--min-reps`, and
the verdict says `RESULT: INCOMPLETE`, names the floor, and **exits nonzero**. The
no-invented-numbers philosophy is intact: the checker demands the *presence* of an
explicit floor, and never supplies its value.

Beyond the fixtured modes, the checker also recomputes each record's
`measured_taken` from `work_end - work_begin` and fails a record whose own field
disagrees; refuses a `skid` it cannot bound (missing `skid_margin`); enforces
`payload_status == 0`; validates that the manifest's `perf` block describes the work
clock (raw `0x21`, `exclude_host`, `!exclude_guest`, `pinned`, and a `sample_period`
consistent with whether the records armed anything); requires every record to carry a
non-empty `state_digest` (an empty one compares equal to every other empty one, which
would make the determinism floors vacuous); and demands the vCPU was pinned unless the
run is AA-1's one sanctioned migration probe. `count-exactness` uses
`oracle_model::expected(payload, scale, seed).total(&weights, reported_taken)` — the
**analytical** oracle (§Evidence integrity #5), never PMU-vs-PMU, which would be
circular.

The AA-3 accept fixture is the canonical shape of a good landing-run's evidence in
miniature: patched `Preempt` mechanism with the marker observed, eight armed
overflows delivered exactly once, each landing exact (`skid == 0`), counts exactly
what the oracle predicts, all boot artifacts verified, managed params, pinned.

## Regenerating the fixtures

The fixtures are **generated from the oracle model**, not hand written, so they stay
consistent with the model as it evolves. A reject fixture is the accept fixture with
exactly one field mutated, which is what lets a test assert *which* check catches it.

```sh
cargo run -p floor-check --bin gen-fixtures      # rewrites fixtures/ from the model
```

The `fixtures_match_committed` integration test regenerates them in memory and
asserts the committed files still match byte-for-byte; a model change that would
silently invalidate a fixture fails the build instead of rotting the evidence.

## Gates

From `spikes/arm-altra/`:

```sh
cargo build  -p floor-check
cargo test   -p floor-check            # or: cargo nextest run -p floor-check
cargo clippy -p floor-check --all-targets -- -D warnings
cargo fmt    -p floor-check -- --check
```

The crate has **no `unsafe`**, no syscalls, and no box dependency; it runs entirely
Mac-local. It depends on `arm-harness` solely for `arm_harness::evidence` (the
canonical shapes) and on `oracle-model` for the independent count oracle.

## What is validated here vs what only silicon can say

| Validated offline, here | Only silicon can say |
| --- | --- |
| The schemas are valid draft-2020-12 and match `evidence.rs` field-for-field (every fixture validates). | Whether any real run-set's `BR_RETIRED` counts are bit-deterministic. |
| The checker catches all seventeen failure modes, and *which* check catches each. | Whether armed overflows are actually delivered exactly once on N1. |
| Counts in the fixtures are exactly what the oracle predicts under the synthetic weights. | The **measured** weights (count offsets) and `skid_margin` — AA-1's to produce. |
| The checker refuses missing weights / skid margin rather than defaulting. | Whether the patched `Preempt` exit is deterministic (AA-3). |

The checker being green on a fixture proves the *checker* works. It says nothing
about the hardware — that is stages AA-0..AA-6's alone to answer, and the checker is
the instrument they answer *with*.
