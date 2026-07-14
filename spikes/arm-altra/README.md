# `spikes/arm-altra/` — ARM vendor spike apparatus (offline, pre-silicon)

> ## ⚠️ UNTESTED ON SILICON
> Every artifact in this tree has been built, and where possible booted under
> emulation, on a development Mac and in aarch64 Linux containers. **None of it has
> run on a Neoverse N1, or any ARM hardware PMU.** It is the *apparatus* for the ARM
> vendor feasibility spike (`docs/ARM-ALTRA.md`), not the spike: it produces no
> measurements, no dispositions, and no evidence manifests, and it never touches a
> box. It exists so that the day an Ampere Altra arrives is spent **measuring, not
> scaffolding** (`docs/ARM-ALTRA.md` §Immediate focus).

This is the deliverable of **task 109** (bead `hm-2kj`), authorized by the pre-build
ruling (`docs/ARCH-BOUNDARY.md` §Pre-build ruling): build the offline apparatus now;
the AA-1 spike on real silicon later decides whether the measured constants and this
mechanism are *trusted and kept*, not whether they may be built.

## Layout

```
spikes/arm-altra/
├── README.md          # this file
├── oracle-model/      # the analytical taken-branch oracle (shared, host + target)
├── payloads/          # the aarch64 bare-metal runtime + 9 oracle payloads + TCG smoke
├── harness/           # the KVM harness: scanner, ELF/window verifier, planner,
│                      #   evidence formats, perf/KVM syscall seam (Linux-only)
├── schemas/           # canonical evidence formats + the floor-checker + fixtures
└── host/              # the kvm/arm64 KVM_EXIT_PREEMPT patch draft + build/verify gate
```

Each directory has its own README with the details; this one is the map and the
command index.

## Toolchain setup

```sh
# Rust targets
rustup target add aarch64-unknown-none              # the payloads (no_std, bare metal)
rustup target add aarch64-unknown-linux-gnu         # the harness cross-build target (the box)

# QEMU (the TCG smoke's slow oracle)
brew install qemu                                   # macOS
# apt-get install qemu-system-arm                   # Linux

# The kernel-patch gate needs a native-aarch64 Linux builder with the pinned
# linux-6.18.35 tree; host/BUILD.md §0 has the one-time container setup.
```

This apparatus was developed on `aarch64-apple-darwin`, which is itself aarch64 —
so the harness's pure-logic tests and the oracle model run **natively**, and the
opcode fixtures are the real target ISA.

## Every build / smoke / gate command

| What | Command | What it proves |
|---|---|---|
| Oracle model | `cd oracle-model && cargo test --features std` | the derivation is self-consistent; the TCG-observed accumulators match the model |
| Payloads build | `cd payloads && cargo build --release` | nine payloads link for `aarch64-unknown-none` |
| **TCG smoke** | `cd payloads && ./smoke.sh` | every payload boots under `qemu-system-aarch64`, round-trips its protocol, matches golden structure — **liveness and protocol only**, with RC propagation |
| Window verification | `cd harness && cargo run --bin arm-scan -- windows ../payloads/target/aarch64-unknown-none/release` | every payload's window branches match the oracle model (makes "known by construction" checked) |
| Harness logic | `cd harness && cargo test` | scanner, ELF reader, console, planner, evidence, **and the `KVM_RUN` loop** (driven against a scripted seam) — all pure-logic, tested natively |
| Harness cross-build | `cd harness && cargo check --target aarch64-unknown-linux-gnu` | the box binary compiles (the perf/KVM syscall paths build for Linux; linking needs the container — see `host/BUILD.md`) |
| Harness under Miri | `MIRIFLAGS=-Zmiri-permissive-provenance cargo +nightly-2026-06-16 miri test -p arm-harness` | the crate carries `unsafe` (the syscall seam), so the repo's unsafe⇒Miri bar applies |
| Expected-count manifest | `cd harness && cargo run --bin arm-scan -- manifest` | regenerates `payloads/expected/expected-counts.json` (kept current by a generator test) |
| **Floor checker** | `cargo test -p floor-check` | every acceptance floor is recomputed from records; 17 reject fixtures each fail the *right* check |
| Dependency policy | `cargo deny check` (and in `payloads/`, `oracle-model/`) | advisories, bans, licenses, sources — all three spike workspaces |
| **Patch gate** | `cd host && ./verify.sh` | the kvm/arm64 patch applies to pristine `linux-6.18.35` and compiles, with the mechanism asserted in the built objects |

## What is validated here vs. what only silicon can say

| Claim | Validated **here** (offline) | Only **silicon** (stage) can say |
|---|---|---|
| A payload's taken-branch count is known by construction | ✅ the window's branch sequence is decoded from the built ELF and matched to the oracle model (`arm-scan windows`) | — |
| The branch predicates + PRNG are correct | ✅ the executed asm's accumulator matches the model bit-for-bit under TCG | — |
| The runtime boots (MMU, GICv3, PL011, exceptions) | ✅ on the *emulated* N1 under TCG | that it boots on real N1 (AA-0) |
| `BR_RETIRED` counting is bit-deterministic on a pinned core | ❌ | **AA-1** — the existential measurement |
| Per-class count offsets, the N1 `skid_margin`, the density table | ❌ (left as explicit unknowns everywhere) | **AA-1** — the constants pack |
| Overflow PMIs arrive exactly once out of `KVM_RUN` | ❌ — the loop *counts* deliveries per record and the checker demands exactly 1 | **AA-1** (multiplicity) |
| The `KVM_RUN` loop assembles an honest record | ✅ the loop's decisions (mark decode, counter sampling, delivery counting, skid, every fail-closed refusal) are driven natively against a scripted vCPU | that the ioctls behave as documented on a real N1 (**AA-1**) |
| The perf event armed is the work clock | ✅ the `perf_event_attr` flag bits are pinned to their kernel-ABI positions by test, and the manifest's `perf` block is *derived from the attr that was armed* | that raw `0x21` opens pinned + guest-only on N1 (**AA-0**) |
| Single-step lands exactly one instruction | ❌ | **AA-2** |
| The patch converts overflow → deterministic exit; exact landing | ❌ — the patch only *applies + compiles* | **AA-3** |
| LSE-only guest holds count-determinism under injection | ❌ — the a/b payloads *exist* and the scanner enforces LSE-only statically | **AA-4** |
| The owned guest lives on work-derived time; raw-counter closure | ❌ — the `clock-page` payload + counter-read scanner *exist* | **AA-5** |
| Guest CPU contract freezable; vGIC round-trips | ❌ | **AA-6** |

The floor checker's verdict — never any harness's done-marker — is what a stage
disposition may rest on (`docs/ARM-ALTRA.md` §Evidence integrity). The apparatus is
built so that when the numbers exist, they are checked against a model that was
frozen before the numbers were seen.

## Evidence integrity is baked in, not bolted on

The six countermeasures of `docs/ARM-ALTRA.md` §Evidence integrity (the PR-98
lesson) are structural properties of this apparatus, not review checklists:

1. **Gate-RC propagation** — `smoke.sh`, `verify.sh`, `arm-scan` and `floor-check`
   all exit nonzero on any constituent failure; there is no done-marker success
   path. (Verified: a tampered golden fails `smoke.sh` nonzero.)
2. **Machine-checked floors** — the floor checker recomputes every floor from the
   raw records; the run-set manifest deliberately carries *no* result totals to
   believe.
3. **Content-hash-verified boots** — the evidence schema makes `verified_before_boot`
   a required field the checker enforces; a recorded-but-unverified hash fails.
4. **Mechanism attestation** — the patched mechanism cannot be silently downgraded:
   `PerfCounter::open` *refuses* to arm the patched exit on a kernel that does not
   advertise the capability (there is no code path from the request to the stock
   kick), the stages that ride the patched force-exit (AA-3/AA-4/AA-6) must declare
   and prove it, and the checker rejects any run whose per-record exit
   reasons do not match the mechanism the manifest claims (the stock-vs-patched
   masquerade that PR-98 caught).
5. **Independent oracle** — counts are judged against the analytical oracle model,
   never PMU-vs-PMU.
6. **Multiplicity + totality** — the checker establishes exactly-once from
   per-record multiplicity and accounts for every attempted sample; a missing
   sample is a failure, not a pass.

## Scope

Apparatus only. No production-crate code (the seam restructure `hm-b5n` and the ARM
backend `hm-cbt` are separate beads with no file overlap). No box access, no SSH, no
Beads-DB. Lands via a normal task PR; the spike-*execution* branch discipline in
`docs/ARM-ALTRA.md` governs the future hardware run, not this task.
