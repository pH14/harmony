# Task 95 M2 — O(dirty) capture + remap restore (D5)

Implements `docs/INTEGRATION.md` §5's Memory/snapshots row — *"KVM dirty-log
harvest → `DeltaBuilder`; `materialize()` → memslot swap"* — and retires
ROADMAP D5. M1 (the production-shape bench + portable store optimizations)
merged separately (PR #91); its numbers are the store-side baseline quoted
below. Surface: `consonance/vmm-backend` + `consonance/vmm-core`;
`consonance/snapshot-store` untouched (read-only per spec — no store change
was needed; its public API sufficed, as predicted).

## M2.1 — capture: KVM dirty-log harvest → `snapshot_derive`

- `KvmBackend::map_memory` registers guest-RAM memslots (**both** LAPIC-split
  parts) with `KVM_MEM_LOG_DIRTY_PAGES`, default **on**;
  `set_dirty_log_enabled(false)` is the `flags: 0` A/B arm of gate (a0) and
  the emergency revert. The per-slot `(slot, gpa, size)` table is recorded at
  map time; a failed split map rolls its entries back with the memslots.
- New `Backend::harvest_dirty_gfns()` — `KVM_GET_DIRTY_LOG`
  (retrieve-and-reset) per RAM slot, decoded to absolute gfns by a **pure,
  portable** `region.rs` helper (exact-value + property tests on macOS),
  sorted + deduplicated. The trait default answers `Unsupported`, so a
  backend without tracking makes every caller full-scan; `Box<dyn Backend>`
  and `PatchedKvmBackend` forward explicitly (the shadowing landmine is
  pinned by a test).
- **The safety rule, implemented one level higher than the spec asked** —
  this is the part a reviewer should read first. KVM's dirty log sees only
  *guest* writes (it tracks sptes, not the userspace mapping), but vmm-core
  writes guest RAM host-side in exactly three production places: the doorbell
  response page (every hypercall), `CorruptMemory` host faults, and
  `restore_guest_memory` (full image). A harvested set that missed those
  would be **silent snapshot corruption** — the under-report case the spec's
  asymmetry names. So `Vmm` now tracks host-side writes itself (a `BTreeSet`
  of gfns; the full-image write latches a *wholesale poison* instead), and
  `Vmm::harvest_dirty_gfns() -> Option<Vec<u64>>` returns the union — or
  `None` on **any** doubt (backend can't harvest, wholesale write pending).
  It is deliberately an `Option`, not a `Result`: the dirty set is a cost
  hint, never a correctness input, and there is no error a caller may act on
  other than "full-scan". The invariant that any *future* host write path
  must call `mark_host_dirty` is documented at that helper.
- **Seal wiring** (`ControlServer`): the session tracks `derive_parent` — set
  after a successful seal (the new snapshot) and after a successful
  branch/replay (the restore source; correct for the memcpy path because the
  memcpy writes exactly the parent's bytes, and for the remap path because
  the mapping *is* the parent's image), `None` on fresh boots and whenever
  the re-arm (`Vmm::reset_dirty_tracking`, a harvest-and-discard) fails. A
  seal derives iff the parent is still live, `chain_len < max_chain_len`, and
  the harvest vouches; every other path — including a failed derive — is
  `snapshot_base`. The seal RPC can never fail because the optimization was
  unavailable.
- **Bounded chains**: `SnapshotEngine::max_chain_len` (default **32**, a
  config knob, `0` = never derive). At the bound the seal flattens via a
  fresh base; content-dedup keeps the flatten cheap in storage. M1's depth
  sweep showed `materialize` flat in depth at 1/8/32 (913/853/874 ms at
  r=131,072 — inside I/O noise), so 32 is not near a cliff.

## M2.2 — restore: the mapping becomes the memslot backing

- `RamBacking { Owned(GuestRam), Snapshot(Mapping) }` — the `Vmm` owns either;
  `Vmm::new` is unchanged (wraps `Owned`), `Vmm::with_backing` is the new
  entry. `ram_backing_is_snapshot()` is the gate/diagnostic probe.
- `bringup::compose_restore_target(backend, mapping, wire_lapic)` composes a
  fresh VM **around** a materialized mapping: contract policy → `map_memory`
  over `mapping.as_mut_slice()` (both LAPIC-split parts, same `SAFETY`
  argument as `compose` — the mapping moves into the `Vmm`, mmap pages never
  move) → **no loader, no entry state** (the snapshot's `restore_vm_state`
  supplies the full register file). `MAP_PRIVATE` does the rest: guest writes
  stay private, untouched pages fault lazily, the store/tempfile are never
  written back.
- `ControlServer::set_remap_factory` (a new `RemapVmmFactory` alias) +
  `RestoreMode { Remap, Memcpy }` — the A/B knob, default `Remap`, effective
  only once a remap factory is installed, so **every existing composition
  (including dissonance's conductor) is byte-for-byte unchanged** until its
  root opts in. A remap-path recoverable restore failure re-boots via the
  normal factory so `RestoreFailed` leaves the session on exactly what the
  memcpy path leaves it on.
- **The escalation rule was not needed**: `Vmm` construction takes the
  backing as a value and no device setup writes RAM before `map_memory`, so
  no constructor restructuring (and no new `unsafe` beyond the one granted
  `map_memory` call in the composer) was required.

## Deviations considered and rejected

- *Changing `VmmFactory` to take an `Option<RamBacking>`.* Rejected:
  `VmmFactory` is constructed in `dissonance/conductor` (outside this task's
  surface); the additive second factory keeps the M2 surface waiver honest
  and existing roots source-compatible.
- *Tracking host writes only at the three current call sites without the
  wholesale latch* (treating `restore_guest_memory` as content-equal to the
  parent, which it is on the branch path). Rejected: `restore_guest_memory`
  is `pub` and callable outside the branch flow (tests do); assuming
  content-equality there would make a *caller pattern* a correctness
  precondition. The latch makes the safe thing automatic and the branch path
  re-arms explicitly right after.
- *A `Backend`-level enable verb for dirty logging.* Rejected: composition
  roots name concrete backends already (R-Backend's one allowed place), so a
  concrete-type knob (`KvmBackend::set_dirty_log_enabled`) suffices without
  widening the trait every mock must honor.
- *Deriving after `drop` of the parent handle by retaining it engine-side.*
  Rejected: the spec's rule is parent-liveness-checked fallback, and holding
  a released snapshot alive for a cost optimization inverts the retention
  pool's authority over what stays resident.

## Review-pass fixes (multi-agent review, 2026-07-09 — findings folded in)

Three confirmed correctness defects in the first cut, all fixed:

1. **`map_memory` rollback truncated `dirty_slots` by the part count** even
   when logging was off and nothing had been pushed — a debug-build underflow
   panic on a partial `flags: 0` split map, or silent deletion of earlier
   slots' harvest entries (an under-report path). Now truncates to the length
   recorded at entry.
2. **A failed seal left `derive_parent` armed after the harvest had drained
   the window** — a caller retrying `Snapshot` through the pub `handle` API
   would derive over a window missing everything dirtied before the failure.
   `snapshot()` now `take()`s the parent across the fallible seal: a failed
   seal always leaves the next seal full-scanning.
3. **`harvest_dirty_gfns` gated completeness on the current knob, not the
   slots**: map RAM with logging off, flip the knob on → an empty harvest
   vouched as complete. The backend now latches `unlogged_slot` forever once
   any RAM slot is registered unlogged — completeness is a property of the
   slots, not the knob position.

Plus: the mock's scripted harvest became **accumulate-then-drain** (KVM's
actual semantics) instead of a queue of per-harvest sets, killing the
two-harvests-per-seal consumption landmine that could make cross-seal tests
vacuous; the redundant second dirty-log drain per derived seal was removed
(the harvest's own retrieve-and-reset is the re-arm); the a0/a A/B arms now
boot through **one shared composition** (`boot_linux_patched_with_dirty_log`)
so the gate can never compare differently-wired VMs; the redundant gfn-bound
pre-check was dropped (the engine's own check + fallback covers it); two test
helpers were de-duplicated/doc-fixed.

**Known cost accepted (review finding, PLAUSIBLE):** dirty logging defaults
on for every KVM composition, so never-snapshot workloads pay the one-time
write-protect fault per touched page (and KVM's hugepage-split behavior on
logged slots). This is what the spec mandates; gate (a0) proves it
hash-inert, gate (c)/(d) and the task-96 stopwatch quantify the wall-clock
cost, and `set_dirty_log_enabled(false)` is the composition-root revert if a
regression shows up. Called out here so the number gets read, not assumed.

## Portable evidence (all green, macOS + x86_64-linux cross-check)

486 tests across `vmm-backend` + `vmm-core` (60 + 426), including new:
harvest union/drain/poison semantics; doorbell-write coverage; bitmap→gfn
decode (exact-value, LAPIC-split translation, padding bits, 512-case
naive-scan equivalence property); seal-path chain assertions
(derive-when-tracked, full-scan-when-not, chain-bound flatten, dropped-parent
and wholesale-write fallbacks); the derived-capture materialize-equality
closure; the `Memcpy` vs `Remap` bit-for-bit branch A/B over the production
`compose_restore_target`; remap-failure session recovery; the
`Box<dyn Backend>` harvest-forward pin. Every pre-existing test passes
unmodified. Standard gates: build / nextest / clippy `-D warnings` / fmt /
`cargo deny` on macOS, plus `cargo check` + clippy on
`--target x86_64-unknown-linux-gnu` (the cfg(linux) review-gap discipline).

## M1 baseline (Apple M1 Max, from `consonance/snapshot-store/IMPLEMENTATION.md`)

- 2 GiB base seal (quarter-resident): **0.487 s** (was 1.413 s pre-M1).
- `dirty_delta_seal`: 1.4 ms @ n=512 → 853 ms @ n=262,144 — the M2.1 payoff
  curve capture now rides.
- `materialize` at r=131,072: **815 ms**, at the mmap-memcpy floor; flat in
  chain depth (1/8/32).
- **The 16:1 floor warning**: on the laptop the *tempfile write inside
  `materialize`* (836 ms at r=131,072) dominates the restore path over the
  full-2-GiB memcpy M2.2 removes (51 ms, ≈39 GiB/s anonymous-memory copy).
  Gate (d) below therefore measures **both** floors on the box before
  attributing M2.2's win; if the box's tempfile write dominates similarly,
  the remap's headline value is the *lazy fault-in* (not paying the eager
  copy at all for untouched pages) plus the removed memcpy, and the next
  lever is not materializing eagerly at all (task 68's territory).

## Box gates (det box, patched KVM, 2026-07-09 — ALL PASSED)

Harness: `consonance/vmm-core/tests/live_dirty_remap.rs`, run per
`docs/BOX-PINNING.md` on a `box-window.sh` lease (core 2), smoke-fired (a0
alone) before the full spend; the window reverted to stock KVM **1396736,
REVERT OK** on release. Guest: the 2 GiB Postgres image, patched backend,
seed `0x0095_D127_5EED_C0DE`, `DR_RUN_VNS=20e6` / `DR_DELTA_VNS=5e6` defaults.

| gate | what | result |
|---|---|---|
| a0 | dirty logging inert (same stop + `state_hash`, no seal) | **PASS** — logging-on and `flags: 0` arms bit-identical at the same Moment |
| a | harvested derive ≡ full-scan capture | **PASS** — chains (1, 2) vs (1, 1), both arms sealed at V-times (74 060 614, 80 078 537); post-replay whole-state hashes identical (`0fc751a9…`) |
| b | `Remap` ≡ `Memcpy` branch | **PASS** — identical stop `Deadline(80 078 537)` + hash (`9fb44634…`); remap arm asserted mapping-backed |
| c | `seal_rate_sweep` + `live_materialization` unchanged | **PASS** (see the gate-c note below — includes a main-tree control run) |
| d | the numbers | see below |

### (d) — the numbers (box, core-pinned, exclusive window)

Seal (gate a's schedule; second seal covers a 5 M-vns mid-boot span):

- **base seal (full 2 GiB scan): 170 ms** — the box floor M1's laptop 487 ms
  corresponds to.
- **derive seal over the harvested dirty set: 17 ms — 10× the full scan**, and
  that includes the per-slot `KVM_GET_DIRTY_LOG` ioctl + decode.
- flags:0 arm's second seal (full rescan → base): 139 ms (scan-domination —
  what a derive-without-dirty-set costs).

Restore (gate b's branch verb, wall time of the whole `Branch` RPC —
materialize + fresh-target composition + restore + reseed):

- **memcpy path: 1 387 ms** (materialize → full factory boot with kernel load
  → 2 GiB `copy_from_slice`).
- **remap path: 73 ms — 19×**: no boot-image load, no memcpy; the mapping is
  the memslot backing and untouched pages fault lazily. (At this mid-boot
  Moment the resident set is small, so `materialize`'s tempfile write — M1's
  16:1 floor warning — is far from dominant; at campaign-scale resident
  counts the remap win narrows toward the materialize floor, which is
  task 68's lazy-materialization territory, as the M1 baseline section
  predicted.)

Chain-depth distribution under `max_chain_len = 32`: the gate schedule only
reaches depth 2 by construction; campaign-shape distributions land with the
conductor remap-factory opt-in + task-96 stopwatch (bead filed).

### Gate (c) — the full story (three box rounds)

- **`seal_rate_sweep` (c2): PASS**, 827 s, exclusive window, current image,
  dirty logging on by default throughout — every §1–§4b determinism assertion
  green, mechanical summary "GO (grid-restricted)" as before. Always-on
  `KVM_MEM_LOG_DIRTY_PAGES` did not perturb the sweep.
- **`live_materialization` (c1): PASS in task-78's sanctioned shape**
  (`HOPS=4`, the Jul-2 pr44-built Postgres image — exactly the configuration
  task 78's own box gate passed with; draw probes came back `[f,f,f,true]`,
  matching that run) — with conductor's seals now taking the **new derive
  path** end-to-end: parent-rooted depth beats the task-63 baseline, eviction
  round-trip bit-identical (folded + from-genesis), composed reproducer
  replays with identical stop + `state_hash`.
- **The first c1 attempt failed — and the failure is NOT task 95's.** With
  default knobs (`HOPS=3`) on the box's *current* image, the task-78
  `REQUIRE_DRAWS` precondition fails (`hop_draws` all false; the tail draws)
  while **every substantive assertion inside still passes** (round-trip
  identities, reproducer closure, 4 509 ppm vs the 15 463 baseline). A
  **main-tree control run failed identically**, pinning the cause: the box's
  canonical `initramfs-postgres.cpio.gz` was rebuilt 2026-07-09 02:56 (the
  t81 checkout's build, md5 `9860a065…`) and differs from the Jul-2 pr44
  build (md5 `46b14619…`) the task-78 gate was proven on — the new image's
  first entropy draw lands past the default hop windows. Filed as a P2 bug
  bead (the gate is broken **on main** with default knobs on the current
  image); evidence in `/tmp/t95-gatec.log` on the box (c0 section).

Box left on stock KVM **1396736, REVERT OK**, zero leases, after every round.
