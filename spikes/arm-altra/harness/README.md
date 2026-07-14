# `harness/` — the host-side KVM harness (scaffolding, untested on silicon)

**UNTESTED ON SILICON.** The pure-logic pieces are tested natively; the perf/KVM
syscall seam compiles and its `perf_event_open` path runs on aarch64 Linux, but the
whole crate has **never run against a real N1 PMU**.

The minimal ioctl-level KVM harness (single vCPU, pinned, raw `BR_RETIRED` armed
guest-only) plus everything around the `KVM_RUN` loop. Almost all of it is pure
logic — no syscalls, no `unsafe` — and is tested on the development Mac (itself
aarch64, so the opcode fixtures are the real target ISA).

## Modules

| Module | Role | Tested |
|---|---|---|
| `scan` | aarch64 opcode decoder: branches (→ window verification), LL/SC exclusives (AA-4 level 2), raw counter reads (AA-5 closure) | native |
| `elf` | minimal, panic-free ELF64 reader — finds window/handler symbols and their bytes | native |
| `verify` | decodes each payload's window from the built ELF and asserts its branch sequence equals the oracle model (makes "known by construction" checked) | native |
| `console` | PL011 decoder: window marks, protocol lines, the exit-status sentinel | native |
| `plan` | deterministic, seeded run planning (a run-set is a pure function of its spec) | native |
| `evidence` | the canonical run-set / run-record formats (stable JSON, no result totals to believe) | native |
| `sys` | the perf/KVM syscall seam — the crate's only `unsafe`, Linux-only | compiles + runs on Linux; **not** on the target PMU |

The crate is `#![deny(unsafe_code)]`; only `sys` opts back in, so the scanner, ELF
reader, console decoder, planner and evidence writer are provably `unsafe`-free.
Keeping the syscall layer thin is what lets the layers above attest the mechanism
honestly — a silent stock-vs-patched fallback cannot masquerade as the mechanism
under test (`docs/ARM-ALTRA.md` §Evidence integrity #4).

## Binaries

- **`arm-scan`** — the offline gates: `windows <dir>` (verify every payload against
  the model), `exclusives <img>` (AA-4 LSE-only scan), `counter-reads <img>` (AA-5
  closure scan), `manifest` (emit the expected-count manifest). RC is the gate.
- **`arm-spike`** — `plan` (emit a deterministic run plan as JSON, off the box) and
  `probe` (issue the AA-0 perf/KVM capability probes, Linux/box only).

## Build / test

```sh
cargo test                                        # 26 logic tests + the manifest generator test
cargo build --target aarch64-unknown-linux-gnu    # the box binary (perf/KVM paths compile for Linux)
cargo run --bin arm-scan -- windows ../payloads/target/aarch64-unknown-none/release
```

The `KVM_RUN` measurement loop itself (arm the counter, run to a window mark, sample
`BR_RETIRED`, write a `RunRecord`) is deliberately **not** wired to hardware here —
that is stage AA-1's to drive on the box. This crate delivers everything around it.
