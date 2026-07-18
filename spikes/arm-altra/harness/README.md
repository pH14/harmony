# `harness/` — the host-side KVM harness (scaffolding, untested on silicon)

**UNTESTED ON SILICON.** The pure-logic pieces are tested natively; the perf/KVM
syscall seam compiles and its `perf_event_open` path runs on aarch64 Linux, but the
whole crate has **never run against a real N1 PMU**.

The minimal ioctl-level KVM harness (single vCPU, pinned, raw `BR_RETIRED` armed
guest-only): `KVM_CREATE_VM` → memory slot → `KVM_CREATE_VCPU` → the `KVM_RUN` loop
that decodes the two window-mark MMIO exits, samples the counter, digests the landed
state, and assembles a `RunRecord`. Almost all of it is pure logic — no syscalls, no
`unsafe` — and is tested on the development Mac (itself aarch64, so the opcode
fixtures are the real target ISA).

The loop is pure logic because it programs against two narrow seams (`Vcpu`,
`WorkCounter`) rather than against ioctls: `run::run_sample` is driven natively by a
scripted vCPU, and `sys::machine` implements the same two traits with real `KVM_RUN`
and `perf_event_open` on Linux. So the part that decides *what a record says* — mark
decode, counter sampling, delivery multiplicity, skid, the fail-closed refusals — is
tested here, pre-silicon; only the part that issues the syscall is not.

## Modules

| Module | Role | Tested |
|---|---|---|
| `scan` | aarch64 opcode decoder: branches (→ window verification), LL/SC exclusives (AA-4 level 2), raw counter reads (AA-5 closure) | native |
| `elf` | minimal, panic-free ELF64 reader — finds window/handler symbols and their bytes | native |
| `verify` | decodes each payload's window from the built ELF and asserts its branch sequence equals the oracle model (makes "known by construction" checked) | native |
| `console` | PL011 decoder: window marks, protocol lines, the exit-status sentinel | native |
| `plan` | deterministic, seeded run planning (a run-set is a pure function of its spec) | native |
| `evidence` | the canonical run-set / run-record formats (stable JSON, no result totals to believe) + the run-set assembler | native |
| `run` | the `KVM_RUN` measurement loop over the `Vcpu`/`WorkCounter` seams: window marks → counter samples → `RunRecord`. Every way to *not* measure is an error, never a record with a plausible zero | native (scripted seam) |
| `sys` | the perf/KVM syscall seam — the crate's only `unsafe`, Linux-only. Its **ABI half** (perf flag bits, ioctl numbers, `kvm_run` offsets) is portable data and is unit-tested natively | ABI: native. Syscalls: compile for aarch64-linux; **never run** |

The crate is `#![deny(unsafe_code)]`; only `sys` opts back in, so the scanner, ELF
reader, console decoder, planner and evidence writer are provably `unsafe`-free.
Keeping the syscall layer thin is what lets the layers above attest the mechanism
honestly — a silent stock-vs-patched fallback cannot masquerade as the mechanism
under test (`docs/ARM-ALTRA.md` §Evidence integrity #4).

## Binaries

- **`arm-scan`** — the offline gates: `windows <dir>` (verify every payload against
  the model), `exclusives <img>` (AA-4 LSE-only scan), `counter-reads <img>` (AA-5
  closure scan), `manifest` (emit the expected-count manifest). RC is the gate.
- **`arm-spike`** — `plan` (emit a deterministic run plan as JSON, off the box),
  `probe` (issue the AA-0 perf/KVM capability probes; **exits nonzero if any
  mandatory row is absent *or unprobed*** — a disposition may never rest on a probe
  that could not run), and `run` (the measurement loop: create the VM, publish the
  params page, run each planned sample, write `run-set.json` + `records.jsonl`).
  `linux-boot --stage2-exec-guard` opts the owned Linux VM into AA-4's default-XN
  mediation and requires nonzero execute/scan/approval counts. `aa4-guard-reject`
  hash-verifies a planted-exclusive ELF and succeeds only after an exact-generation
  reject leaves its PC in the rejected page. `aa4-guard-write` pins the dedicated
  self-modifying ELF and requires the original page at the synchronous pre-store exit,
  then the exact expected replacement page at a fresh scan generation. Linux/box only
  for all syscall paths.

The AA-4 commands are proof apparatus, not evidence by their existence. They have not run on the
patched N1. TCG proves only that the self-modifier completes and its protocol is stable. Live
planted rejection, write-before-store/rescan, stale-generation, notifier replacement, and
two-vCPU scan/write-race tests remain required.

## Build / test

```sh
cargo test                                        # 63 logic tests + the manifest generator test
cargo check --target aarch64-unknown-linux-gnu    # the box binary (perf/KVM paths compile for Linux)
cargo run --bin arm-scan -- windows ../payloads/target/aarch64-unknown-none/release

# The crate carries `unsafe` (the sys seam), so the repo's unsafe⇒Miri bar applies:
MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p arm-harness
```

What arrival day still has to do: **run** it. The loop, the VM setup, the counter
arming, the state digest and the evidence writer all exist and compile for the box;
none of them has ever touched a real PMU, a real `/dev/kvm`, or an N1. Every constant
they *measure* (count offsets, skid margin, event density) is a stage deliverable and
is treated here as an unknown — never a default.
