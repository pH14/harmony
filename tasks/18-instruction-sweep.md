# Task 18 — instruction-sweep payloads (C1 corpus)

Read `tasks/00-CONVENTIONS.md` and `guest/README.md` (the Part-A payload pipeline) first, then
`docs/DETERMINISM-CORPUS.md` (§C1). Touch only `guest/payloads/` (new payloads + their entries
in `payloads/Cargo.toml` and `payloads/run-tests.sh`), `guest/golden/`, and append the new
items to `docs/corpus-manifest.toml`.

## Environment

Payloads build and run **twice byte-identically under stock QEMU** on macOS **and** Linux (the
existing Part-A gate). Capturing goldens for the trap-dependent payloads (RDTSC/RNG/timer)
additionally needs the **box** (`/dev/kvm` + patched KVM, task 16) once the VMM can load them;
until then those payloads are gate-tested for *shape* under QEMU/TCG (no panic, protocol-valid
output) and their determinism/conformance goldens are captured on the box and committed.

## Context

C1 is the deterministic, exhaustive corpus: **one tiny bare-metal payload per trapped
instruction / MSR class we've identified** (docs/RESEARCH.md §3.5; `docs/cpu-msr-contract.toml`; R1).
Each exercises its instruction many times and at boundaries, emits only protocol-valid,
timing-independent lines (`guest/README.md` Part-A rules), and registers as a `Micro` corpus
item so `det-corpus` (task 17) can drive O1/O2/O3 over it. These are the cheapest, highest-signal
items and the seed corpus for the fuzzer (task 19).

Follow the documented "Adding a new payload" flow exactly (`guest/README.md`). Reuse `common`
for the boot shim and `common::payload::{start, ok, pass, fail}`; put anything reusable
(e.g. a hypercall "report u64" helper) in `common`.

## The payloads

One payload per row. "Asserts" is what the payload checks in-guest and/or what the golden pins.
Where a value is contract-defined, the golden is derived from `docs/cpu-msr-contract.toml` /
`docs/fragments/cpuid-model.md`, not hand-typed.

| Payload | Instruction(s) | Asserts (O2 / in-guest) | O3 tag |
|---------|----------------|--------------------------|--------|
| `insn-rdtsc` | RDTSC, RDTSCP | strictly monotonic across N reads; deltas match the V-time formula; never a raw host TSC | pure (seed-independent) |
| `insn-rng` | RDRAND, RDSEED | values == contract PRNG stream for the seed; CF set per contract; stream advances | rng-consuming, control-flow-stable |
| `insn-cpuid` | CPUID | **every** frozen leaf/subleaf in `docs/fragments/cpuid-model.md`, exact EAX/EBX/ECX/EDX | pure |
| `insn-rdpmc` | RDPMC | trapped/denied per contract (#GP or contract value) | pure |
| `insn-hlt` | HLT | idle-skip: instruction count unchanged across the HLT; wake at the armed deadline | pure |
| `insn-mwait` | MONITOR/MWAIT, PAUSE | exit/no-op behavior per contract; no host-time leak | pure |
| `msr-allowed` | RDMSR/WRMSR (allowed set) | each allowed MSR reads its contract value; writes round-trip per contract | pure |
| `msr-denied` | RDMSR/WRMSR (unknown) | **default-deny**: every unknown MSR raises #GP (sample a spread of MSR numbers) | pure |
| `irq-landing` | LAPIC timer | **the hard core**: arm a timer in V-time; report "instructions retired before first IRQ" via hypercall. O1 compares it across runs; O2 pins it. Sweep deadlines on/around `skid_margin=128` (task 07) | pure |
| `pit-pic-stub` | PIT/PIC | deterministic boot-stub behavior per R1 | pure |

Notes:
- `irq-landing` is the payload most likely to expose a determinism bug — give it the most
  cases (multiple deadlines, including ±1 around `skid_margin`). It depends on the `lapic`
  crate (task 13) and the VMM's injection path, so its live goldens are box-captured.
- The contract-derived payloads (`insn-cpuid`, `msr-*`) should *generate* their expected
  values from the committed contract at build time where practical, so a contract bump
  (contract-v3) surfaces as a payload diff rather than a silent drift. See
  [[contract-v2-freeze-ratified]].
- Report machine-checkable values via the hypercall channel (a `report(u64)` helper in
  `common`), **not** as raw decimal in the serial banner — the banner stays
  protocol-clean (`PAYLOAD <name> START/PASS/FAIL`, no raw TSC/IRQ counts per Part-A rules).
  **QEMU-safe requirement (load-bearing):** the mandatory stock-QEMU Part-A gate runs these
  payloads with **no vmm-core VMCALL handler**, so a `VMCALL` on a bare Multiboot payload
  raises `#UD` and would fail the QEMU shape/golden run before the box oracle ever sees it.
  `report()` MUST therefore **degrade to a no-op when the hypercall channel is absent** —
  either a build cfg for the QEMU lane, or a runtime probe whose `#UD` is trapped to a
  no-op. Reported values are a **box-oracle-only** concern (the real VMM consumes them); the
  QEMU gate validates **only** the PASS/FAIL banner, which must hold with `report()`
  short-circuited out. (For `insn-rdtsc` / `irq-landing`, the QEMU run thus checks the
  serial shape only; the trap-dependent goldens are the box oracle's job.)

## Acceptance gates

Beyond the standard payload gates (`guest/README.md` Part-A: builds, runs **twice**
byte-identically under QEMU, golden committed, `make -C guest test-payloads` green):

1. **Coverage of the trap surface**: every row above has a payload; a checklist in
   `guest/payloads/README` (or this task's `IMPLEMENTATION.md`) maps each trapped
   instruction/MSR class in `docs/cpu-msr-contract.toml` to its payload, and names any
   deliberately-omitted item with a reason. Silent gaps are the failure mode to avoid.
2. **Manifest registration**: each payload is appended to `docs/corpus-manifest.toml` as a
   `Micro` item with its `oracles` (Determinism always; Conformance with a `golden`;
   SeedSensitivity with the O3 tag above) — and `det-corpus validate` (task 17) passes.
3. **Conformance derivation**: `insn-cpuid` and `msr-allowed` expected values trace to the
   committed contract (generated or test-asserted equal), not hand-entered constants.
4. **Box determinism gate** (when the VMM can load payloads): each payload passes O1
   (`det-corpus check_determinism`) on the patched-KVM backend; `irq-landing` passes across
   its full deadline sweep. Capture and commit goldens from the box; record the box commit
   in `IMPLEMENTATION.md`.
5. **Shape gate on Mac**: trap-dependent payloads (RDTSC/RNG/timer) that can't produce a
   meaningful golden under TCG still build and emit protocol-valid output under QEMU on macOS
   (no panic, banner correct) so the Part-A gate stays green cross-platform.

## Non-goals

The harness/oracles (task 17); the fuzzer (task 19); SQLite/real workloads (task 20); any
Linux-guest payload (gated on a guest OS + R3); performance/timing assertions (V-time is the only
clock — wall-clock timing is meaningless and forbidden in goldens).
