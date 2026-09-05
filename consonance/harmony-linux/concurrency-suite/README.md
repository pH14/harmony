<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# M6 concurrency-discovery suite

This directory measures concurrency discovery under the virtual-time model in
`../../../docs/DETERMINISM.md`. It contains two separately compiled,
cooperatively scheduled concurrent payloads:

- `rust-lost-update`: two real Rust threads split a non-atomic increment into
  an atomic load and atomic store. The losing interleaving leaves `1` instead
  of `2`.
- `go-publish-before-init`: two real Go goroutines expose a publication-order
  bug. The consumer can observe the published flag before the value is
  initialized.

Only the selected actor receives a step token, so host thread scheduling cannot
reach the result. Every completed instrumented block crosses the SDK threshold
protocol and gives the host the canonical, actor-id-ordered runnable set. A
selection indexes that set; a singleton set is forced to selection zero and
does not consume a search choice.

`m6-plan.json` is the predeclared measurement plan. The Rust entry is a seeded
reproducer. The Go entry is held out: the plan contains a fixed seed and budget,
but no reproducing schedule. The search driver enumerates the generic three-bit
schedule vocabulary in seed-permuted order and stops at the first finding.

Run the complete claim-based measurement from the repository root:

```sh
scripts/run-m6-concurrency.sh /tmp/harmony-m6-report.json
```

The command builds both payloads, runs every wrong-schedule negative, verifies
same-schedule transcript identity, performs the held-out search, checks the
per-bug report with a separately implemented semantic model, and proves that
independent comparator can fail by planting a false Go reproducer. The report
records the full SDK threshold trace rather than only a final result.

The suite uses the exact production HCP1 SDK operation. Its process transport is
the deterministic measurement seam; the production guest `/dev/harmony` and VMM
doorbell routing are covered separately by the threshold-protocol oracles in
`hypercall-proto`, `harmony-sdk`, `libvoidstar`, and `vmm-core`.
