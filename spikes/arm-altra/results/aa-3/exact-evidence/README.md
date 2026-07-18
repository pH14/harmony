<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-3 exact-landing ≥10⁶ run — parallel evidence

The patched-KVM (`-aa3preempt`) force-exit + `run_until_overflow` + `single_step` exact
landing, run at ≥10⁶ armed deadlines sharded 76-wide across cores 4–79 concurrently (Paul's
parallel ruling; the concurrent run **is** the co-tenant determinism stress test). Grades on
the seven deterministic-count payloads; `wfi-idle` is excluded (foreman ruling — its timer
resume is a real-time, AA-5 paravirt-clock concern, recorded in `docs/ARM-ALTRA.md`).

The raw records are **578 MB** and live on the ephemeral box, not in git. What is committed
here is the compact, integrity-verifiable evidence trail:

- **`verdict.txt`** — the full `floor-check` aggregate transcript over all 76 shard run-sets,
  recomputed from the raw records with the normative floors
  (`--min-armed-overflows 1000000 --min-cases 500000 --min-reps 2`, **no** `--sub-normative`).
  Headline: **RESULT: PASS (1371 checks)** — `armed-overflow-floor` PASS at **1,010,800**
  armed overflows, `case-coverage` PASS at **505,400** distinct (payload, scale, seed, target)
  cases, and every per-shard check green (totality, multiplicity, count-exactness, skid=0
  exact, mechanism-attestation=Preempt, replay-identity, rep-floor, pinning, perf-config).
- **`determinism.json`** — the solo-vs-co-tenant P0 comparison (Paul's rule: a co-tenant
  digest differing from its solo reference is a P0). A **solo** reference lane
  (`aa3-exact-solo-ref`, run alone on an idle box, base seed `3330000000000001`, condition
  `pinned-solo`) shares its (payload, scale, seed, target) tuples with **co-tenant shard s0**
  (same base seed, condition `co-tenant-other-core`, run under 76-way concurrency). Every
  shared tuple's exact-landing digest (`overflow.landed_digest`) **and** window-end full-state
  digest (`state_digest`) is compared solo-vs-co-tenant; `verdict: MATCH` means co-tenancy did
  not perturb any deterministic guest state.
- **`manifests/aa3-exact-r3-s*/run-set.json`** — the 76 per-shard manifests. Each carries the
  `records_sha256` of its (box-side) records file, the pinned host-kernel hash/build-id, the
  mechanism attestation (`kvm_patched: true`), and the measurement environment — so the
  verdict above is reproducible against the raw records and each shard's provenance is bound.

A representative full shard's records (the `aa3-smk6` smoke, 3500 records) is committed under
`results/aa-3/exact-smoke/` for spot-checking the record schema and re-running `floor-check`
locally.

**Disposition:** AA-3 GO — see `docs/ARM-ALTRA.md` (§AA-3) and the trait-freeze memo in
`docs/ARCH-BOUNDARY.md`. Finding **AA3-F1** (BR_RETIRED under-determines PC; the exact landing
must reach the canonical first-PC-at-`work==target`) is recorded there.
