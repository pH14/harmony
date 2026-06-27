# Task 41 — non-quiescent snapshots: capture in-flight CPU event/interrupt state

> **dissonance substrate unlock.** Task 40's branching demo measured the binding constraint: task 39's
> snapshot codec can only serialize a **quiescent** machine, so a never-halting interrupt-driven guest
> (Postgres + the LAPIC timer) was snapshottable at **0 of 8392** V-time points. Branch-at-boot-only is
> a severe limit — the dissonance mission needs to fork a system *while it is doing work* (mid-transaction,
> mid-replication). This task makes **any V-time point snapshottable** by capturing the in-flight CPU
> event/interrupt state task 39 drops. It is the **deferred PR#7 P2s done properly** (capture, not
> fail-closed-reject). Depends on **task 39** (snapshot substrate), **37** (Postgres, to test mid-workload),
> **40** (branching demo, to re-validate a mid-Postgres fork) — all merged. Branch from current main.
>
> **Environment:** box-only for the determinism gate (patched KVM per [[box-patched-kvm-ops]]); pin per
> `docs/BOX-PINNING.md` (task 38 owns core 2 — use **core 4**). Self-serve box gates via git (workers 36-40
> all did; rsync is blocked, git works).

Read `tasks/00-CONVENTIONS.md`, `tasks/39-live-snapshot-branch.md`, the **PR #7 review comments** (the
deferred P2s: in-flight injection, staged completions, SREGS2/debugreg/timer fail-closed), `consonance/vmm-core/src/snapshot.rs`,
`consonance/vmm-core/src/vmm.rs` (the inject seam + `save_vm_state`/`restore_vm_state`), `consonance/lapic/`,
and `consonance/vm-state/` first.

## The gap (precise)

A snapshot taken mid-execution must reproduce the guest's **pending interrupt/event delivery** exactly.
Task 39's adapter serialized a reduced subset and **fail-closed-rejected** the rest, so any point with an
in-flight interrupt is unsnapshottable. The state that must round-trip:
- **Full `kvm_vcpu_events`** (`KVM_GET_VCPU_EVENTS`/`KVM_SET_VCPU_EVENTS`): `interrupt.injected`/nr/soft,
  `nmi.injected`/pending/`masked`, `exception.injected`/nr/error_code/**payload**, `sipi_vector`, SMI state,
  `triple_fault.pending` — not the reduced subset.
- **LAPIC in-flight bits** (IRR/ISR/the timer) — the `lapic` crate models these; confirm the full register
  file (incl. an in-service or requested vector) round-trips.
- **The VMM inject-seam pending state** — `pending_irq` / accepted-interrupt (`Backend::take_accepted_interrupt`/
  `set_pending_irq`): an IRQ raised+routed but not yet injected.
- **Staged backend completion** — if an exit was serviced but its completion value is still staged in
  `kvm_run` (the PR#7 staged-completions P2), the snapshot point must capture it or be defined to exclude it.

On **restore**, re-establish all of it (`KVM_SET_VCPU_EVENTS` + LAPIC load + seam state) so the guest resumes
mid-delivery identically. Because the injection itself is a deterministic function of V-time, the restored
interrupt fires identically — so determinism is preserved.

## Acceptance gates

1. **Non-quiescent point is snapshottable (box):** at a V-time point where the guest has an interrupt in
   flight (where task 40 measured 0/8392), `save_vm_state` now succeeds and `restore_vm_state` resumes
   bit-identically. Quote the before (rejected) / after (snapshottable) counts on the same Postgres run.
2. **Round-trip mid-execution (box, the milestone):** snapshot a **running Postgres mid-workload**, restore
   into a fresh VM, and resume → the resumed run reaches the same terminal `state_hash` as the un-snapshotted
   run (restore is exact at a non-quiescent point). Deterministic-twice.
3. **Branching from a live snapshot (box):** re-run task 40's matrix but seal `S` at a **mid-Postgres** point
   (not boot-entry) — each seeded fork reproducible (gate 1) and ≥1 divergent (gate 2). This is the capability
   40 documented as missing.
4. **No regression:** quiescent snapshots still work; M1/M2/P6/det-corpus + Linux-boot goldens byte-identical;
   standard gates green (mutants — pin the new event fields with exact-value tests; Miri any new `unsafe`;
   public-api refresh on the box). Revert patched KVM to stock after; verify `lsmod` == 1396736.

## Non-goals

Multi-node / live `pv-net` (D2); crash-consistency / durability faults (D1); the network fault model; the
automated explorer (task 12). This delivers the **single-VM** non-quiescent snapshot/restore — the substrate
both the explorer and a live-system branching demo need. No CPU/MSR contract change; the `state_hash` schema
may grow to include the newly-captured event fields (re-bless goldens only if a non-Linux path's hash changes —
it should not, since these fields are zero on the M1/M2/corpus paths).
