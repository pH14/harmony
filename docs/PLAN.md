# Consonance implementation plan

Consonance is a deterministic hardware-virtualized machine: the same image,
seed, and input schedule produce bit-identical state and outputs.

## Architecture

- A typed backend enters the guest and surfaces VM exits.
- The architecture vendor layer normalizes exits and enforces the frozen CPU,
  register, interrupt, and device contract.
- One virtual clock advances by deterministic integer durations assigned to
  serviced VM exits. Idle guests jump to modeled timer deadlines.
- Entropy comes from one caller-seeded stream.
- Snapshots contain every state component that can affect the future.
- The control protocol schedules host events at deterministic exit boundaries.

## Delivery order

At each iteration the VMM delivers already-pending modeled interrupts, enters
the guest, classifies and services exactly one exit, advances virtual time,
updates devices and pvclock, applies due host events, and records any requested
checkpoint. All collections that reach bytes have a stable order.

## Verification

The repository gates build, test, lint, format, dependency policy, public API,
coverage, mutation, property tests, Kani, and Miri for unsafe crates. Live gates
exercise Linux x86 KVM, Linux arm64 KVM, and macOS arm64 HVF. The authoritative
determinism argument and instruction tables are in `docs/DETERMINISM.md`.
