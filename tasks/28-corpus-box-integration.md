# Task 28 — corpus box-integration: run the C1 corpus on the patched backend (det-corpus Machine + report channel + O2 goldens)

Read `tasks/00-CONVENTIONS.md`, `docs/DETERMINISM-CORPUS.md`, then the merged pieces this composes:
`consonance/det-corpus/` (#48, the O1/O2/O3 oracle runner over a `unison::Machine`), `guest/payloads/`
(#49, the C1 instruction-sweep payloads — shape-gated under QEMU, `report()` currently a no-op),
and `consonance/vmm-backend/src/patched_kvm.rs` + `consonance/vmm-core/` (#45, the PatchedKvmBackend
+ deterministic RDTSC/RNG, proven by P6). This is the **third piece** that makes the conformance
corpus *run on the 9900K*: a `det-corpus` `Machine` backed by the VMM running a payload, a **report
channel** for the trap-dependent values, the O2 digest goldens captured through it, and the box
O1/O2 gate.

> **Why it's separate:** #48 drives a `Machine` (a logical guest); #49 ships the payloads; neither
> wires "the VMM running a payload" as a `Machine`, and #49 deliberately deferred the box value
> capture ("Gate 4") because the report transport was undesigned. This task builds that bridge.

## Part 1 — the report channel (the one new ABI)

The payloads must report their **trap-dependent values** (the V-time TSC reads, the seeded RNG
words, the retired-branch counts) to the host oracle. The serial lane can't carry them (timing /
ordering would perturb the byte stream), and #44's **port-IO doorbell owns `0x0CA1`** — so the
report stream gets its **own dedicated port**, distinct from the doorbell:

- **`REPORT_PORT` = `0x0CA2`** (pick + pin it; adjacent to but separate from the doorbell). Each
  `OUT REPORT_PORT, EAX` (a 32-bit write) appends `EAX` to an **ordered report stream**;
  `report(u64)` is two writes (low dword then high). Host side: stock KVM surfaces it as
  `Exit::Io { port: 0x0CA2, size: 4, write: Some(v) }` — vmm-core appends `v` to a `Vec<u32>`
  report stream on the VM. No completion needed (it's a write/OUT). Determinism-clean: every
  reported value is already deterministic (V-time TSC / seeded PRNG / retired count), the stream is
  ordered by execution, so the stream is a pure function of the run.
- Wire it in `consonance/vmm-core` (the `Exit::Io` dispatch gains the report-port case → push to the
  stream) and document the ABI in `docs/INTEGRATION.md` (a new "report channel" section next to §1's
  doorbell) and add a `REPORT_PORT` row to `docs/cpu-msr-contract.toml` / `CPU-MSR-CONTRACT.md`
  (port-IO, host-captured, **carries no per-host input** so it stays out of the §6 hash — confirm
  `contract_hash` unchanged).

## Part 2 — un-no-op the payloads' `report()`

`guest/payloads/common/src/report.rs`: make `report(u64)` emit the two `OUT 0x0CA2` writes on the
real lane (still a no-op under QEMU shape-testing, which has no host capturing the port — keep the
Part-A serial gate byte-identical). The payloads already call `report(..)` at the right points
(#49) — this just gives those calls a live transport on the box.

## Part 3 — the `Machine` bridge (`consonance/vmm-core`)

Provide a `unison::Machine` backed by the VMM running a payload through `boot_selected(Patched, …)`
(the frontier glue the dissonance ruling places in vmm-core). It must:
- `run_to(work)` / `state_hash()` per the `Machine` contract (reuse the existing V-time/`state_hash`
  from #45);
- override **`observable_digest()`** to hash the **report stream** (+ the serial banner) — *not*
  `state_hash` (the O2/O3 distinction, per #48): the report stream is the guest-observable
  conformance output;
- be constructible from a corpus item's payload (load the built payload ELF, map it, run).
- `cfg(target_os = "linux")` + box-only live path; the pure logic (stream digest, the Machine
  contract) unit-tested on macOS via a mock that scripts report-port writes.

## Part 4 — O2 digest goldens + manifest

Run each conformance-bearing payload on the box (patched modules loaded), capture its
`observable_digest` (the report-stream digest), and commit it under `guest/golden/` as the **64-hex
O2 golden**. Then re-add `"conformance"` to those items' `oracles` in `docs/corpus-manifest.toml`
(#49 omitted it pending these goldens) with the `golden = ...` digest path. `det-corpus validate`
must still pass; `det-corpus run` must now exercise O2.

## Part 5 — the box gate (the proof point)

On the box (patched modules loaded, `taskset` pinned, then **reverted to stock**): `det-corpus run`
over the C1 manifest with the VMM-backed `Machine` must report **O1 (determinism) + O2 (conformance)
PASS for every item, deterministic-twice** (identical aggregate across two runs). Paste the run into
`IMPLEMENTATION.md`. This is the gate the whole task exists for; foreman re-runs it on the box at
review (the proxy patched modules are at `<box>/kvm-spike/deb612/.../kvm{,-intel}.ko`).

## Gates

Mac: `build`/`nextest`/`clippy -D warnings`/`fmt` for `vmm-core` (the Machine bridge + report-port
dispatch, mock-tested) + `det-corpus` (unaffected) + the QEMU shape gate still green (report is a
no-op there). `contract_hash` unchanged (the report port carries no hashed input). The **live box
O1/O2 run is box-only** (evidence in IMPLEMENTATION.md). Cross-model pass — this is the
determinism/conformance corpus actually running, so the bar is high.

## Deliverables

The `REPORT_PORT` ABI (vmm-core dispatch + INTEGRATION.md + contract row, hash-unchanged); the live
`report()` on the box lane; the VMM-backed `det-corpus` `Machine` with report-stream
`observable_digest`; the O2 digest goldens + re-enabled `conformance` in the manifest; the **box
O1/O2 deterministic-twice proof** in IMPLEMENTATION.md. Box left on stock KVM.
