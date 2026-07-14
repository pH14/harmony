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

## Round-2 review fixes (PR #108, cross-model pass)

A blind GPT-5.6 pass over the round-1 head found ~21 issues, almost all in the *new*
`sys/machine.rs` + `run.rs` + `arm-spike run` plumbing round 1 added. Every one was
verified against the code before fixing; all held. They cluster:

**KVM/perf ABI — the harness could not have booted or armed anything.**

- **PC set to the wrong register.** `KVM_REG_ARM_CORE_REG(regs.pc)` is `0x100/4 = 0x40`;
  the code used `0x44`, which names `sp_el1`. Every launch wrote the EL1 stack pointer
  and left `PC` at reset — the guest never entered the payload. The constant is now
  *derived* from the field offset and pinned by a test.
- **No vGICv3.** The payload runtime programs the GIC distributor at `0x0800_0000`
  before it prints a byte; with no in-kernel vGIC those are MMIO exits the loop
  refuses. `Machine::new` now issues `KVM_CREATE_DEVICE` + the dist/redist addresses +
  `CTRL_INIT` at the addresses `gic.rs` expects. Nothing boots without it.
- **Deterministic-intercepts cap advertised but never enabled.** The patch gates
  `KVM_ARM_PREEMPT_EXIT` on a per-VM flag only `KVM_ENABLE_CAP` sets; the code only
  *checked* the cap was advertised, so every arm would `EINVAL` on the patched kernel.
  `enable_deterministic_intercepts` now issues the enable for the patched mechanism.
- **`PERF_EVENT_IOC_PERIOD` passed by value.** It is an `_IOW` taking a `*u64`; the
  value was passed directly, so the kernel read the deadline as an address and returned
  `EFAULT` — no overflow ever armed. Now passed by pointer.

**The counting-window + rearm contract — armed records would have been wrong.**

- **Period live before `MARK_BEGIN`.** The event was opened with `sample_period` set and
  enabled at construction, so a small delta overflowed during boot and the kick arrived
  unarmed. The fd now opens in *counting* mode; the period is programmed at
  `arm_overflow`, which the loop calls at the mark. The manifest still reports the
  intended sampling config, derived from a reporting attr.
- **Advisory exits counted as deliveries.** The patch's own arch note: on arm64 the PMU
  overflow is a maskable IRQ, so the armed vCPU exits on *any* host IRQ and every
  `KVM_EXIT_PREEMPT` is **advisory** — re-read the counter, re-arm if the target was not
  reached. The loop now reads the counter at each mechanism exit: `work < target` is an
  advisory exit (recorded in the new `advisory_exits` field, re-armed, re-entered), only
  `work >= target` is a delivery. An early timer tick can no longer masquerade as an
  exactly-once PMI. A no-progress storm is bounded and refused rather than spun on.
- **Counter frozen after the one-shot.** `REFRESH(1)` disables the event on overflow, so
  `work_end` would read the landing, not the window's end, and every armed count would
  disagree with the whole-window oracle. `resume_counting` re-enables with an
  out-of-reach period after the landing.
- **Landing digest taken at the wrong Moment.** The digest was sampled at the exit
  sentinel, where two different landed states can converge. AA-3's replay identity is
  about the state *at the landing*, so the loop now captures a `landed_digest` there,
  before resuming, and the checker's replay-identity compares that for armed records.
- **Scale hard-coded to Smoke.** `arm-spike run` offered no scale override, so the AA-1
  1e6/1e7/1e8 differential sweep — the whole existential measurement — could not be run.
  Added `--scale` (repeatable) threaded through the plan.

**Evidence integrity.**

- **Failed attempts vanished.** A sample error `?`-returned before any evidence was
  written, so neither the failure nor the prior attempts reached the totality checker —
  a reliability failure that disappears on rerun. `arm-spike run` now writes the partial
  run-set (with `attempted` = full plan) before surfacing the error; the gap is in the
  evidence, which is what totality catches.
- **One ELF booted under every label.** `--payload` was one file booted for all eight
  classes while the record label changed — mislabeled evidence. Replaced with
  `--payload-dir`; each sample loads the ELF matching *its* payload, each content-pinned.
- **Repetitions re-drew their seed.** `reps` advanced the RNG per repetition, so no two
  records shared a `(payload, scale, seed, condition, target)` key — the round-1
  replay-identity check found nothing to compare and passed, and `--min-reps` counted
  rows. AA-6 could go green without comparing a single same-seed pair. The matrix is now
  drawn once, above the rep loop; a repetition repeats the input.
- **`solve()`'s fractional truncation.** The SVC and WFI weights were computed by integer
  division that silently truncated a remainder, and the residual was recomputed from the
  SVC row alone (which reproduces itself), so `solve` could return `Ok` with residual 0
  while its weights did not reproduce the WFI measurement — hiding the unexplained count
  mismatch the program calls blocking. Division is now exact (`NonIntegralWeight` on any
  remainder) and the residual is the worst over *every* supplied observation.
- **Host kernel never content-verified.** `host_kernel_sha256` was an operator-typed
  string the checker only checked was nonempty. Replaced with `--host-kernel-image`: the
  image is read and hashed, and that hash is both the mechanism identity and a verified
  image pin — so §Evidence integrity #3, which names host kernels, actually covers it.
- **Truth-table expectations unconstrained.** A mandatory row could claim
  `expected: absent, found: absent, confirmed: true` for an existential capability,
  hiding the failure. The schema now pins the normative `expected` per row id, requires a
  confirmed row's `found` to match it, and requires an unconfirmed row's to actually
  differ. Verified against a Draft 2020-12 validator: the exact evasion is rejected.
- **Condition hard-coded in the plan** while the manifest used `--condition` — the two
  could disagree about which experiment ran. `--condition` now threads into every sample.

**Gates / quality.**

- **Miri coverage for the new unsafe.** The memory-safety-critical payload-image copy
  (including the `p_filesz > p_memsz` OOB the review found separately) is factored out of
  the Linux-only KVM harness into `elf::Elf::load_into` — safe code over a `&mut [u8]`
  that Miri drives against an in-process buffer. The mmap/ioctl paths stay
  `cfg(target_os="linux")`. The bare-metal `runtime`/`oracles` crates are documented as
  the asm/privileged-class Miri exception (no_std, inline asm, physical MMIO — the
  interpreter cannot model them; the TCG smoke exercises them instead).
- **`CPU_SET` OOB panic.** `--core ≥ CPU_SETSIZE` panicked libc's `CPU_SET` on CLI input;
  now bounded with a clean error.
- **SPDX headers** added to all 38 new Rust files, the 10 `.s` files, and `host/verify.sh`
  (the repo's `AGENTS.md`-mandated header; 346/346 first-party `.rs` files carry it).
- **Fixtures validated against the JSON Schemas.** A new `schema_conformance` test
  structurally validates every fixture manifest and record against the committed
  `run-set`/`run-record` schemas (a dependency-free Draft-2020-12 subset validator) and
  asserts the schema's pinned `schema_version` equals the Rust constant — which
  immediately caught that the schemas had drifted (still v1, missing the two new overflow
  fields). Schemas updated to v2.

The evidence schema is now `SCHEMA_VERSION = 2` (added `advisory_exits`, `landed_digest`).

## Round-3 review fixes (PR #108, converging)

A smaller cross-model set, all verified against the code first.

- **The KVM loop could not boot a real payload.** A booting guest writes the PL011
  config registers (CR/IBRD/FBRD/LCR_H) and *reads* the flag register before it can
  print; with no in-kernel PL011 those are all MMIO exits, and the round-2 loop
  accepted only DR writes, rejecting the very first `runtime_init` write. The loop now
  models the PL011: config-register writes are accepted no-ops, and an FR read is
  answered "ready" (TXFF/BUSY clear, so the guest's polls are single-pass, exactly as
  QEMU's FIFO-disabled model — and those polls sit outside the counting window, so no
  counted branch is touched). This needed a `complete_mmio_read` seam on `Vcpu` (the
  KVM MMIO-read protocol: stage the value into `kvm_run.mmio.data` and re-enter).
- **The skid rules rejected the evidence AA-1 exists to collect.** AA-1(c) *measures*
  the early/late skid distribution to derive the margin, so a landing at `target + 1`
  is a datum there, not a violation; the no-overshoot/within-margin/exact rules are the
  patched *landing contract* (AA-3/AA-4/AA-6). `check_skid` is now stage-and-mechanism
  aware: at AA-1 it enforces only that the recorded skid is self-consistent; the
  contract binds on the patched-mechanism stages. Fixtures both ways —
  `accept-aa1-skid` (early/late landings accepted) and `reject-overshoot`/
  `reject-skid-exceeds-margin` moved to AA-4 where the contract binds.
- **The AA-6 rep floor counted total rows.** `--reps 125` over an eight-payload matrix
  is 1,000 records but only 125 reps of each input, and `--min-reps 1000` passed —
  though no input was repeated 1,000 times. The floor is now the count of the
  *least-repeated* distinct `(payload, scale, seed, condition, target)` input; every
  group must meet it. Fixtures: `accept-aa6-gate` (the same input repeated,
  bit-identical) and `reject-aa6-rep-floor` (total meets the floor, per-input does not).
- **Artifacts were hashed but not verified against trusted identities.** `arm-spike run`
  hashed whatever bytes were present and asserted `verified_before_boot: true`. It now
  takes `--payload-pins` (a JSON map of trusted expected sha256 per payload) and
  `--host-kernel-sha256`, hashes each loaded artifact, and compares: a mismatch is a
  hard error, and only a match attests verification. A swapped or rebuilt artifact can
  no longer receive a fresh accepted identity.
- **Emitted md5 pins were schema-invalid.** Every real run-set wrote an empty `md5`,
  which violates the schema's `^[0-9a-f]{32}$`. No md5 implementation is on the
  whitelist and sha256 is the identity, so `ImagePin::md5` is now `Option<String>`
  (nullable in the schema), and the harness emits `None` — a canonical, schema-valid
  manifest.
- **Miri had no coverage of the machine-layer pointer logic.** The KVM harness is
  Linux-only and the interpreter runs on the Mac, so its ioctls are the documented
  asm/privileged exception — but the pure pointer logic they hand off to is not. The
  `kvm_run` decode, the MMIO-read staging, and the state-digest hashing are factored
  into portable `sys` functions (`decode_kvm_run`, `stage_mmio_read`, `digest_state`)
  driven under Miri against an in-process `KvmRun`; `machine` forms the references from
  its mapped pointer and calls straight through. Miri now runs 76 tests (up from 72).

P2s: the manifest's `sample_period` is per-sample (each AA-3 cell draws its own
`target_delta`), so it is derived from the records — `Some(p)` only when every armed
record shares one uniform period, else `None` (the per-sample truth is each record's
`target - work_begin`), and `check_perf` cross-checks the uniform claim. The
schema-conformance validator now enforces `pattern`/`minimum`/`minLength` (and panics on
any pattern it does not recognise, so a new one can't slip in unchecked). `KVM_GET_REG_LIST`'s
host-supplied count is bounded (`checked_add`, a 65 536 sanity cap) before allocating.
`smoke.sh` resolves `timeout`→`gtimeout` so the Mac-local gate does not exit 127 on a
stock Homebrew setup.

## Round-4 review fixes (PR #108, closing round)

A narrow, mechanical set — all implemented (none needed rebuttal; the one option-shaped
finding, Miri for the payload crates, was resolved via the reviewer-sanctioned per-crate
documentation).

- **Live timer registers in the state digest** — the flagship. `KVM_GET_REG_LIST`
  includes the generic-timer *counters* (`CNTVCT_EL0`, `CNTPCT_EL0`, their `…SS`
  variants, `KVM_REG_ARM_TIMER_CNT`), whose value advances with elapsed host time, so
  hashing them would make two same-seed runs digest differently the moment scheduling
  differs — replay identity dead on arrival day. `digest_state` now excludes them via a
  portable, Miri-tested `is_host_time_register` (they are the arm64 sysreg coordinates
  `op0=3, op1=3, CRn=14`; the deterministic controls/comparators/CNTFRQ are kept). The
  fixture (a sys.rs test) proves two runs differing only in the live counter digest
  identically, while a real pc difference still diverges.
- **The PMU probe passed without scheduling the event** — it opened a *disabled*
  descriptor and closed it, so a pinned event that cannot actually be placed on the PMU
  reported the AA-0 row green. It now enables the event, runs a little branch work, and
  reads it back with `TOTAL_TIME_ENABLED`/`RUNNING`: green only if the counter advanced
  and ran for the whole enabled window (`enabled == running`, non-multiplexed).
- **A stray external `SIGUSR1` could be counted as a stock delivery.** The handler is
  now `SA_SIGINFO` and classifies the source by `si_code` — a perf-fd `O_ASYNC` signal
  carries a `POLL_*` code, a `kill()` carries `SI_USER`. `Machine::run` counts a
  `SignalKick` only for a perf-sourced kick and re-enters the guest on a foreign signal,
  so an injected signal cannot certify a delivery the counter never made.
- **Runs failing before the first counter opened lost their evidence.** The write was
  gated on an armed attr, so a failure in `Machine::new` / the patch probe /
  `PerfCounter::open` on the first sample wrote nothing and the totality gap vanished on
  rerun. Evidence is now written unconditionally (the intended counting-mode perf config
  when nothing armed), and an empty plan (`--reps 0`) is rejected outright.
- **Repetitions grouped by absolute target split same-input runs.** The plan reuses one
  target *delta*, but the stored target is `work_begin + delta`; a divergent `work_begin`
  gave different absolute targets and replay-identity reported "no group." The
  repetition key is now `target - work_begin` (checked), and a `target < work_begin`
  (negative delta) is flagged as malformed rather than producing a phantom group.
- **`pinned: true` with `core: null` passed.** The recorded core is required evidence
  for the rr #3607 migration condition; an unrecorded core is now a pinning failure
  (fixture `reject-pinned-no-core`).
- **Schema-invalid evidence passed at load.** serde checks Rust types and
  `deny_unknown_fields`, not the schema's `pattern`/`minLength`/`minimum` — so a manifest
  with `sha256: ""` loaded and could pass every semantic check. A new `well-formed`
  check enforces the load-bearing constraints (hash formats, non-empty required strings,
  the sampling-period minimum) at grade time (fixture `reject-malformed-hash`, which the
  schema-conformance test also confirms is genuinely schema-invalid).
- **Miri for the payload crates** — documented per-crate rather than gated, because the
  limit is intrinsic: `runtime`/`oracles` are `no_std`/`aarch64-unknown-none` and every
  unsafe op is inline `asm!` or physical-address MMIO the interpreter cannot execute or
  model, with no non-privileged logic left to seam. The runtime crate doc and the CI
  comment now spell that out per-crate; the Miri-checkable pointer logic was already
  factored into `arm-harness`.
- **P2:** the governor is read from the *pinned* core's sysfs path (`cpu{core}`), not
  CPU 0's.

Fixtures: 24 (four accept, twenty reject). Miri now runs 78 tests (was 76).

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
