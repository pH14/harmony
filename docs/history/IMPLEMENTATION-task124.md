# Task 124 — Repair the vacuous public-api CI gate (hm-64j)

Infra task (touches `.github/workflows/quality.yml`, no single crate dir), so the
write-up lives here per `tasks/00-CONVENTIONS.md` ("Where the write-up lives").

## What was broken

The `public-api` job installed the pinned nightly toolchain but **never installed the
`cargo-public-api` CLI** that the per-crate `public_api` integration tests shell out to.
When the binary is absent, each test takes its loud-skip branch and returns 0 (see any
`tests/public_api.rs`: the `no such command` / `is not installed` stderr match → `return`
instead of `panic!`). So the gate reported **green-by-skipping**, not green-by-checking,
for all 21 workspace crates + `revision-coordinator` + `guest/sdk` ever since **#118**
moved the job off the pre-provisioned self-hosted box (where the tool existed) onto hosted
runners.

## The fix

`.github/workflows/quality.yml`, `public-api` job: added an `Install CI cargo tools` step
(`taiki-e/install-action@v2`, `tool: cargo-public-api@0.52.0`) after the `rust-cache` step
and before the snapshot-test step — the same prebuilt-binary mechanism the fmt/clippy/deny
jobs use. The job's pinned nightly (`nightly-2026-06-16`) supplies the rustdoc-JSON the tool
needs.

**Judgment call — pinned the version.** The spec suggested unpinned `tool: cargo-public-api`.
`cargo-public-api`'s snapshot *formatting* is version-dependent, so an unpinned upgrade could
flip a byte-identical surface to a spurious diff and re-introduce exactly the "gate reports
the wrong state" failure this task removes. `0.52.0` is the version that reproduced every
committed snapshot byte-identically in the audit below; pinning it (like `cargo-deny@0.19.9`
in the same file) keeps the gate deterministic. If a snapshot is ever intentionally
regenerated under a newer `cargo-public-api`, bump this pin in the same PR.

## Mandatory drift audit — full inventory

First real run of the gate since #118. Reproduced the job's exact command
(`cargo public-api -p <crate> --all-features -sss --color never`, `cargo-public-api 0.52.0`,
`nightly-2026-06-16`) for **every** crate in the job's list and diffed against the committed
`tests/public-api.txt`.

**Disposition: all 23 snapshots verified byte-identical. Zero drift. No snapshot updated,
no leak escalated.** (The PR-120 and PR-124 closers had been running `cargo public-api` by
hand precisely because the gate was asleep, which kept the snapshots current — nothing
slipped through the sleeping gate.)

### Portable crates — audited on macOS (aarch64-apple-darwin)

| Crate | Result |
|---|---|
| hypercall-proto | MATCH |
| snapshot-store | MATCH |
| unison | MATCH |
| vtime | MATCH |
| vm-state | MATCH |
| lapic | MATCH (0 `cfg(linux)` sites — fully portable surface) |
| gicv3 | MATCH (0 `cfg(linux)` sites) |
| environment | MATCH |
| control-proto | MATCH |
| explorer | MATCH |
| sdk-events | MATCH |
| flow | MATCH |
| matcher | MATCH |
| runtrace | MATCH |
| campaign-runner | MATCH |
| tactics-regime | MATCH |
| logtmpl | MATCH |
| resolution | MATCH |
| det-corpus | MATCH |
| revision-coordinator | MATCH (its test is not `#[ignore]`d; also gates the plain nextest suite) |
| harmony-sdk (`guest/sdk`, out-of-workspace) | MATCH (0 `cfg(linux)` sites) |

### `cfg(linux)` crates — audited on the determinism box

`vmm-backend` (24 `cfg(target_os/linux)` sites) and `vmm-core` (16) gate real public surface
behind `cfg(linux)` — `cargo public-api` compiles the crate to emit rustdoc-JSON, so a macOS
run only sees the portable subset. On macOS both showed the **expected** subset-drift (the
committed snapshot's extra lines were exactly the KVM/perf surface — `impl Backend for
KvmBackend`; the KVM `boot_linux_*` / `boot_selected` / `boot_patched_corpus` bringup fns;
the `vendor::x86::work_perf::PerfWorkCounter` module). This is the gap `tasks/124` §3 warns
about, so both were re-audited on Linux where the full surface compiles.

- **Box:** determinism box (Intel i9-9900K, `x86_64-unknown-linux-gnu`, Linux 6.12.90),
  `cargo-public-api 0.52.0` + `nightly-2026-06-16`, compile pinned `taskset -c 1,2,3,4`
  (lower threads, SMT siblings idle; off the CI runner's cores 5-7/13-15 per
  `docs/BOX-PINNING.md`). Snapshot output is CPU-independent (it's a compile, not a
  measurement), so pinning here is box-citizenship, not a determinism requirement.
- **Provenance:** the box worktree was checked out from the repo's own remote at the branch
  head (`a66b4ad`); `vmm-backend`/`vmm-core` source trees and committed snapshots were
  confirmed byte-identical to the branch by content-hash set before the run (26 and 40 files
  respectively). The throwaway worktree and temp files were removed afterward.

| Crate | Result |
|---|---|
| vmm-backend | MATCH — byte-identical, full Linux surface |
| vmm-core | MATCH — byte-identical, full Linux surface |

The box run (`x86_64-unknown-linux-gnu`, same nightly + same `cargo-public-api 0.52.0`) is a
faithful proxy for the hosted `ubuntu-latest` job the fix enables: identical target triple,
toolchain, and tool version ⇒ identical surface. So the first real CI run of the repaired
gate is expected green.

## Gate status

- `quality.yml` installs `cargo-public-api@0.52.0`; YAML parses; step ordering correct.
- Full drift audit complete: 23/23 crates byte-identical, nothing blessed, nothing escalated.
- No crate source was touched (fix is CI-only + this doc), so the per-crate cargo gates are
  unaffected; the change under test is the workflow file, exercised by the audit above.

`hm-64j` closes on merge.
