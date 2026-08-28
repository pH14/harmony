# Testing consonance

A green test must be able to catch the regression it claims to cover.

## Repository gates

Run the commands in `AGENTS.md`: all-feature build, nextest, clippy with
warnings denied, rustfmt, cargo-deny, and the quality-toolchain checks. Every
crate containing `unsafe` also runs under Miri through an
interpreter-reachable seam.

Property suites use at least 256 cases. Kani proves bounded arithmetic and wire
invariants. Public-API snapshots freeze supported surfaces. Coverage and
mutation floors ratchet upward.

## Determinism oracles

Same-seed runs compare normalized event logs, checkpoint/state-hash sequences,
guest output, and placement of scheduled events. Each comparator is credited
only after a planted mismatch is shown to fail and localize the difference.

The fixed live matrix is:

- macOS arm64 using Hypervisor.framework;
- Linux arm64 using KVM on msr1;
- Linux x86 KVM on both Intel and AMD GitHub-hosted runner pools.

Virtual-time tests assert that each normalized exit advances by its contract
constant, timers fire at the first eligible exit boundary, idle jumps land at
the modeled deadline, and snapshot/restore resumes the same accumulator and
entropy position.

## Hardware qualification

Hardware-specific lanes verify the frozen instruction/register surface,
save/restore fixpoints, normalized exit behavior, and cross-host identity. A
missing runner or skipped prerequisite is inconclusive, never a pass.
