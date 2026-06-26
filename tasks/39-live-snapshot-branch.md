# Task 39 — live VM snapshot / branch (Phase 4): wire snapshot-store + vm-state into vmm-core

> **START NOW · dissonance mechanism, the long pole.** Runs **in parallel with task 36** (consonance) —
> independent, both box-only. This is the substrate the explorer (task 12) and the branching demo
> (task 40) depend on: "branch" *is* restore-snapshot + reseed, and without live restore the dissonance
> crates stay pure-logic islands. Works against the guest you **already boot** (task 34) — it does **not**
> need Postgres. Depends on **snapshot-store + vm-state merged** (both built, pure-logic) and **task 08's
> chosen restore mechanism**. Branch from a main with task 34; fast-forward local `main` before spawning.
>
> **Environment:** box-only (Linux bare-metal Intel, VMX, `/dev/kvm`; dirty-log + memslot remap are
> KVM-specific). The vm_state-adapter *logic* is Mac-unit-testable against a mock backend; live restore
> is box-only. `unsafe` is **granted** for the KVM ioctls + the mmap/memslot remap, each with a
> `// SAFETY:` comment and a Miri-exercisable seam (see Convention rule 7 + AGENTS.md unsafe⇒Miri).

Read `tasks/00-CONVENTIONS.md`, `tasks/08-snapshot-restore-spike.md` (+ its results — the chosen
mechanism + restore-latency baseline), `tasks/09-vm-state.md`, `docs/INTEGRATION.md` **§4 (snapshot
contents checklist)** + **§5 (adapter map)**, `consonance/snapshot-store/src/lib.rs` (the `Store` API),
`consonance/vm-state/src/lib.rs` (the `VmState` codec), and `consonance/vmm-core/src/vmm.rs`
(`save_vtime`/`restore_vtime`/`state_hash` — the V-time-only precedent this generalizes) first.

## What exists vs. what this builds

- **Built (pure logic, don't reimplement):** `snapshot-store` — layered CoW page store, content-addressed
  by BLAKE3, `begin_base`/`derive(parent)`/`write_page`/`seal(vm_state)`/`materialize(snap) -> Mapping`
  (private CoW mapping over a sparse tempfile) — and `vm-state` — the versioned TLV codec for the
  non-memory blob (`VmState::encode`/`decode`). Both treat the KVM side as "lives elsewhere." **This task
  is that elsewhere.**
- **Exists in vmm-core:** `save_vtime`/`restore_vtime` (V-time/RNG block only) and an ad-hoc
  `state_hash`. **No full memory snapshot, no vm_state adapter, no memslot remap, no branch.** vmm-core
  does not yet depend on snapshot-store/vm-state — adding those two deps is a **reviewed addition**
  (record it in the PR like the `kvm-*` deps), they are first-party workspace crates.

## Phase 1 — the vm_state adapter (canonical blob replaces the ad-hoc hash)

Per INTEGRATION.md §5: vmm-core reads the **live** machine via backend ioctls and fills `vm-state`'s
plain-data structs — GPRs, segment/control regs, XCR0, debug regs, pending events, MP state, the
contract MSR set, the XSAVE image, the V-time block (reuse `save_vtime`), the timer queue, the hypercall
dispatcher's saved state, the device state (LAPIC + the bring-up device shims), the `contract_hash` —
then `VmState::encode` to bytes. Restore decodes and writes them back. Enforce INTEGRATION.md §4's
**snapshot-only-at-a-quiescent-point** rule with an assertion (no armed-but-unfired injection plan in the
blob — vm-state deliberately has no plan field). Fold the canonical blob into `state_hash` (BRINGUP: "the
canonical `vm_state` encoding replaces the ad-hoc register hash") — non-Linux paths must stay
byte-identical, so gate the swap so M1/M2/corpus goldens don't move (or re-baseline them in one audited
step, digests quoted).

## Phase 2 — dirty-page harvest → snapshot-store

Base layer = the booted image via `Store::begin_base()` → `write_page` per guest frame → `seal(vm_state)`.
Each later snapshot = the pages **dirtied since its parent** (KVM dirty log / write-protect per task 08)
harvested into `Store::derive(parent).write_page(...)` then `seal(vm_state)`. Capture is
dirty-set-proportional, not image-size-proportional.

## Phase 3 — restore-by-remap (task 08's mechanism)

Implement the mechanism task 08 measured + chose: `Store::materialize(snap)` yields a private CoW mapping;
point the KVM memslot at it (memslot swap / `mmap`-remap / `MADV_DONTNEED` per the spike's verdict) and
restore the vm_state into vCPU + devices. Restore cost tracks the dirty set, not guest size. Verify via
the guest (the spike's discipline: confirm restored GPAs read back the **restored** backing, observed by
the guest, not just host reads).

## Phase 4 — branch + shared base

`branch(snap, seed')` = restore(snap) + reseed (a fresh entropy stream / forked V-time) → a divergent
continuation. Share **one read-only base image** across N materialized views (INTEGRATION.md §5 / task
08's sharing measurement) so N VMs fork from one boot without N copies.

## Acceptance gates

1. **Restore replays bit-identical (box, the milestone gate):** snapshot the running task-34 guest at a
   quiescent `HLT`, restore it, and run forward — the restored continuation is **bit-identical** to the
   un-snapshotted continuation (serial + `state_hash`) from that point. Quote the equal digests. *Same
   state ⇒ same future.*
2. **Restore latency is dirty-set-proportional**, measured on the box, quoted against task 08's spike
   baseline (and beating the full-`memcpy` baseline).
3. **N VMs share one RO base:** materialize N independent CoW views from one base, run/branch each,
   quote store sharing stats (pages stored once store-wide).
4. **vm_state round-trips** and now drives `state_hash`; M1/M2/P6 + det-corpus goldens byte-identical
   (or re-baselined in one audited step with digests). Standard gates green incl. **mutants** (pin the
   adapter field set + restore logic with exact-value tests), **Miri** (the granted `unsafe`, via the
   in-process seam), **public-api** (refresh on the box for the `cfg(linux)` surface).
5. **Box hygiene:** revert to stock KVM after each patched run; verify `lsmod`.

## Non-goals

The explorer **policy** (task 12 — this builds the mechanism it drives, not the search); the fault seam
(`dissonance/environment`); the branching **demo** on Postgres (task 40); durability/disk faults (the
deferred host-side RAM-disk model, D1); snapshotting across a *device-surface* larger than task 34's
(LAPIC + bring-up shims) — as the workload surface grows (task 36/37), §4's checklist must grow with it,
but that tracks the workload, not this task. Build on snapshot-store / vm-state / task 08 — don't
reimplement CoW or re-measure the mechanism.
