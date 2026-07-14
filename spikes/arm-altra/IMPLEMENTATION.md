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
- **`harness/`** — the KVM harness: the ioctl-level single-vCPU machine
  (`KVM_CREATE_VM` → memory slot → `KVM_CREATE_VCPU` → `KVM_RUN`), the measurement
  loop over it, the aarch64 opcode scanner (branch / exclusive / counter-read), a
  panic-free ELF reader/loader, the window verifier, the console decoder, the
  deterministic planner, the canonical evidence formats, and the Linux-only perf/KVM
  syscall seam. 63 native tests + the manifest generator test. Cross-compiles for
  `aarch64-unknown-linux-gnu`; `probe` genuinely issues `perf_event_open` and
  `KVM_CHECK_EXTENSION` on Linux.
- **`schemas/`** — the canonical evidence JSON schemas and the `floor-check` crate:
  recomputes every acceptance floor from retained per-sample records, with 1 accept
  + 17 reject fixtures, each asserting *which* check catches it. The checks are
  **stage-aware**: the stages that ride the patched force-exit must prove they did,
  the unpinned migration probe belongs to AA-1 alone, AA-5 must attest the
  harness-maintained clock page, and a floor nobody requested is reported as
  `NOT-REQUESTED` (nonzero RC), never as a pass.
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
| harness logic | `cd harness && cargo test` | 63 + manifest test pass |
| harness cross-build | `cargo check --target aarch64-unknown-linux-gnu --all-targets` | the syscall seam compiles for the box |
| harness under Miri | `cargo +nightly-2026-06-16 miri test -p arm-harness` | 63 pass, 1 ignored (the subprocess test) — the crate carries `unsafe` |
| floor checker | `cargo test -p floor-check` | 24 unit + 20 integration: accept + 17 rejects, each catches the right check |
| dependency policy | `cargo deny check` ×3 workspaces | advisories, bans, licenses, sources all ok |
| patch gate | `cd host && ./verify.sh` | applies + compiles; mechanism in objects |
| clippy / fmt | per crate | clean |
| **CI** | `.github/workflows/quality.yml` → `spike-arm-altra` | every gate above except the TCG smoke (no qemu on the runner; it stays the documented local gate) |

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
   Every ioctl the loop needs is written out and compiles for `aarch64-linux`; none
   has executed against a real `/dev/kvm` or a real PMU. What *is* checked
   pre-silicon is the part that can be: the `perf_event_attr` flag bits, the KVM
   ioctl numbers and the `kvm_run` field offsets are pinned to the kernel ABI by
   native unit tests — because a flag on the wrong bit does not fail loudly, it arms
   a *different event* (unpinned, host-inclusive) and reports the AA-0 row green.
5. **The `KVM_RUN` measurement loop exists but has never driven a vCPU.** The loop,
   the VM/memory/vCPU setup, the counter arming (both mechanisms), the state digest
   and the evidence writer are all here; arrival day *runs* them. The loop's
   decisions — mark decode, counter sampling, delivery multiplicity, skid, every
   fail-closed refusal — are driven natively against a scripted seam, so what a
   record *says* is tested pre-silicon; whether the ioctls behave as documented on N1
   is AA-1's.
6. **QEMU `-cpu neoverse-n1` under TCG is not N1 silicon.** `ident`'s self-report is
   representative in *shape* (the ID-register layout) but its values, and every
   counter fact, are the emulator's.

## Round-1 review fixes (PR #108)

The review's finding was that the defects were almost all of one species —
**instruments that can go green without measuring the thing** — which is the exact
pathology this apparatus exists to kill. Each fix below closes one, and the fix is in
every case *a check that did not exist*, not a comment saying it should.

| # | Finding | Fix |
|---|---|---|
| 1 | `perf_event_attr` flag bits were wrong: `FLAG_PINNED = 1<<3` actually set `exclusive`, `FLAG_EXCLUDE_HOST = 1<<9` actually set `comm`. The AA-0 PMU probe would have opened a **multiplexed, host-inclusive** counter and reported the row green. | Constants corrected to their kernel-ABI positions (`pinned=1<<2`, `exclude_host=1<<19`, plus `exclude_guest`/`exclude_hv`), and the whole ABI half of `sys` (flags, ioctl numbers, `kvm_run` offsets, `perf_event_attr` layout) hoisted into portable code and **pinned by native unit tests**. The manifest's `perf` block is now *derived from the attr that was armed* (`sys::perf_config`), so evidence cannot describe an arming that did not happen. |
| 2 | `arm-spike probe` exited **0** with mandatory AA-0 rows unprobed. | The RC is now the rule: any mandatory row *unprobed* ⇒ nonzero; an expect-present row absent ⇒ nonzero; the determinism cap absent stays OK (it is the one expect-*absent* row — a stock kernel does not have it). |
| 3 | The `KVM_RUN` measurement loop was absent — arrival day would have written code instead of running it. | Built: `sys::machine` (VM, memory slot, vCPU, `KVM_RUN`, `KVM_GET_REG_LIST`-based state digest, `PerfCounter` arming both mechanisms) behind the existing seam, `run::run_sample` (the loop) tested natively against a scripted vCPU, and `arm-spike run` to drive a plan and write a run-set. Wiring it un-stubbed both KVM-cap probes (they needed a VM fd). |
| 4 | The checker was **stage-blind** in five ways: self-selected mechanism tuples, `migration_probe` exempting pinning at any stage, a **vacuous rep floor** (`state_digest` was never compared — it appeared only in fixture data), unchecked `perf` and `clockpage_mode` surfaces. | Five new/tightened checks: `mechanism-attestation` now enforces the **stage tuple** (AA-3/AA-4/AA-6 must *be* on the patched exit — self-consistency is not attestation); `pinning` gates the migration probe to AA-1; `replay-identity` groups records by `(payload, scale, seed, condition, target)` and demands bit-identical digests (an empty digest is itself a failure); `perf-config` validates raw `0x21`/`exclude_host`/`!exclude_guest`/`pinned`/period-consistency; `clockpage-mode` requires AA-5 records to attest the harness-maintained page. Five new reject fixtures, one per mode. |
| 4b | `RESULT: PASS` over an overflow-bearing run-set with no floor requested read as full acceptance. | New `NOT-REQUESTED` status: the verdict names the missing floor and **exits nonzero** (`RESULT: INCOMPLETE`). The checker demands the *presence* of an explicit floor; it still never supplies one. |
| 5 | `elf.rs` panicked on untrusted input (`e_shoff = u64::MAX` → overflow), contradicting its own no-panic claim. | Every file-supplied offset now goes through `checked_add`; the repro is a test, with three siblings (absurd `e_phoff`, an overrunning section count, a huge `sh_offset`). |
| 6 | The scan surface was **section-headers-only**, so a stripped image (no section table — what real vendor kernels are) scanned vacuously clean and `arm-scan counter-reads` exited 0. For AA-5 the scan *is* the enforcement. | Program headers are parsed and executable `PT_LOAD` segments are the scan surface (sections remain the refinement when there are no segments); an image with **no executable surface is an error**, not a clean scan. Stripped-image and no-executable-surface fixtures pin both halves. |
| 7 | The truth-table schema omitted three mandatory AA-0 rows, including the two *existential* work-clock rows AA-1 rests on. | `perf-raw-0x21-pinned`, `host-overflow-delivers`, `writable-id-registers` added; `minItems` 10 → 13. |
| 8 | `cargo deny check` **failed** (wildcards vs versionless path deps) and **no CI job ran any of this**. | Path deps versioned; `cargo deny check` passes in all three spike workspaces. New `spike-arm-altra` job in `quality.yml`: fmt, clippy, tests, deny, the aarch64-linux cross-check, the payload build, the window-vs-oracle gate, and Miri. |

Accepted suggestions: the totality check now computes the missing-sample count
arithmetically (a corrupt `attempted: u64::MAX` fails closed instead of hanging);
`deny_unknown_fields` on every evidence shape (so the Rust loader enforces what the
schemas' `additionalProperties: false` promises — the real danger being a *misspelled*
optional field silently becoming `None`); the subprocess-spawning drift test is
`#[cfg_attr(miri, ignore)]`d.

On the fourth suggestion (`Weights` carries one global `window_offset` while AA-1's
acceptance speaks of *per-class* offsets) I took the reviewer's "make the stance
explicit" branch rather than generalizing, and the reasoning is now in the field's
doc: a free offset per class, fitted from one scale each, would absorb every ambiguity
weight into itself and make the solve **unidentifiable** — the over-determination that
gives `Solved::residual` its meaning would be gone, and the model would fit anything,
including a wrong answer. So the single offset is stated as a *falsifiable prediction*
(`solve` returns `InconsistentOffset` when the two zero-ambiguity classes disagree,
and a class-dependent offset the weights cannot absorb surfaces as a nonzero
residual), with the arrival-day escape hatch named: if N1 delivers stable but
class-dependent offsets, the field generalizes to a per-class **intercept** map solved
across the 1e6/1e7/1e8 scales — which is exactly why AA-1(a) sweeps scales
differentially. The silent middle was the only wrong option, and it is closed.

One correctness bug found while fixing the above, not in the review: the fixture
generator emitted `clockpage_mode: "materialized"`, which is **not a token any payload
can print** (`payloads/runtime/src/pvclock.rs` emits `managed` or `self-seeded`). The
new AA-5 check reads that field, so a fixture inventing a third token would have been
testing a string no guest can emit. Corrected to `managed`.

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
