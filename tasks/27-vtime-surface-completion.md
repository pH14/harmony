# Task 27 — V-time determinism surface completion (deferred from #45)

Read `tasks/00-CONVENTIONS.md` first. Touch `consonance/vmm-core/` (+ the contract docs only if a
disposition is clarified). This finishes the V-time determinism surface that task 21
(`PatchedKvmBackend`, merged #45) deliberately left to a follow-up so the instruction-sweep corpus
could start ASAP. The RDTSC/RDTSCP/RDRAND/RDSEED **instruction** path is done and box-proven (P6
deterministic-twice); these are the corners around it that the #45 cross-model review rounds 2–3
surfaced. **All are loud / non-silent today** (abort or fail-closed), so deferring them was safe.

> **PROMOTED (2026-06-25) — item 2 is now the blocking item.** The task-28 box corpus's O1
> (determinism) is **blocked** on it: on real hardware the `VTIM` `state_hash` chunk diverges
> intermittently across two same-seed runs (the box O1 diagnostic localized the *sole* divergence
> to `vtim`; all other components + `observable_digest` match). **Do item 2 first**; items 1 and 3
> stay lower-priority follow-ups. The corpus proof point (PR #51) re-runs green once item 2 lands.

## Items (each test-pinned)

1. **TSC-MSR `emulate-vtime` wiring** (the round-3 [P1]). The contract marks `IA32_TSC` (0x10) and
   `IA32_TSC_ADJUST` (0x3b) as `emulate-vtime`, but `vmm.rs` still routes them to
   `dispatch_rdmsr/wrmsr` → `ContractViolation` ("V-time is not wired in this skeleton" — stale
   message; V-time *is* wired on the patched path). A guest reading TSC via `RDMSR(0x10)` aborts
   despite the patched path advertising deterministic TSC. **Fix:** route the `EmulateVtime` MSR
   dispositions through the same `VClock::tsc(work)` (and the `TSC_ADJUST` state for 0x3b) used by
   the RDTSC instruction completion. Test: a guest reading `IA32_TSC` via RDMSR gets the same
   V-time value the RDTSC instruction would, deterministic-twice.

2. **VTIM hash determinism + restore-transparency** (round-3 [P2], **extended by the task-28 box
   corpus — the blocking item**). Two problems in `encode_vtime` (the `VTIM` state_blob chunk):
   - **(a) restore-transparency:** it encodes `vns_base` + `work` **separately**, so a restored VM
     (`vns_base=elapsed, work=0`) and a fresh VM at the same *effective* V-time (`vns_base=0,
     work=elapsed`) hash **differently**, breaking the transparency `unison::compare_runs` wants.
   - **(b) determinism-twice (found on the box, PR #51):** it hashes a **live read of the raw
     retired-branch `work` counter** (`vt.work.work()`) at hash time. Perf **skid** (post-last-
     intercept / exit-path branches) makes the *terminal* raw work non-deterministic across two
     same-seed runs — even though work at every *intercept* is deterministic — so the `VTIM` chunk
     (hence `state_hash`) **diverges intermittently**. The box O1 diagnostic localized the sole
     divergence to `vtim` (every other component + `observable_digest` match); P6 carries the same
     chunk but its payload never exposed it.

   **Fix:** hashing `snapshot_vns(work) = vns_base + work` (the (a)-only fix) is **insufficient** — it
   still carries the terminal skid. Anchor the hashed V-time to a **deterministic** work value: the
   work at the **last intercept** (the synchronized point the patched backend already corrects to),
   or drop the ephemeral raw terminal counter from the hash entirely, then canonicalize a single
   effective-V-time field. Tests: **(i)** two fresh same-seed runs produce **byte-identical** `VTIM`
   (deterministic-twice — **re-verified on the box**; this is what unblocks task 28's O1);
   **(ii)** a snapshot/restored VM and a fresh VM at the same effective V-time hash the **same**;
   M1/M2 (`vtime: None`) unchanged; P6 still deterministic-twice (its `VTIM` hash value changes —
   capture the new box evidence).

3. **Clear `rng_completion_staged` on restore** (the round-3 [P3]). `restore_vtime` can leave the
   staged-RNG flag set, so a caller that restores then immediately `save_vtime`s gets a spurious
   `ContractViolation` until another step clears it. **Fix:** clear the flag in `restore_vtime`.
   Test: restore-then-`save_vtime` succeeds at a clean boundary.

## Gates / acceptance

Standard `vmm-core` gates (build/nextest/clippy -D warnings/fmt) + `contract_hash` unchanged + M1/M2
`state_hash` unchanged. **Re-run P6 on the box** (the proxy patched modules at
`<box>/kvm-spike/deb612/.../kvm{,-intel}.ko` — load, `live_determinism` deterministic-twice, revert
to stock) and confirm still byte-identical; the restore-transparency change alters the VTIM hash,
so capture the new (still-deterministic-twice) P6 evidence in `IMPLEMENTATION.md`. Cross-model pass
(the V-time snapshot/hash path has been subtle — keep iterating until a clean pass).

## Out of scope

The full mid-exit VM snapshot (capturing/replaying the staged `complete_userspace_io` completion)
remains **task-08 snapshot-store** territory — #45's `save_vtime` already fails closed at that
boundary. **pv-blk fault enforcement** (torn writes / NOSPC / flush-barrier reordering — the
crash-consistency core) is its own future spec when writable storage lands; not here.
