# Task 80 — inspection verbs: `read`/`regs` on the control transport + the moment address, proven live

> **FRONTIER · the resolution observation surface.** The control protocol can `hash` guest state
> but cannot *look at* it: there is no memory-read verb and no register view anywhere on the wire
> (`dissonance/control-proto` serves only `hash(scope)`; full register state exists internally via
> `save_vm_state` but is never exposed). This task adds the two observation verbs and proves the
> **moment address** live: a `(genesis-complete Environment, Moment)` pair materializes to a live
> session at exactly that instruction, twice, byte-identically. This is the substrate for
> everything in `docs/RESOLUTION.md`.
>
> Depends on **task 58** (the control-transport server + socket `Machine`). Independent of
> 59/60/61 and of the Wave-5 queue (63–76).

Read first: `tasks/00-CONVENTIONS.md`, `docs/RESOLUTION.md` ("The moment address", "The
search-surface criterion"), `docs/DISSONANCE.md` ("The control transport (verbs)"),
`tasks/25-control-proto.md`, `tasks/58-close-the-loop.md`, `consonance/vmm-core/src/vmm.rs`
(`guest_memory` ~557, `state_hash` ~1478, `save_vm_state` ~953),
`consonance/vmm-backend/src/kvm_sys.rs` (`read_guest` ~265).

## Environment

Wire types, codec, and server dispatch are **mock-backend-testable on macOS + Linux** (the task-58
loopback pattern) and MUST carry the portable gates. The materialize-at-`Moment` proof is
**box-only** (patched KVM, det-cfl-v1, the Postgres image). Pin per `docs/BOX-PINNING.md`; always
revert KVM to stock **1396736** + verify after any patched run.

Surface list (frontier waiver of hard rule 1): `dissonance/control-proto` (wire types + codec +
fuzz corpus), `consonance/vmm-core` (server dispatch + read/regs plumbing).

## What to build

### 1. `dissonance/control-proto`: two observation verbs

- `read { gpa: u64, len: u32 } → Reply::Bytes(Vec<u8>)` — guest physical memory. `len` bounded
  (implementer picks the cap, documents it; oversized/out-of-range → `ControlError`, never a
  truncated success).
- `regs {} → Reply::Regs(RegsView)` — a **versioned** register view: GPRs, `rip`, `rflags`,
  segment selectors, `cr0/cr3/cr4`, and the current `Moment`/V-time. A view, not the save/restore
  format: additive evolution, no round-trip obligation.

Extend the fuzz corpus (decoder fuzzing is an existing gate for this crate).

### 2. `consonance/vmm-core`: serve them

Dispatch in the task-58 server: `read` via the existing guest-memory path, `regs` via the vCPU
state the backend already exposes. **Observation semantics are the contract:**

- Neither verb mutates guest state, V-time, or any hash: `hash(Whole)` before and after any
  sequence of `read`/`regs` calls is bit-identical.
- Neither verb is recorded into any `Environment` (the `docs/RESOLUTION.md` search-surface
  criterion: observation, not a move).

### 3. The moment address, proven

A materialization procedure (client-side; in the demo bin or a test harness — no new crate):
given `(env, moment)` where `env` is genesis-complete, `branch(genesis_snap, env)` then
`run(until = moment)` using the exact-`Moment` stop the deterministic force-exit machinery
provides (tasks 47/55), landing with retired-instruction count == `moment`.

## Acceptance gates

1. **Portable (macOS + Linux, mock backend):** integration tests over the loopback server
   exercising both verbs, including error paths (OOB `read`, oversized `len`); proptest (≥256)
   that arbitrary interleavings of `read`/`regs` between other verbs never change `hash`
   results or `StopReason` outcomes vs. the same sequence without them.
2. **Box gate — the moment address:** against the Postgres workload: pick ≥ 4 mid-workload
   `Moment`s; for each, materialize `(env, moment)` **twice from genesis** → identical `regs`
   (including `rip` and `Moment`), identical `read` of ≥ 3 probe regions, identical
   `hash(Whole)`. Record the table in `IMPLEMENTATION.md`.
3. **Observation invariance on the box:** a full inspection pass (regs + several reads)
   mid-materialization does not perturb the run: continuing to a later `Moment` yields the same
   `hash` as an uninspected control run.
4. Standard suite green on both touched crates; no golden re-blessing (additive verbs; existing
   `live_*` gates byte-identical).

## Box-safety (CRITICAL)

Stock KVM = **1396736**. ALWAYS leave the box on stock + verified after every run; kill harness
processes first, wait for `kvm_intel` users=0, `rmmod`/`modprobe`, verify size on a fresh ssh
connection. Pin to `taskset -c 2` (`docs/BOX-PINNING.md`). Foreground gates; read results before
reporting.

## Non-goals

- `exec` / any guest-input channel (task 81); an `observe`/watchpoint verb family (post-v1 —
  needs the RunTrace/matcher machinery, Wave 5); thread enumeration or guest-OS-aware
  introspection (a `read`-consumer's job, guest-schema-aware, not substrate);
  the `dissonance/resolution` crate (task 82); snapshot performance (D5).
