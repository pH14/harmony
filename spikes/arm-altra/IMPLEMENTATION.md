# IMPLEMENTATION.md — task 109, the ARM pre-build apparatus

Bead `hm-2kj`. Branch `task/arm-prebuild-apparatus`. **Everything here is untested
on silicon** — apparatus for the `docs/ARM-ALTRA.md` spike, not the spike.

## What landed

The four directories + READMEs the task specifies, all under `spikes/arm-altra/`:

- **`oracle-model/`** — the analytical taken-branch oracle, shared `no_std`/`std`
  between the payloads and the host harness. Single definition of every payload
  parameter and every expected count; the four ambiguity weights (exception
  entry/return, SVC, WFI) are unknowns with no `Default`, solved from an
  over-determined measurement set. 17 unit tests + 2 TCG-observed accumulator pins.
- **`payloads/`** — the minimal aarch64 bare-metal runtime (boot shim, MMU, GICv3,
  PL011, params/pvclock pages, semihosting exit) and nine oracle payloads with
  hand-written counted bodies. `smoke.sh` boots each twice under
  `qemu-system-aarch64` (TCG), verifies windows against the model, diffs normalized
  console vs `golden/`, and propagates every RC.
- **`harness/`** — the KVM harness: aarch64 opcode scanner (branch / exclusive /
  counter-read), minimal ELF reader, window verifier, console decoder,
  deterministic planner, canonical evidence formats, and the Linux-only perf/KVM
  syscall seam. 26 native tests + the manifest generator test. Cross-builds to a
  real `aarch64-unknown-linux-gnu` ELF; `probe` genuinely issues `perf_event_open`
  on Linux.
- **`schemas/`** — the canonical evidence JSON schemas and the `floor-check` crate:
  recomputes every acceptance floor from retained per-sample records, with 1 accept
  + 12 reject fixtures, each asserting *which* check catches it.
- **`host/`** — the kvm/arm64 `KVM_EXIT_PREEMPT` patch draft (the 0004-analogue).
  `git am`-applies to pristine `linux-6.18.35` and compiles (`arch/arm64/kvm/` +
  `vmlinux` link), with the mechanism asserted in the built objects by `verify.sh`.

## Gates — all green

| Gate | Command | Result |
|---|---|---|
| oracle model | `cd oracle-model && cargo test --features std` | 17 + 2 pass |
| payloads build | `cd payloads && cargo build --release` | 9 payloads link (aarch64-unknown-none) |
| TCG smoke | `cd payloads && ./smoke.sh` | all 9 boot ×2, golden-match, RC-propagated (verified: tampered golden ⇒ nonzero) |
| window verify | `arm-scan windows …` | 8 windows match the model |
| harness logic | `cd harness && cargo test` | 26 + manifest test pass |
| harness cross-build | `cargo build --target aarch64-unknown-linux-gnu` | real ARM Linux ELF (built + run in an aarch64 container) |
| floor checker | `cd schemas/floor-check && cargo test` | accept + 12 rejects, each catches the right check |
| patch gate | `cd host && ./verify.sh` | applies + compiles; mechanism in objects |
| clippy / fmt | per crate | clean |

## Deviations considered and rejected

- **Reusing the x86 payload *code*.** Rejected per the task: the x86 payloads test
  the x86 contract. Only the host-derived-golden *pattern* is reused (a counted
  window bracketed by MMIO marks; a golden diff of structure). The bodies, the
  runtime, and the contract are new-by-purpose.
- **`WFI` on the generic timer for the idle payload.** Rejected: `WFI` may complete
  spuriously, so a timer-woken loop needs a wall-clock-dependent re-check whose
  back-edge falls inside the counting window and destroys the oracle. A
  self-directed SGI makes the interrupt pending before the `WFI`, so no spin is
  needed and the interrupt lands at an instruction fixed by construction. The cost
  (this payload no longer proves the vCPU truly blocks — a liveness property) is
  paid explicitly and re-homed to AA-5(c)'s Linux boot.
- **Inventing `skid_margin`, count offsets, or ambiguity weights.** Rejected hard —
  this is the task's central "no invented constants" rule. `Weights` has no
  `Default`; the manifest leaves `window_offset` as "measured-AA-1 (unknown
  pre-silicon)"; the floor checker *refuses to check counts* when weights are
  absent rather than falling back to a guess.
- **A result-total field in the run-set manifest.** Rejected: a checker that read
  "mismatches: 0" from a line the harness wrote about itself is the PR-98
  pathology. The manifest carries no totals; the checker derives everything from
  the records, whose sha256 the manifest pins.
- **`serde::Deserialize` on `Expectation`.** Rejected and made impossible: the type
  is serialize-only so nothing can read back a claimed expectation and believe it —
  consumers recompute it from `(payload, scale, seed)`. Evidence-integrity #2
  enforced by the compiler.
- **An off-the-shelf ELF crate for the scanner.** Rejected: the reader is on the
  trusted path of two acceptance gates and must not panic on a malformed kernel
  image; a hand-rolled, fully bounds-checked, `unsafe`-free reader is smaller and
  auditable.

## Known limitations / sim-vs-silicon gaps (what only silicon can close)

1. **No count is measured or validated here.** The TCG smoke proves liveness and
   protocol only. `BR_RETIRED` determinism, per-class offsets, the N1 `skid_margin`,
   the density table, PMI multiplicity, and skid are all stage AA-1's — the
   apparatus leaves them as explicit unknowns and provides the model + checker to
   test them against.
2. **The patch only applies + compiles.** It has never booted a host kernel or run
   a guest. The x86-NMI vs arm64-maskable-IRQ difference (an armed vCPU exits
   `KVM_EXIT_PREEMPT` on *any* host IRQ) is a named residual for AA-3; so is the
   precise-exit alternative (in-kernel `perf_event_create_kernel_counter` with a
   `preempt_pending` flag), which is flagged, not implemented.
3. **arm64 KVM is built-in (`CONFIG_KVM=y`), not a module.** No `kvm.ko` hot-swap
   like x86 — the patched kernel must be booted, so every AA-3 cycle costs a reboot.
4. **The perf/KVM syscall seam is Linux-only and has never run on the target PMU.**
   It compiles and its `perf_event_open` path executes on aarch64 Linux (returns
   EPERM in an unprivileged container — the syscall really fired), but the two
   KVM-cap probes (`GuestDebug`, `DeterministicIntercepts`) are stubbed as
   hard "cannot probe" (not faked) pending a real `KVM_CHECK_EXTENSION` on a VM fd.
5. **The `KVM_RUN` measurement loop is not wired to hardware.** By design: arming
   the counter, running to a window mark, sampling `BR_RETIRED`, and writing a
   `RunRecord` is AA-1's to drive on the box. The apparatus delivers everything
   around it — the plan, the evidence shapes, the scanner, the checker.
6. **QEMU `-cpu neoverse-n1` under TCG is not N1 silicon.** `ident`'s self-report is
   representative in *shape* (the ID-register layout) but its values, and every
   counter fact, are the emulator's.

## Notes for the integrator

- **`.gitignore` change (one line, root).** `spikes/*` was gitignored wholesale;
  `docs/ARM-ALTRA.md` §Repository layout and this task make `spikes/arm-altra/` a
  *tracked* apparatus location. Added `!spikes/arm-altra/` plus an in-directory
  `.gitignore` that keeps build/measurement outputs (`target/`, `results/**/raw/`)
  untracked. No other spike is affected.
- **Standalone workspaces.** `payloads/` (aarch64-unknown-none) and the top-level
  harness workspace are separate; `oracle-model` carries its own empty `[workspace]`
  table so both can path-depend on it. None joins the repo root workspace (the root
  globs only `consonance/*` and `dissonance/*`).
- **No production-crate code, no box, no Beads.** Zero file overlap with the seam
  restructure (`hm-b5n`) or the ARM backend (`hm-cbt`).
- **The container prereq for the patch gate** (`host/verify.sh`) is a native-aarch64
  Linux builder with the pinned tree; `host/BUILD.md` §0 documents the one-time
  setup. The gate was run green on such a builder during development.
