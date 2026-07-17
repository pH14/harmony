# Task 124 — Repair the vacuous public-api CI gate (hm-64j)

Claim `hm-64j` first (`bd update hm-64j --claim`).

## Root cause (from PR #124 verify tribunal, finding V3, CONFIRMED)

The `public-api` job in `.github/workflows/quality.yml` installs the pinned nightly
toolchain but **never installs the `cargo-public-api` CLI binary** that the `public_api`
integration tests shell out to. When the binary is absent the tests take a loud-skip
branch and exit 0 — so the gate has reported **green-by-skipping**, not green-by-checking,
for **all 21 workspace crates + `guest/sdk`** ever since **#118** (the GHA migration moved
this job off the pre-provisioned self-hosted box, where the tool existed, to hosted runners
and dropped it). The gate proves nothing today. (Both the PR-120 and PR-124 closers had to
run `cargo public-api` by hand to verify their crates because the CI job is asleep.)

## The fix

1. **Install the tool in the job.** Add a `cargo-public-api` install step to the
   `public-api` job before the snapshot-test step — use `taiki-e/install-action@v2`
   with `tool: cargo-public-api` (prebuilt binary, seconds; the same mechanism the fmt/
   clippy/deny jobs use for their tools), or `cargo install --locked cargo-public-api`
   mirroring the Kani job if the prebuilt is unavailable for the runner. The pinned
   nightly is already installed by the job; `cargo-public-api` needs it for rustdoc-JSON.

2. **MANDATORY drift audit BEFORE pushing — do not skip, do not scope down.** This will be
   the gate's first *real* run since #118, so committed `tests/public-api.txt` snapshots may
   have silently drifted. Reproduce the job's exact command locally (install the tool +
   pinned nightly `nightly-2026-06-16` via `scripts/install-quality-tools.sh`), running the
   full crate list the job runs:
   ```
   cargo test -p hypercall-proto -p snapshot-store -p unison -p vtime -p vm-state \
     -p vmm-backend -p vmm-core -p lapic -p gicv3 -p environment -p control-proto \
     -p explorer -p sdk-events -p flow -p matcher -p runtrace -p campaign-runner \
     -p tactics-regime -p logtmpl -p resolution -p det-corpus \
     --test public_api -- --ignored --nocapture
   cargo test --manifest-path guest/sdk/Cargo.toml --test public_api -- --ignored --nocapture
   ```
   For **every** crate that now fails the snapshot diff, classify the drift:
   - **Legitimate/intended** (an additive or renamed surface that matches already-merged
     work): regenerate and commit the updated `tests/public-api.txt`, and state in the PR
     which merged change caused it.
   - **Unintended** (a real public-API leak, an accidental `pub`, a contract regression):
     do **NOT** paper it over by blessing the snapshot — report it, file a bead, and
     escalate. A silent gate may have let a real leak through; that is exactly what this
     repair is meant to catch.
   Report the full drift inventory (crate → drift → disposition) in the PR description.

3. **cfg(linux) crates.** Several crates are Linux/KVM-only (`vmm-backend`, `vmm-core`,
   `lapic`, `gicv3`, and any that `cfg(linux)`-gate surface). `cargo public-api` must
   compile the crate to emit rustdoc-JSON, so a Mac run cannot audit Linux-only surface.
   Run those crates' `public_api` test **on the determinism box** (`ssh <det-box>`, pin
   with `taskset` per `docs/BOX-PINNING.md`) or confirm hosted-Linux-CI coverage — do not
   silently omit them from the audit (that would reproduce the very gap being fixed). The
   pure-portable crates audit on the Mac.

## Definition of done

`quality.yml` installs `cargo-public-api`; the full drift audit is complete with every
crate's snapshot either verified byte-identical or updated-with-justification (real leaks
escalated as beads, not blessed); the job passes locally/on-box green before push. PR opened
with the drift inventory. `hm-64j` closes on merge.
