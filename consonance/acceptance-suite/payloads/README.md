<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Bare-metal acceptance payloads

These are small Multiboot-v1 payloads for the portable acceptance gate. Each
uses the shared boot, UART, exit, and IDT scaffolding, emits a stable serial
banner, and exits through the debug device. `run-tests.sh` builds every payload,
boots it twice under QEMU TCG, and compares the payload output with the
matching file in `../golden/`.

## Instruction and MSR corpus

The instruction-sweep payloads cover the selected trapped instruction and MSR
classes. The serial banner checks host-independent behavior. `common::report`
sends values that require the deterministic VMM over the dedicated report
channel; stock QEMU discards those writes, so the portable serial gate remains
independent of host CPU values. The corpus manifest assigns O1 determinism and
O3 seed sensitivity to each payload and assigns O2 conformance where the live
VMM can currently complete the workload.

| Payload | Instruction or MSR surface | Serial assertion | Reported value | O3 |
|---|---|---|---|---|
| `insn-rdtsc` | RDTSC, RDTSCP | Reads are monotonic | TSC readings and TSC_AUX | pure |
| `insn-rng` | RDRAND, RDSEED | Values eventually return successfully | Seeded RNG stream | rng-consuming |
| `insn-cpuid` | CPUID | Contract leaves are stable | Live registers and model match | pure |
| `insn-rdpmc` | RDPMC | Faults and resumes | Per-selector fault disposition | pure |
| `insn-hlt` | HLT | Halts and wakes at a timer deadline | Idle-skip markers | pure |
| `insn-mwait` | MONITOR, MWAIT, PAUSE | Executes or faults without hanging | Fault disposition | pure |
| `msr-allowed` | Allowed RDMSR/WRMSR | Every stateful MSR round-trips | Index and written value | pure |
| `msr-denied` | Unknown RDMSR/WRMSR | Default-deny accesses fault | Per-index disposition | pure |
| `irq-landing` | LAPIC timer | Each deadline delivers one IRQ | Representative deadlines | pure |
| `pit-pic-stub` | PIT, PIC | Deterministic ticks and port reads | Refresh bit and tick count | pure |

## Deliberate coverage boundaries

VMCALL belongs to the hypercall-doorbell tests. Uniform `#UD` instructions are
represented by the fault-catch payloads rather than enumerated individually.
Native-permitted instructions, XSETBV, named deny-`#GP` MSRs, and the full
device register files are outside this corpus. The TSC MSR semantics are
exercised through `insn-rdtsc`; direct emulated-time MSR checks remain a
hardware-gate concern.

The `msr-allowed` payload sweeps the generated allow-stateful set rather than a
hand-maintained sample. Its host-side contract-data tests keep that set aligned
with the machine-readable CPU contract and validate the write values.

## Adding a payload

Add a package to this standalone workspace, use the shared `common` helpers,
register its serial golden in `../golden/`, and add it to `run-tests.sh`. Add a
manifest entry with only the oracle classes the payload exercises. Keep serial
output host-independent; send deterministic-VMM values through `report`.
