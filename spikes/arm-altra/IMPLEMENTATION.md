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

## Round-5 review fixes (PR #108, cross-model pass r5)

Ten P1s of the same evidence-integrity species — two hardware-ABI bugs the foreman
independently confirmed, two vacuous-pass paths the foreman flagged as blocking, and
five more of the same shape. Nine implemented; one (skid-aware exact landing) rebutted
on the PR as a deliberate AA-2/AA-3 arrival-day deferral.

- **The PMU probe could never see a nonzero count on real hardware.** `probe_br_retired`
  reused `br_retired_attr`, which sets `EXCLUDE_HOST` (correct for the guest measurement
  loop), but the probe's workload is a *host-userspace* loop with no guest — so a
  guest-only counter reads exactly zero and the mandatory AA-0 row always fails.
  The probe now clears `EXCLUDE_HOST` and sets `EXCLUDE_KERNEL` (so scheduler/IRQ
  branches don't inflate the count); whether the *guest-only* attribution works on N1 is
  AA-1(b)'s measurement, called out in the doc comment.
- **AA-5 guests self-seeded the clock page unconditionally.** `Machine::build` loaded the
  image and params but never published a pvclock page, so the guest saw a zero ABI page
  and reported `mode=self-seeded` — the fallback, not the work-derived mechanism AA-5
  certifies. `build` now calls `publish_pvclock_page`, writing the managed ABI-1 page
  (SEQ/VNS/GUEST_CLOCK/HZ/FLAGS) at the pvclock GPA before the vCPU runs.
- **The AA-5 clock check rejected every standard run.** It graded *all* AA-5 records, but
  seven of the eight default payloads emit no `CLOCKPAGE` line (`clockpage_mode == None`),
  so a run that proved managed mode on the clock-page payload still failed. The check is
  now scoped to `Payload::ClockPage` records — and fails if an AA-5 set contains *none*
  (the mechanism was never exercised), closing the mirror-image vacuity.
- **Replay-identity passed after comparing zero digests.** With the default `--reps 1`,
  AA-3 has no repeated group, yet the check left the verdict PASS — evidence could claim
  replay identity with no replay performed. `check_replay_identity` now emits
  NOT-REQUESTED (never PASS) when nothing was compared at a stage that requires it
  (AA-3/AA-6). The `accept` fixture is now two bit-identical reps of each of eight inputs,
  so replay identity is actually exercised and passes.
- **AA-3 could pass with zero armed records.** The missing-armed-floor case was gated on
  `armed > 0`, so an AA-3 run submitted without `--with-targets` emitted *no* floor
  outcome and the mechanism/skid checks had nothing to inspect — a landing stage passing
  without testing a single deadline. The requirement is now enforced on the *stage*
  (`requires_patched_mechanism`), independent of what the records happened to contain:
  AA-3/AA-4/AA-6 without a floor is NOT-REQUESTED, even (especially) at zero armed.
- **Window verification checked branch *classes* only.** A `b.ne`→`b.eq` flip or a
  redirected target keeps the `BCond` class while reversing the loop / changing the taken
  count, and the class-sequence compare passed both. `verify` now also checks each
  `B.cond`'s condition against the model's `inline_branch_conds` and that every immediate
  branch's target lands inside the counting window. New portable, Miri-tested
  `scan::decode_cond` / `branch_target` / `window_branches`; the oracle model gained the
  per-payload condition sequence (all window `B.cond`s are `NE`).
- **`ident` read BR_RETIRED support from the wrong register.** Common event 0x21 lives in
  `PMCEID1_EL0` bit 1 (`PMCEID0_EL0` covers 0x00–0x1F); `pmceid0 >> 0x21` sampled a
  reserved-zero bit and would falsely report BR_RETIRED unsupported on conforming PMUv3.
  The payload now reads `PMCEID1_EL0`, prints it, and tests bit 1. Golden regenerated for
  the new `ID pmceid1=` line.
- **The replay digest omitted vGIC state.** Two AA-6 reps differing only in pending/
  active/injected interrupt state carried identical vCPU registers and RAM, so the digest
  matched and a real injection divergence read as replay-identical. `state_digest` now
  dumps the vGIC distributor's enable/pending/active registers via `KVM_GET_DEVICE_ATTR`
  (`GRP_DIST_REGS`, a fixed offset order spanning IRQ IDs 0–95) and length-prefixes them
  into `digest_state` (bumped `v1`→`v2`). At AA-3 (no injection) the distributor is
  quiescent and identical across reps, so this strengthens AA-6 without disturbing AA-3.
- **The oracle recomputed `expected` per record at the large floors.** At scale 1e8,
  tens of thousands of `branch-dense` records each re-ran the full-scale PRNG — trillions
  of iterations, an impractical checker. `check_counts` now memoizes `expected` by
  `(payload, scale, seed)`.
- **[REBUTTED — arrival-day deferral] Skid-aware exact landing before grading AA-3.**
  The run loop arms at the full remaining delta and has no early-margin arm or single-step
  convergence, so exact landing happens only on zero-skid delivery. This is a deliberate
  AA-2/AA-3 deferral, not a defect: the convergence loop needs AA-1's *measured*
  `skid_margin` (task 109 forbids defaulting it) and AA-2's *validation* that single-step
  delivers exactly one instruction (an open question AA-2 answers before any patch is
  written). Arming at the full delta is exactly what AA-1(c) needs to measure the skid
  distribution. Crucially, the grading is *not* vacuous: `check_skid` at AA-3
  (`exact_required`) fails any record where `landed != target`, any overshoot, and any
  `|skid|` past the margin — so a nonzero-skid run *fails* AA-3, it cannot pass silently.
  Building the on-silicon convergence now would presume the very results AA-1/AA-2
  establish (docs/ARCH-BOUNDARY.md §Pre-build ruling: spikes gate trust, not
  construction). Rebutted on the PR with this reasoning.

Also fixed two cfg(linux)-only breakages the native gates cannot see (the
ci-cfg-linux-review-gap): the new `perf_flags` reference needed importing into the linux
`imp` module, and a latent `useless_conversion` in `arm_spike.rs` surfaced only under
aarch64 *clippy* (prior rounds ran aarch64 `check`). Round 5 runs aarch64 clippy, not
just check, so both are now caught pre-merge.

Fixtures unchanged in count (24); only the `accept` fixture's records changed (now 2
reps × 8 inputs). Miri still runs 78 tests.

## Round-6 review fixes (PR #108, cross-model pass r6)

The r5 skid-landing rebuttal was **accepted** as an arrival-day deferral. r6 returned 8
P1s, three of them wrong *kernel-UAPI constants* (the foreman independently confirmed
two) — the sharpest class yet, because the portable ABI tests self-validated the same
wrong constants and so passed while the real KVM path would `ENOTTY`. All 8 implemented;
one carries a scoped rebuttal for the part that cannot host-compile.

- **`KVM_GET_ONE_REG`/`KVM_GET_DEVICE_ATTR` had the wrong direction bits.** Both are
  `_IOW` in the KVM ABI (the *get* encodes write — userspace writes the descriptor, the
  kernel fills a pointed-at buffer), not `_IOR`/`_IOWR`. The code had `GET_ONE_REG =
  0x8010_AEAB` (`_IOR`) and `GET_DEVICE_ATTR = 0xC018_AEE2` (`_IOWR`); both select
  *unknown* ioctls and return `ENOTTY`, so **every** completed real-KVM sample would fail
  in `state_digest`. Corrected to `0x4010_AEAB` / `0x4018_AEE2` and pinned to their
  literal ABI numbers (the tests had asserted the wrong directions — self-validation).
- **The live-clock filter matched no real register — the r4 leak was back.**
  `is_host_time_register` encoded `KVM_REG_ARM64_SYSREG` at bit 48 (`0x0013 <<
  48`), but the coprocessor selector lives in the **low** bits (`0x0013 << 16`). A real
  timer-counter id has `0x0013_0000` there, so the predicate returned false for every
  one and the live `CNTVCT`/`CNTPCT` values re-entered the digest — same-seed replay dead
  again. Fixed to mask arch/size/coproc at their real positions, and pinned with a
  *literal* real id (`CNTVCT_EL0 = 0x6030_0000_0013_DF02`) so the encoding cannot silently
  regress from a builder that shares the bug.
- **The vGIC digest read the wrong group and missed the timer-PPI state.** `DIST_REGS`
  is group **1**, not 5 (5 is `REDIST_REGS`); and on GICv3 the SGI/PPI (private
  interrupt, IDs 0–31) enable/pending/active state — where the guest's **timer PPIs**
  land — lives in the **redistributor** SGI frame, not the distributor. The round-5 dump
  read distributor words 0–2 under the wrong group number, so it captured neither the
  SGI/PPI state nor valid SPI words. Now reads the redistributor SGI frame (`RD_base +
  0x1_0000`, `GICR_CTLR`/`ISENABLER0`/`ISPENDR0`/`ISACTIVER0`) for private interrupts and
  the distributor (word 1+, SPIs) for the rest — so an AA-6 injection divergence in the
  timer PPI is actually visible.
- **Payloads exited via `SYS_EXIT` (`0x18`), not `SYS_EXIT_EXTENDED` (`0x20`).** The
  two-word `{reason, status}` block that conveys an exit *code* is the defined interface
  of the extended call; relying on QEMU's AArch64 extension of `0x18` to read a block is
  version-dependent, and where it does not, a payload's `exit(1)` reports process success
  and the smoke's status gate goes blind to failures. Switched to the unambiguous `0x20`.
- **Window verification checked in-window targets, not *exact* ones.** A backedge, `CBZ`,
  or `TBZ` retargeted to a *different* in-window label keeps its class, its condition, and
  its in-window-ness while changing control flow and the taken-branch count. The oracle
  model now declares each branch's exact target (byte offset from the window base,
  extracted by construction from the built ELF), and `verify` checks the decoded target
  equals `base + offset`. All 18 window branches across the 8 payloads are pinned.
- **`ident` read BR_RETIRED support from `PMCEID1_EL0`** (bit 1), not `PMCEID0>>0x21` —
  this was the r5 finding #8; r6 re-flagged the *decode* which is now correct and pinned.
  (No change needed beyond r5; verified in place.)
- **No watchdog around `KVM_RUN`.** A guest wedged in WFI with no wake, a lost PMI, or a
  livelocked exclusive blocks the ioctl forever, so the command hangs and never writes the
  partial manifest — violating the bounded/total evidence requirement. Added a per-`KVM_RUN`
  `SIGALRM`/`ITIMER_REAL` watchdog (`--watchdog-secs`, default 300, 0 disables): a fired
  deadline makes the blocked ioctl return `EINTR`, and the run loop turns that into
  `RunError::Watchdog` — distinct from the perf kick — so the caller records a failed
  attempt instead of hanging. Portable test: a wedged `Vcpu` surfaces the error through
  `run_sample`, never a record.
- **AA-2 could pass with no single-step observed.** An ordinary `--stage aa2` run is
  unarmed and ends at the console sentinel, so the mechanism check graded nothing and the
  floor could report PASS having stepped not once. Added `check_debug_evidence`: at AA-2 it
  requires a debug-exit record (`ExitReason::Debug`) and reports NOT-REQUESTED (never PASS)
  when none is present — the same anti-vacuity shape as r5's replay-identity fix. The
  single-step *run path* stays arrival-day (the loop still refuses an unrequested debug
  exit, and the stepping loop would presume AA-2's own result — consistent with the
  accepted r5 deferral), so today AA-2 reads honestly-unexercised.
- **[PARTIAL REBUTTAL] Miri for the payload crates.** The runtime and oracles crates
  cannot be *built* for a Miri host target: their top-level `global_asm!` is aarch64
  machine code the host assembler rejects, and their `unsafe` is `asm!` + fixed-GPA
  volatile MMIO (integer-literal addresses with no Rust provenance for Miri to validate).
  So `cargo miri test` on them does not compile, let alone run — that part is rebutted
  with the compile-time fact. What Miri *can* check — the clock-page **byte layout** both
  the runtime (self-seeded) and the harness (managed) write — is now seamed into
  `oracle_model::pvclock` as pure `[u8]` packing, de-duplicating the two writers onto one
  layout, and **oracle-model is added to the nightly Miri job**. So the payloads' one
  Miri-checkable piece is now interpreted, and the residual is documented per-crate as
  intrinsic (the r4 disposition, strengthened).

The vGIC digest bumped `arm-spike-state` v1→v2 in r5; the group/offset fix here changes
which bytes it reads but not the version. Miri now runs the arm-harness seam (80 tests)
plus oracle-model (24 tests, incl. the shared clock-page layout).

## Round-7 review fixes (PR #108, cross-model pass r7)

Five P1 + three P2. Two carried explicit foreman steering (Miri exclusion — third
appearance, "no third partial measure"; and the AA-5 static page). All eight resolved:
seven implemented, one (the Miri exclusion) resolved as a definitive, contract-quoting
rebuttal.

- **The runtime/oracles Miri exclusion — resolved definitively as a rebuttal
  (`payloads/runtime/src/lib.rs`).** The foreman offered either real `cfg(miri)` seams or
  a contract-quoting rebuttal. The rebuttal is the correct resolution *by the contract's
  own terms*: `AGENTS.md:48-55` and `tasks/00-CONVENTIONS.md:104-117` require the
  privileged/`asm!` paths to be `#[cfg(not(miri))]`-excluded with "the unsafe logic driven
  by an in-process loopback" — which presupposes (a) the crate *compiles* under
  `cfg(miri)` and (b) there is a substitutable allocation for the loopback. Neither holds:
  the crates' top-level `global_asm!` is aarch64 machine code the host assembler rejects
  (so `cargo miri test -p runtime`/`-p oracles` does not compile), and the residual unsafe
  is the rule's explicit `asm!` carve-out plus fixed-GPA volatile MMIO (integer-literal
  addresses, no provenance, no allocation to loopback). The rule's actual target — "the
  pointer/bounds logic" (`tasks/14-backend.md`) — is, for these payloads, the clock-page
  byte layout, which *is* seamed into `oracle_model::pvclock` and Miri-run. The doc comment
  now quotes the contract verbatim and lays this out; the CI comment points to it. Not a
  third partial — the rule's target is covered and the residual is the sanctioned carve-out.
- **AA-5 static pages no longer pass (`machine.rs`, `pvclock`, `check_clockpage_mode`).**
  Publishing a one-time zero page let the guest report `managed` and AA-5 accept it —
  a static page certifying a clock that is supposed to be work-derived and refreshed.
  Added `FLAG_WORK_DERIVED` to the shared page layout; the harness's static placeholder
  sets only `FLAG_MATERIALIZED`, so the guest now reports `managed-static`, and the AA-5
  check reads that as **NOT-REQUESTED** — the plumbing works, but the work-derived clock
  (`hm-8h8`) is a silicon-day item, the accepted-deferral shape. `work-derived` passes,
  `self-seeded`/absent hard-fails. (The TCG golden is unchanged: TCG self-seeds.)
- **Zero acceptance floors are rejected (`check_floors`).** `--min-armed-overflows 0`
  passed `0 >= 0`, and `--min-reps 0` was as vacuous. Both now fail closed: a floor of
  zero is met by a run that armed/repeated nothing, exactly the pass the floor exists to
  prevent. (Sub-normative *nonzero* floors stay valid — the fixtures use small floors to
  test the checker's logic, and the verdict names the exact floor a disposition rests on.)
- **AA-2 now validates a measured step, not a forgeable label (`StepRecord`).** The r6
  check keyed on `exit_reason == Debug` — one enum byte a rehashed run-set can flip. Added
  a structured `StepRecord` (pc before/after, instructions retired, `BR_RETIRED` delta) to
  the record schema (v2→v3); the AA-2 check requires it and validates the PC advanced and
  exactly one instruction retired. No run emits one yet (the stepping run path is
  arrival-day), so AA-2 reads NOT-REQUESTED — but a bare enum flip no longer satisfies it.
- **AA-6 verifies the whole matrix before grading reps (`check_aa6_matrix`).** The rep
  floor only grades inputs that are present, so 1,000 copies of one payload satisfied
  `--min-reps 1000` while the rest of the matrix was silently absent — and the
  `accept-aa6-gate` fixture was itself that vacuity (one payload). Now every windowed
  payload must be present; missing payloads fail. The accept fixture is the full 8-payload
  matrix, each bit-identically repeated. (Full-system AA-6 coverage — the AA-5 Linux
  guest, the AA-0 truth-table — needs arrival-day artifacts and is scoped as silicon-day.)
- **P2: the harness refuses to overwrite a run-set (`arm_spike.rs`).** Reusing `--out`
  truncated prior evidence (and a mid-write failure left new records beside an old
  manifest). It now refuses an existing output directory and writes each file with
  exclusive creation (`create_new`) — a run-set is immutable evidence.
- **P2: watchdog setup failures propagate (`arm_watchdog`).** A `--watchdog-secs` past
  `time_t` range cast negative and `setitimer` returned `EINVAL`, silently ignored — the
  harness then entered `KVM_RUN` believing a watchdog was armed. It now validates the
  conversion and checks `setitimer`'s return, surfacing a seam error so no run proceeds
  with a phantom watchdog.
- **P2: CI runs aarch64 *clippy*, not just check (`quality.yml`).** The cross-target job
  ran `cargo check`, so the `cfg(target_os = "linux")` seam's target-specific lints (the
  `useless_conversion` class from r5) could merge undetected. Now `cargo clippy -- -D
  warnings`.

Fixtures: 24, unchanged in count; `accept-aa6-gate` is now the full 8-payload matrix, and
every record carries the new `step: null`. Schema v3. Miri runs arm-harness (80) plus
oracle-model (24, incl. the shared clock-page layout with the work-derived flag).

## Round-8 review fixes (PR #108, cross-model pass r8)

Five P1 + three P2 (the Miri finding is adjudicated-settled from r7 and was not
re-litigated). Seven fixed; one (per-class offsets) rebutted as a documented arrival-day
deferral.

- **The armed perf event could not arm (foreman-confirmed EINVAL).** `PerfCounter::open`
  opened every run — armed or not — with `br_retired_attr(None)` (`sample_period == 0`, a
  non-sampling event). `PERF_EVENT_IOC_PERIOD`, which `arm_overflow` uses at MARK_BEGIN,
  rejects a non-sampling event with `EINVAL`, so the first arm of every `--with-targets`
  run failed on real hardware. An armed run now opens in SAMPLING mode with a period
  beyond any window's reach (`u64::MAX`) — sampling from the start (so `IOC_PERIOD` works)
  yet not overflowing during boot; a counting run stays non-sampling.
- **Zero/below-normative floors + the floor was not printed.** Two-part. (a) The r7 claim
  that "the verdict names the exact floor" was wrong for a PASS: `floor-check` printed only
  `[PASS] armed-overflow-floor`, not the number. It now prints each outcome's DETAIL, so
  the exact floor is on the face of the verdict. (b) Added the stage-normative minima
  (AA-1/AA-3 = 1,000,000 armed, AA-6 = 1,000 reps): a requested floor below the minimum
  now fails closed unless `--sub-normative` is passed, and every weakened outcome is tagged
  `[SUB-NORMATIVE]` so it can never be mistaken for a normative acceptance. (Fixtures pass
  `--sub-normative`.)
- **AA-1's cumulative floor was not summable across conditions.** `floor-check` took one
  directory, but `arm-spike run` emits one run-set per condition and AA-1's million-overflow
  floor is cumulative across the contamination matrix. The CLI now takes several
  directories; `check_run_sets` runs each set's per-record floors on its own and applies
  the armed-overflow floor ONCE over the union (`check_armed_floor` split out for it) — so
  a million `pinned-solo` overflows can no longer pass while the contamination conditions
  went unmeasured, and smaller condition sets that together clear the floor now do.
- **[REBUTTED — arrival-day deferral] Per-class window offsets.** The model uses one global
  `window_offset`; AA-1's contract permits *stable per-class* offsets. This is a deliberate,
  documented deferral (oracle-model:456-500), not a gap: emitting a per-class pack requires
  the differential-scale-intercept solve whose input is the 1e6/1e7/1e8 counts AA-1 measures
  on silicon, and building that solve pre-silicon as a free per-class fit would make the
  model *unidentifiable* — destroying the over-determination (`Solved::residual`) that
  catches a wrong answer. The apparatus validates the identifiable uniform model pre-silicon
  and names the per-class generalization as the escape hatch — the accepted skid-landing
  deferral shape. Rebutted on the PR; the field doc strengthened.
- **ELF executable-segment scan was fail-open.** `executable_ranges` `filter_map`ped
  segments, silently dropping a malformed one — so an image with one valid and one truncated
  executable segment reported a clean *partial* scan, a fail-open for AA-5 where the opcode
  scan IS the enforcement. It now fails closed: a malformed executable segment (or section)
  is `Truncated`, never dropped. (Test: a segment with `p_filesz` past the file errors.)
- **CBZ/TBZ predicate operands were unverified (P2).** Window verification checked `B.cond`
  conditions and exact targets, but a `TBZ` whose tested bit/register — or a `CBZ` whose
  source register — is changed keeps its class and target while changing the taken-branch
  count. The model now declares each branch's register/bit operand
  (`scan::branch_test_operand`, extracted by construction), and `verify` checks it.
- **Full-width target ranges panicked (P2).** `PlanSpec { target_delta_range: Some((0,
  u64::MAX)) }` overflowed the inclusive width `hi - lo + 1` — a debug panic, a release
  divide-by-zero. `PlanSpec` is public/untrusted, so the width is now checked: a full-u64
  span uses the draw directly. (Test pins it.)

Schema unchanged (v3). Fixtures: 24. Miri runs arm-harness (82) plus oracle-model (24).

## Round-9 review fixes (PR #108, cross-model pass r9)

Four P1/P2 fixes plus one ruled doc-only change. The Miri finding stays adjudicated-settled
(r7), and the per-class-offsets rebuttal was ACCEPTED.

- **`FLAG_WORK_DERIVED` reserved-bit collision — RULED (doc-only here).** PR #110 froze ABI 1
  with bit 0 = MATERIALIZED and the rest reserved, and this spike's `FLAG_WORK_DERIVED` took
  bit 1. Foreman ruling: amend ABI 1 to *define* bit 1 = WORK_DERIVED (docs/PARAVIRT-CLOCK.md
  §1 flags row + the real stamping path), coordinated through PR #110; this spike's usage then
  conforms as-is. This PR's only change is the flag comment, which now cites the amended ABI
  (bit 1 defined, not consumed) rather than describing itself as taking a reserved bit.
- **Duplicate run-sets could inflate the cumulative floor.** The r8 aggregation summed records
  across directories with no uniqueness check, so the same run-set supplied twice double-counted
  — a 500,000-record set passed as a million. `check_aggregation` now rejects duplicates by
  run-set id AND by `records_sha256` (identical evidence), so only distinct measurements sum.
- **AA-1 aggregation did not enforce the condition matrix.** The only cross-run-set check was
  the stage, so a million `pinned-solo` samples (or several same-condition sets) passed without
  the contamination probes. Two additions: (a) every record's `condition` must match its
  manifest's (`check_condition_consistency`, per-set, all stages); (b) at AA-1, the cumulative
  run must cover the required distinct condition matrix — pinned-solo, co-tenant-other-core,
  co-tenant-same-core, memory-pressure (`docs/ARM-ALTRA.md` §AA-1) — before the summed floor
  means anything (`check_aa1_condition_matrix`). `check_run_sets` now loads then delegates to a
  unit-testable `aggregate`.
- **AA-2 did not validate `br_retired_delta`.** The step check verified the PC advanced and one
  instruction retired, but a `StepRecord` with `br_retired_delta: 99` passed — the branch delta
  was never examined, so the gate proved nothing about the counter's per-branch behaviour. It
  now ties the delta to the branch outcome: a step that lands at `pc + 4` fell through (delta 0),
  anywhere else took a branch (delta 1); anything else fails. (This is the whole point of AA-2:
  BR_RETIRED counts a taken branch exactly once and nothing else.)
- **ELF load-segment parsing was fail-open (P2).** r8 fixed `executable_ranges`, but
  `load_segments` (the guest-RAM loader's source) still `filter_map`ped — a truncated `PT_LOAD`
  was silently dropped and the payload booted as a partial image. `load_segments` now returns
  `Result` and fails closed on a malformed segment; `load_into` propagates it. (Test: a segment
  with `p_filesz` past the file errors, and `load_into` refuses.)

Fixtures: 24, unchanged. Miri runs arm-harness (83) plus oracle-model (24).

## Round-10 review fixes (PR #108, cross-model pass r10)

Seven P1 + two P2, all fresh surface, plus a **self-sweep** the foreman asked for — an
independent audit of the whole floor-check/harness surface for the two patterns these
rounds keep hitting (vacuous-pass on absent/partial evidence; panic-on-untrusted-input) —
which surfaced two more, both fixed here.

- **Hash-normalized dedup + char-safe truncation.** The r9 duplicate-dedup compared raw
  `records_sha256` strings, so the same records once as `<hex>` and once as `sha256:<hex>`
  (both schema-valid) counted as distinct and doubled the floor. Dedup now uses the same
  `normalise_hash` verification uses. And the diagnostic's `&hash[..16]` byte-slice would
  panic on a malformed multi-byte hash before the well-formed check could reject it; it is
  now `chars().take(16)`.
- **Single-directory AA-1 goes through the matrix.** The r9 aggregation shortcut let a lone
  normative `pinned-solo` million-overflow directory `RESULT: PASS` without the
  contamination matrix. `check_run_sets` no longer shortcuts a single directory — every CLI
  run goes through the same aggregate path, so the matrix requirement applies.
- **Zero-attempt run-sets refused.** `attempted: 0` with an empty (correctly hashed) records
  file passed totality and every per-record check vacuously. `check_totality` now fails
  closed on it, and the schema pins `attempted` ≥ 1.
- **AA-2 step-coverage matrix + opcode-based classification.** Two coupled fixes. (a) One
  valid `StepRecord` beside seven `step: null` records passed — a partial characterization.
  The check now requires the FULL transition matrix (sequential, taken-branch, exception
  entry, ERET, WFI, injection). (b) The r9 delta check inferred "taken branch" from
  `pc_after != pc_before + 4`, which misclassifies an SVC/ERET/injected-IRQ *exception*
  transfer as a retired branch and wrongly forces `delta == 1`. `StepRecord` now carries a
  `StepTransition` recorded from the stepped opcode, and the delta is validated against the
  class: forced only where the architecture guarantees a retired branch, measured (0-or-1)
  for the exception/WFI/injection classes AA-2 characterizes.
- **Migration-probe CLI mode.** `execute` always hard-pinned and recorded
  `migration_probe: false`, so AA-1's required bounded migration probe could not be run
  honestly. Added `--migration-probe`: it skips pinning and records `pinned: false,
  migration_probe: true` — the posture the checker already accepts only for that sanctioned
  probe.
- **`plan` bounds reps (P2).** `--reps 18446744073709551615` saturated `matrix.len() * reps`
  to `usize::MAX` and panicked in `Vec::with_capacity`. `plan` now returns `Result` and
  refuses a total over `MAX_PLANNED_SAMPLES` (or an overflow) with a normal error.
- **`host/verify.sh` — CI patch format/parse gate (P2).** The full apply-onto-6.18.35 +
  compile gate needs a native-aarch64 builder with the pinned kernel tree and cannot run on
  the x86 runner. CI now validates each patch is a well-formed git mail-format patch whose
  diff parses (`git mailinfo` + `git apply --stat`), so the most likely regression — a
  corrupted/truncated patch that stops applying — fails CI instead of merging green; the
  full compile gate is documented as the builder's (`host/BUILD.md`).

**Self-sweep findings (found and fixed proactively):**

- **AA-1 matrix was satisfied by labels, not measurement.** The r9/r10 matrix required each
  condition's *name* to appear, so a million-overflow `pinned-solo` set beside three
  counting-mode (zero-armed) run-sets carrying the other labels passed — the cumulative
  floor met by `pinned-solo` alone, the contamination conditions never actually measured.
  Now each required condition must contribute a NONZERO armed count, and (for a normative
  run) at least its equal share of the floor.
- **Unbounded oracle work on hostile records.** `check_counts` memoizes `expected` by
  `(payload, scale, seed)`, but a records file of many DISTINCT large-scale `branch-dense`
  seeds forces a fresh ~10⁸-iteration simulation each — a multi-hour hang, violating the
  checker's own "fail closed, not hung" discipline. The cumulative simulated trips are now
  bounded (`MAX_ORACLE_TRIPS`); over the ceiling it fails closed.

Fixtures: 24, unchanged (step records are still `null`; `attempted` ≥ 8). Schema v3 (the
step-record gains `transition`, an unproduced sub-schema). Miri runs arm-harness (84) plus
oracle-model (24).

## Round-11 review fixes (PR #108, cross-model pass r11)

Six P1 + one P2. The Miri item stays settled (bare-metal payloads cannot run under
Miri; the portable logic is seamed into arm-harness/oracle-model, both of which Miri
does run). The review's cadence question — the checker is being hardened ahead of the
arrival-day evidence it grades — is escalated to Paul; work continued per the note.

- **AA-0 probe fails closed on ANY absent OR unprobed mandatory row.** The host `probe`
  command only asked four rows (`/dev/kvm`, raw 0x21 pinned, guest-debug, the patch
  marker), so a host missing an *existential* mechanism the truth table lists mandatory
  — `br-retired-pmceid0`, `host-overflow-delivers`, `vgicv3-creatable`,
  `writable-id-registers` — probed green. Added those four as `Capability` variants with
  real Linux probes: BR_RETIRED implemented-at-all (`perf_event_open` not `ENOENT`); a
  host overflow actually *delivered* (arm a small sample period, run past it, confirm the
  ring buffer's `data_head` advanced); vGICv3 `KVM_CREATE_DEVICE`-able; and an ID register
  writable (`KVM_GET_ONE_REG` then `KVM_SET_ONE_REG` of the SAME value — a read-only
  kernel rejects it with `EINVAL`, and writing back what was read cannot change a feature).
  `Capability::mandatory_aa0()` is the single list `probe` iterates; every row absent or
  unprobed is disqualifying (the patch marker alone stays expect-absent, reported
  separately). Each new probe returns `Ok(false)` only on a clean "no" errno and `Err`
  on any failure-to-probe — the same fail-closed discipline as the existing rows.
- **Records that promise a `retries=` term that never printed are refused.** `run`
  defaulted the reported term to `0` when the guest never printed a `retries=`/`RETRIES`
  line, so a payload whose count model *includes* a reported term (`llsc-atomics`,
  `clock-page`) could silently run with the term dropped and the checker would recompute
  a too-low expected count and pass it. The parser now holds `Option<u64>`; at record
  assembly a payload with `has_reported_term()` that produced no line is a hard
  `RunError::MissingReportedTerm`, while a payload with no reported term keeps `0`.
- **AA-2 step-moment replay identity + explicit LL/SC stepping coverage.** Two coupled
  fixes. (a) Replay identity compared the final `state_digest`, which can *converge* (two
  different stepped states reach the same exit sentinel), so divergent single-step
  behaviour across repeated inputs could pass. `StepRecord` now carries a `step_digest`
  taken at the step moment, and AA-2/AA-3/AA-6 replay identity compares THAT across every
  repeated `(payload, scale, seed, condition)` group. (b) The required step matrix had no
  exclusive-monitor case, so LL/SC stepping — the one AA-2 most needs characterized — was
  uncovered; added `StepTransition::LlscExclusive` to the enum, the required-transition
  set, and the delta validation (measured 0-or-1, like the other non-branch classes).
- **AA-6 determinism matrix includes the AA-5 Linux guest class.** The matrix required
  only the windowed bare-metal payloads, so a determinism run could omit the full Linux
  VM (AA-5) entirely. Added `Payload::LinuxGuest` (a guest *class*, deliberately NOT in
  `ALL_PAYLOADS` — it is not a buildable bare-metal payload) and made
  `required_aa6_classes` the windowed payloads plus it; `check_aa6_matrix` now demands the
  Linux class appear.
- **AA-4 dropped from the million-overflow normative floor.** `normative_armed_floor`
  applied the ≥10⁶ cumulative-armed floor to AA-4 too, but `docs/ARM-ALTRA.md` scopes that
  floor to AA-1 (skid/count invariance) and AA-3 (exact landing); AA-4 is the LSE-only
  contract, whose evidence is not a million overflows. The floor is now `Aa1 | Aa3`.

The schema mirrors the code: `run-record.schema.json`'s step-record gains `step_digest`
(minLength 1) and the step-transition enum gains `llsc-exclusive`; the payload enum gains
`linux-guest`. Fixtures: 24 (the AA-6 accept fixture and the RepFloor reject fixture each
gained a `linux-guest` record so the newly-required class is present; no new fixture
files). Gates re-run green: harness 86 tests, floor-check 39 + 27 + 3, oracle-model 22 +
2, both clippy targets and all three `cargo deny` clean, the window-vs-oracle scan and TCG
smoke pass, and Miri runs arm-harness (85) plus oracle-model.

## Round-12 review fixes (PR #108, cross-model pass r12 — intended final mechanical round)

The review's 6 P1 + 2 P2; the Miri-on-payload-crates item stays adjudicated-settled (the
`runtime`/`oracles` `global_asm!` is aarch64 the host assembler rejects, so `cargo miri
test -p runtime` cannot even build an interpreter run to gate — the rule's actual target,
the byte-layout logic, is seamed into `oracle-model` and interpreted under Miri already;
this is the r11 adjudication and CI documents it at `quality.yml`'s Miri step). The other
seven are fixed.

- **Aggregated run-sets must share one constants pack + measurement environment.** Summing
  records across contamination conditions and grading count invariance is only meaningful
  if every set was measured under the same `weights`, `perf`, `environment`, and
  `mechanism`; otherwise a condition-dependent count change hides behind a compensating
  per-set difference (a different offset in each `weights` pack) and the aggregate still
  "passes" invariance. `check_aggregation` now requires those four to match across the
  union; only `condition` (and AA-1's pinning posture) may vary — they are the sweep.
- **AA-1 differential scale sweep enforced.** An AA-1 run carrying only the CLI-default
  `smoke` scale could be certified, though the binding method derives the per-class offsets
  from the 1e6/1e7/1e8 sweep (one scale cannot separate the offset from the
  scale-proportional term). A normative AA-1 verdict now requires all three sweep scales
  present; `--sub-normative` relaxes it as it already relaxes the floor magnitude.
- **AA-1 armed runs must use the stock signal mechanism.** Only AA-3/AA-4/AA-6 had a
  stage-specific mechanism constraint, so `--mechanism patched` at AA-1 produced a
  self-consistent `Preempt` tuple that passed — but AA-1(c) measures the *pre-patch* host
  signal kick, and the in-kernel force-exit (AA-3's) has different delivery and skid
  behaviour. `check_mechanism` now requires an armed AA-1 run to declare `signal-kick`
  (counting-mode AA-1 arms nothing and is exempt).
- **Replay identity must cover EVERY acceptance-bearing group.** The existential
  `compared > 0` let one stepped record per AA-2 transition (each a singleton) ride beside
  two duplicate *unstepped* records and pass, though not one stepped state was replayed;
  likewise a unique armed AA-3 landing beside a repeated unarmed group. AA-2/AA-3 now rest
  on the acceptance-bearing groups alone (stepped states / armed landings): all singletons
  reads NOT-REQUESTED (submit reps), a mix of replayed-and-singleton is a FAIL (a specific
  input left unreplayed), all-repeated passes. An unrelated repeated group is never a
  stand-in. AA-6 keeps its whole-record digest logic.
- **Reject pinned runs labelled as the migration probe (P2).** `migration_probe: true` with
  `pinned: true` (or a pinned core) reported a normal pinning pass, but the sanctioned probe
  is unpinned *by design* to exercise the rr #3607 migration failure mode — a pinned probe
  never migrates. The contradictory tuple is now refused.
- **Perf ring geometry from `_SC_PAGESIZE` (P2).** The `host-overflow-delivers` AA-0 probe
  mmap'd `4096 * (1 + 8)`; on a 64 KiB-page Altra host that 36 KiB rounds to a single
  metadata page with ZERO data pages, so `mmap` fails and the mandatory row cannot be
  probed at all. The geometry now derives from the runtime page size (a real arrival-day
  breaker, not a portability nicety).
- **Guest-reported scale/seed cross-checked (beyond the named 4 P1 + 2 P2).** The guest
  prints `PARAMS mode=<m> scale=<s> seed=<hex>` — the params page it *actually saw* — but
  the harness kept only `mode` and labelled the record's scale/seed from the sample spec.
  A stale or mis-written page on a seed-ignoring payload (`straight-line`) whose counts
  still match the oracle would be accepted, attributed to an input the guest never ran.
  `run_sample` now cross-checks the reported scale and seed against the spec and refuses a
  mismatch (`ReportedScaleMismatch`/`ReportedSeedMismatch`) — the exact parallel of the
  existing `params_mode` cross-check. Verified it holds and fixed it here per the round's
  "fix what holds" discipline; called out separately so it can be deferred if unwanted.

No schema change (no new record fields; the rest is checker/probe logic). Gates re-run
green: harness 87 tests, floor-check 44 + 27 + 3, oracle-model 22 + 2, both clippy targets
and all three `cargo deny` clean, the window-vs-oracle scan and TCG smoke pass, the
kernel-patch format/parse gate, and Miri runs arm-harness plus oracle-model.

## Round-13 review fixes (PR #108, cross-model pass r13)

Seven findings, all fixed — same species as the prior rounds (close a path by which
arrival-day evidence could pass a floor without measuring what the floor is about, or a
digest could equate non-equivalent machine states). Paul's cadence ruling on how long to
keep fortifying the checker ahead of the box is still pending; the loop continued meanwhile.

- **The migration probe must force and attest movement, and AA-1 must require it.** Two
  coupled fixes. (a) In migration-probe mode the harness merely skipped pinning, so on a
  quiet host the scheduler could leave the vCPU thread on one core for the whole run,
  exercising no migration at all. It now DELIBERATELY rotates the thread across the allowed
  cpuset (`sched_getaffinity`), one core per sample, forcing cross-core movement; it refuses
  to run in a single-core lease (nothing to migrate between). The evidence still records
  `pinned: false` / no single core — the truth. (b) The checker's AA-1 aggregation now
  REQUIRES a run-set carrying `pinning.migration_probe`: four pinned contamination
  conditions never leave their core and cannot supply the rr #3607 cross-core evidence the
  stage contract calls for, so a probe-less report no longer certifies (normative only).
- **Compare payload images before aggregating.** The aggregation comparability check
  (weights/perf/environment/mechanism) now also compares the pinned payload image content
  hashes. Records from different binaries could otherwise be summed into one condition
  matrix, so an apparent condition-dependent difference (or invariance) could be an artefact
  of a changed workload rather than the contamination.
- **Sequential steps must land at exactly PC+4.** A `Sequential` transition was accepted
  whenever the PC merely changed, so a single-step advancing by 8 (a skipped instruction)
  passed. AArch64 instructions are a fixed 4 bytes; a sequential step now must land at
  `pc_before + 4`, which is exactly the skip/double AA-2 exists to detect.
- **LL/SC single-steps must reject a BR_RETIRED delta of 1.** The `LlscExclusive` class was
  grouped with the exception/WFI/injection classes and admitted a delta of 1. A
  single-stepped `LDXR`/`STXR` is a load or store, not a taken branch, so BR_RETIRED must
  not move (the retry is a separate `CBNZ`, stepped and classed as a TakenBranch). It now
  requires delta 0, like Sequential.
- **Extend the vGIC digest to all injection-relevant state.** The redistributor/distributor
  digest read only control/enable/pending/active, so two runs differing only in group,
  priority, configuration, routing, or the redistributor wake state produced the same digest
  though a pending interrupt would inject differently — letting AA-6 replay identity pass for
  non-equivalent machine states. The digest now also covers `IGROUPR`/`IGRPMODR` (group),
  `IPRIORITYR` (priority), `ICFGR` (config), `GICD_IROUTER` (SPI routing), and `GICR_WAKER`
  (wake), for the private interrupts and the low SPI range.
- **Enforce state_digest well-formedness on every record.** The well-formedness loop
  validated only `condition`, despite its comment. An armed or stepped record with an empty
  `state_digest` (which replay identity does not read — it compares the landed/step digest)
  passed while violating the schema's `minLength: 1` and lacking the advertised complete-state
  evidence. Every record's `state_digest` is now required non-empty.
- **Reject undersized ELF program/section-header entries.** The parser advanced by an
  untrusted `e_phentsize` while reading fields that need the 56-byte ELF64 program header;
  with `e_phentsize = 0` every declared header re-read the first entry, so a later executable
  segment carrying a forbidden opcode was never scanned. `e_phentsize < 56` (and the identical
  `e_shentsize < 64` hazard) are now rejected.

No schema change. Gates re-run green: harness 88 tests, floor-check 44 + 27 + 3,
oracle-model 22 + 2, clippy `-D warnings` native + aarch64-linux (the affinity, vGIC and
migration-probe code compiles for the box), three `cargo deny`, the window-vs-oracle scan,
TCG smoke, kernel-patch format/parse, and Miri (arm-harness + oracle-model).

## Round-14 review fixes (PR #108, cross-model pass r14)

Eight findings fixed, one (Miri on the payload crates) stays adjudicated-settled. The first
is a **regression** the r13 work's predecessor introduced; the rest continue the species.

- **Perf sample-period sentinel below bit 63 (CRITICAL regression).** The r8 sampling-mode
  fix opened an armed event, and resumed it after a landing, with `sample_period = u64::MAX`.
  Linux rejects a period whose top bit is set (`perf_event_open` and `PERF_EVENT_IOC_PERIOD`
  both `EINVAL`), so every `--with-targets` run failed before the first sample. Both sites now
  use `PARKED_PERIOD = i64::MAX`, and a compile-time assertion pins bit 63 clear so the
  regression cannot recur.
- **Migrate a LIVE armed perf context, and require armed probe records.** Two coupled fixes.
  (a) r13's per-sample re-pin changed affinity BEFORE each fresh `PerfCounter` was opened and
  dropped it before the next, so no armed context ever migrated — it could not exercise the rr
  #3607 missed-overflow mode. The probe now runs a background `MigrationChurner` that rotates
  the LIVE vCPU thread across the cpuset while the sample loop arms and reads its counter, so
  an armed `KVM_RUN` in progress is forced across cores; the run fails if the churner issued
  zero moves. (b) The checker's AA-1 predicate now requires ARMED overflows FROM the
  migration-probe set(s), not merely a `migration_probe: true` label — a counting-mode probe
  migrates nothing under an armed overflow.
- **AA-1 scale sweep per payload class.** The union-level check passed when 1e6/1e7/1e8 each
  occurred for DIFFERENT classes, though that gives no class a differential sweep. Every
  acceptance-bearing (armed) payload class must now itself cover all three scales.
- **Verify the non-branch oracle-load-bearing opcodes.** The window gate checked only
  branches; removing an `svc #0` or retuning `subs …, #1` to `#2` leaves the branch classes,
  predicates, targets, and smoke output unchanged while breaking the count. The model now
  declares each payload's required non-branch ops (`Payload::required_window_ops`: SVC, WFI,
  LL/SC exclusive, the `subs #1` loop decrement) and the gate verifies them, with a new
  opcode decoder (`scan::classify_oracle_op`, calibrated against the built payloads and pinned
  by encoding tests).
- **Probe a real ID-register feature CHANGE.** The r11 writable-ID probe wrote the value it
  just read; some KVM versions accept an identity `SET_ONE_REG` (migration compatibility)
  while rejecting any changed invariant, false-greening the row though AA-6 cannot install its
  below-host synthetic model. The probe now reduces one feature nibble by 1 (skipping the
  exception-level and absent/0xF fields), writes it, and READS IT BACK: the row is true only
  if a reduced value is both accepted and observed.
- **PMCEID-backed BR_RETIRED proof, not a clean open.** ARM perf accepts arbitrary raw event
  encodings, so a clean `perf_event_open` of raw 0x21 is not proof the architectural event
  exists (it opens and reads zero). The `br-retired-pmceid0` probe now reads the PMU's
  `events/br_retired` sysfs file — which the PMUv3 driver exposes only when the PMCEID bit is
  set — and confirms it encodes `event=0x21` (`sysfs_event_encodes`, unit-tested off the box).
- **Validate hash casing before normalization.** `normalise_hash` lowercases for the
  case-insensitive records-hash match, so an UPPERCASE sha256 passed the well-formed FORMAT
  check though the schema pattern is `[0-9a-f]`. The format check now strips only the optional
  `sha256:` prefix and validates the raw digits.

The Miri-on-payload-crates finding stays adjudicated-settled (r11): the payload crates'
top-level `global_asm!` is aarch64 the host assembler rejects, so `cargo miri test -p runtime`
cannot build an interpreter run to gate; the byte-layout logic is seamed into `oracle-model`
and Miri'd there.

No schema change. Gates re-run green: harness 90 tests, floor-check 47 + 27 + 3, oracle-model
22 + 2 (the model gained `OracleOp`), clippy `-D warnings` native + aarch64-linux (the perf,
churner, ID-probe and PMCEID code compiles for the box), three `cargo deny`, the
window-vs-oracle scan (now opcode-checked), TCG smoke, kernel-patch format/parse, and Miri
(arm-harness + oracle-model).

## Round-15 review fixes (PR #108, cross-model pass r15)

Seven P1s, mostly corner cases in the r13/r14 migration and window-gate machinery.

- **Keep the vCPU signals off the migration helper.** The r14 churner thread inherited
  unblocked `SIGUSR1` (the stock overflow kick) and `SIGALRM` (the watchdog). Both are
  process-directed (`F_SETOWN(getpid())`, process-wide `ITIMER_REAL`), so the kernel could
  run either handler on the churner instead of the vCPU thread blocked in `KVM_RUN` — the
  handler would set its global atomic without interrupting the ioctl, reporting a real
  overflow lost or leaving a wedge unbroken. The churner now `pthread_sigmask`s both signals
  blocked, forcing delivery to the vCPU thread.
- **Consume perf kicks that race a normal exit.** A stock `SIGUSR1` can be handled just
  AFTER `KVM_RUN` returned an MMIO exit; the handler set `PERF_SOURCED_KICK` but only the
  `EINTR` branch consumed it, so the next `KVM_RUN` re-entered after the one-shot counter had
  disabled itself and blocked for a second signal that never came (a lost PMI → timeout or
  zero deliveries). `Machine::run` now checks the flag at the top of its loop and surfaces a
  pending kick as the `SignalKick` it is before re-entering.
- **Attest migration within an ARMED interval.** The churner starts before kernel hashing,
  payload loading, planning and VM construction, and the r14 check considered only its
  lifetime move total — which could be satisfied entirely before the first `arm_overflow`.
  The harness now snapshots the churner's move count around each sample and requires that at
  least one ARMED sample saw the thread move; a probe with no move during any armed interval
  fails rather than certifying a no-op.
- **Enforce the explicit AA-1 payload × condition × scale grid.** The r14 per-payload scale
  check unioned scales across conditions and checked conditions only by armed totals, so a
  submission of only `straight-line` at three scales under four conditions plus the probe
  passed though it never measured the WFI/exception classes, and disjoint payloads per
  condition gave no contamination comparison. A normative verdict now requires an armed
  record in every `REQUIRED_AA1_PAYLOADS` × condition × scale cell (the five distinct
  `BR_RETIRED`-behaviour classes: sequential, branch-dense, synchronous-exception, wait,
  async-abort).
- **Include the step moment in the replay key.** For unarmed AA-2 records the `RepKey`
  omitted the step moment, so two step points of one input — an `SVC` entry and its `ERET` —
  grouped together and their necessarily-different `step_digest`s read as false divergence.
  `RepKey` now carries `(pc_before, transition)`, grouping each step point with its own
  repetitions.
- **Require the exact non-branch opcode sequence (order + multiplicity).** Two coupled
  fixes. The model's `required_window_ops` fell through to `&[]` for `exception-abort` and
  the other counted loops, so retuning their `subs …, #1` to `#2` (halving the iterations)
  passed the branch gate and the golden output. And the window gate used `contains`, so a
  SECOND `svc #0` in the loop (doubling the exception contribution) also passed. The model
  now declares each window's EXACT ordered class sequence — every counted loop's
  `SubsDecrement`, both sides of the LL/SC pair — calibrated against the built payloads, and
  the gate compares the decoded sequence verbatim.

No schema change. Gates re-run green: harness 91 tests, floor-check 47 + 27 + 3,
oracle-model 22 + 2 (the model's `required_window_ops` became the exact sequence), clippy
`-D warnings` native + aarch64-linux, three `cargo deny`, the opcode-checked window scan,
TCG smoke, kernel-patch format/parse, and Miri (arm-harness + oracle-model).

## Round-16 review fixes (PR #108, cross-model pass r16)

Six P1 + one P2 — all in the harness/probe surface (the floor checker was untouched this
round).

- **Distinct target draws per repetition (the armed floor).** The plan drew one target per
  matrix cell and cloned it across every rep, so `--with-targets --reps 125000` on one
  cell/scale reached a million records with only eight target deltas — no seeded-random
  target/skid *distribution*. Added a `--cases` dimension: each case draws its own seed and
  target, and `--reps` repeats each case for replay identity. The floor is now over distinct
  cases (`cells × cases × reps`), not one delta repeated.
- **Accept early source-verified perf kicks (negative skid).** The advisory branch (work <
  target) caught a stock `SignalKick`, but a source-verified `SignalKick` is raised only by
  the perf overflow — a kick below the target is the overflow landing EARLY, the negative-skid
  case AA-1 exists to measure. And `rearm` is a no-op for the stock mechanism, so treating it
  as advisory hung the sample. The advisory path is now `Preempt`-only; a `SignalKick` is
  always the delivery.
- **Bind the kernel pin to the running kernel.** Hashing `/boot/Image` proved a FILE matched
  a pin, not that it was the image executing (a stale or newly-installed image hashes fine
  while another kernel is booted). The harness now reads the RUNNING kernel's GNU build-id from
  `/sys/kernel/notes` and requires a match with the operator's expected build-id
  (`--host-kernel-build-id`), and cross-checks the live `uname -r` against the environment
  block, before setting `verified_before_boot`.
- **Truth-table schema names PMCEID1 bit 1.** Event 0x21 is bit 1 of `PMCEID1_EL0`
  (`PMCEID0_EL0` covers only 0x00..0x1f); the `ident` payload already reads PMCEID1. The schema
  row id (`br-retired-pmceid0` → `br-retired-pmceid1`) and the two PMCEID0 descriptions were a
  regression, now corrected, and the harness `Capability::Pmceid` name matches.
- **Probe emits the complete `truth-table.json` artifact.** `probe` printed eight transient
  host-cap rows and wrote nothing. It now builds and writes the full AA-0 deliverable: all
  thirteen mandatory rows (host caps + the ID-register facts `ecv`/`lse`/`pmuver`/`sve`/
  `nested-virt`/`kvm-mode`, read from a disposable VM's vCPU), the machine identity (MIDR
  decoded + operator-supplied SoC/firmware + online cores + `uname`), and the core-assignment
  topology, to `--out`. A new `truth_table` module holds the schema-mirroring structs and the
  pure assembler; the RC is nonzero if any row deviates (a deviation, even a favourable one,
  needs an explicit ruling).
- **Hash all implemented vGIC routing state.** The digest hashed `GICD_IROUTER` only for SPIs
  32..47 and omitted IDs ≥96. It now derives the implemented range from `GICD_TYPER` and hashes
  every SPI's group/config/priority/routing/enable/pending/active, so two states differing in a
  higher SPI's injection cannot share a replay digest.
- **Realistic plan-memory ceiling (P2).** `MAX_PLANNED_SAMPLES` was 10⁹, which passed the bound
  then reserved ~64 GB in `Vec::with_capacity` and OOM-killed the process; it is now 10⁷ (~10×
  the AA-1 floor, well under a gigabyte), so a hostile `--reps`/`--cases` is a normal error.

No run-record/run-set schema change (the plan is not evidence); `truth-table.schema.json`'s
row-id/descriptions corrected. Gates re-run green: harness 97 tests, floor-check 47 + 27 + 3
(unchanged), oracle-model 22 + 2 (unchanged since r15), clippy `-D warnings` native +
aarch64-linux (the build-id, uname, ID-register and truth-table code compiles for the box),
three `cargo deny`, the opcode-checked window scan, TCG smoke, kernel-patch format/parse, and
Miri arm-harness.

## Round-17 review fixes (PR #108, cross-model pass r17)

Two P1 + five P2 — the review is narrowing.

- **AA-1 counting-only runs get a distinguished verdict, never a silent stage PASS.** A
  counting-only AA-1 run (zero armed, no floor) emitted no `ArmedOverflowFloor` outcome, so
  `accept-counting` exited `RESULT: PASS` while reporting zero armed overflows — but AA-1's
  stage acceptance is the ≥10⁶ armed-overflow floor. Any stage with a normative armed floor
  (AA-1, AA-3) now requires one; a counting-only AA-1(b) run reads NOT-REQUESTED on the floor
  (a distinguished sub-experiment verdict), its counting-mode checks stand on their own, and
  the overall RC is nonzero.
- **Machine-supported AA-0 deviation rulings.** `probe` failed on ANY deviation (even the
  explicitly allowed favourable one, ECV present) because it gated on "all confirmed", with no
  way to supply a ruling. `probe` now takes `--rulings` (a `{ row-id: ruling }` JSON map); a
  deviation WITH a recorded ruling carries it as its disposition and is acceptable, an unruled
  one keeps the placeholder and gates the RC. Acceptance is now over *unresolved* rows, not all
  deviations.
- **Effective KVM mode, not the VHE feature bit (P2).** The `kvm-mode` row read
  `ID_AA64MMFR1_EL1.VH`, which stays nonzero on an `nvhe`-booted host. It now reads
  `/sys/module/kvm_arm/parameters/mode`, the mode KVM actually selected.
- **Online-CPU count, not affinity-available (P2).** `core_count` used
  `available_parallelism()`, which reflects the process's affinity/cgroup allowance — under
  `taskset`/a systemd CPU set that records the lease size, not the Altra's topology. It now
  reads `/sys/devices/system/cpu/online`.
- **Oracle totals fail closed on overflow (P2).** `Expectation::total` saturated, so a
  malformed record with huge weights predicted `u64::MAX`, which a record whose `measured_taken`
  is `u64::MAX` (`work_begin = 0`, `work_end = u64::MAX`) then matched. It is now checked
  arithmetic returning `Option`; the checker reads `None` as a count-exactness failure.
- **PMU probe I/O errors propagate as unprobed (P2).** The PMCEID probe's `flatten()`/`if let
  Ok` silently discarded a dir-iteration or `events/br_retired` read error (`EACCES`/`EIO`),
  returning `Ok(false)` — absence. Only `NotFound` is now absence; other errors propagate as
  `Err` for an `unprobed` truth-table row.
- **Cross-check live identity into the manifest (P2).** `execute` recorded the operator's
  environment verbatim and verified only the kernel release, so stale MIDR/KVM-mode survived a
  reboot or a move to another machine. It now also requires the live `MIDR_EL1` and the
  effective KVM mode to match the environment block before writing any measurement.

`truth-table.schema.json` unchanged this round (the disposition path uses the existing
`disposition` field). Gates re-run green: harness 99 tests, floor-check 48 + 27 + 3,
oracle-model 22 + 2, clippy `-D warnings` native + aarch64-linux, three `cargo deny`, the
opcode-checked window scan, TCG smoke, kernel-patch format/parse, and Miri (arm-harness +
oracle-model).

## Round-18 review fixes (PR #108, cross-model pass r18)

Five P1 + two P2, concentrated in AA-5/AA-6 evidence completeness.

- **vGIC CPU-interface state in the digest.** `vgic_state` read only the redistributor and
  distributor save groups; two runs differing only in CPU-interface interrupt state (priority
  mask, group enables, active priorities) shared an AA-6 digest. It now also reads the
  `KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS` group — the `ICC_*` registers (`ICC_PMR_EL1`,
  `ICC_IGRPEN0/1_EL1`, `ICC_AP0R0/AP1R0_EL1`, the BPRs, CTLR, SRE), read 64-bit.
- **AA-6 compares the FINAL state digest.** `comparison_digest` selected `landed_digest` for
  every armed record, including AA-6, though AA-6's contract is the ordinary final
  `state_digest`: two reps can land identically at the injection Moment then diverge PROCESSING
  the event. The digest is now selected by stage — AA-3 compares the landing, AA-6 the final
  state.
- **AA-6 matrix coverage from injected records.** The matrix counted a class present even when
  all its records were unarmed, so a run could supply repeated unarmed records for the required
  classes, one armed class, and pass. Coverage is now built from ARMED, DELIVERED records — a
  class that injected nothing does not count.
- **Image pins bound to every exercised artifact.** The check passed on a single verified image
  and never bound the kernel identity. It now requires a verified image pin for every exercised
  payload class (file name = class name, including the AA-5/AA-6 Linux guest) AND a verified
  pin whose hash equals `mechanism.host_kernel_sha256`. Fixtures regenerated with per-payload
  pins; `build_run_set` binds the kernel image hash to the mechanism.
- **AA-5 requires replay identity.** AA-5 was omitted from `requires_replay_identity`, so a
  single work-derived clock-page record read PASS. AA-5 is now included, and its
  acceptance-bearing classes (`clock-page`, `linux-guest`) must be in repeated groups — a
  singleton reads NOT-REQUESTED.
- **Reject empty AA-0 rulings (P2).** `{"ecv": ""}` marked a deviation resolved because
  `unresolved` only recognised the placeholder. A ruling is now trimmed and rejected if empty,
  so an empty/whitespace ruling stays UNRULED and gates.
- **Drop the undefined AA-6 armed-overflow floor (P2).** AA-6 rode the patched mechanism, so it
  required an explicit `--min-armed-overflows` — but AA-6 defines only the ≥1000-rep floor and
  its arming is a matrix invariant, so valid armed AA-6 evidence read INCOMPLETE. The armed
  FLOOR is now required only by stages that DEFINE one (AA-1, AA-3); AA-6's arming is enforced
  by `check_aa6_matrix`, not a numeric floor.

`truth-table.schema.json` and the run-record/run-set schemas unchanged. Gates re-run green:
harness 100 tests, floor-check 53 + 27 + 3 (fixtures regenerated), oracle-model 22 + 2
(unchanged), clippy `-D warnings` native + aarch64-linux, three `cargo deny`, the
opcode-checked window scan, TCG smoke, kernel-patch format/parse, and Miri (arm-harness;
oracle-model unchanged this round, its r17 run stands).

## Round-19 review fixes (PR #108, cross-model pass r19)

Four fresh P1s (the fifth listed item, "run Miri for every unsafe payload crate", stays
adjudicated-settled — bare-metal `global_asm!` crates cannot build under Miri).

- **Retain the timer comparator (CVAL) in the digest.** `is_host_time_register` excluded
  `(crm, op2) == (0, 2)` as the live `CNTVCT_EL0`, but the KVM one-reg ABI REMAPS the timer
  pseudo-registers off the architectural encodings: id `ARM64_SYS_REG(3,3,14,0,2)` is
  `KVM_REG_ARM_TIMER_CVAL` (the guest's programmed virtual-timer deadline, deterministic
  state), and the live counter is `KVM_REG_ARM_TIMER_CNT` at `(3, 2)`. Two guests differing
  only in their timer deadline hashed identically. The filter now excludes only the live
  counters `(0,1) | (0,5) | (0,6) | (3,2)` and keeps `(0,2)`; the flagship digest test now
  also asserts a CVAL change DOES diverge.
- **Count only injected AA-6 repetitions toward the rep floor.** `check_rep_floor` counted every
  record in a `RepKey` group regardless of `overflow.armed`, so an AA-6 group of one injected
  record and 999 `armed: false` records sharing a target met a 1,000-rep floor without 1,000
  reps under injection. At AA-6 only armed, delivered records now count toward the floor (other
  stages that opt into a floor still count every record).
- **Require BOTH AA-5 classes.** `is_acceptance_bearing` filtered whichever AA-5 class happened
  to exist and never required both, so a run of repeated `clock-page` records with no
  `linux-guest` read a full PASS. AA-5 now has its own per-CLASS replay branch: both the
  clock-page and the Linux-guest class must be present AND repeated; a missing or singleton
  class reads NOT-REQUESTED (a divergent repeated group still fails).
- **Hold pinning fixed across aggregated normal runs.** Comparability omitted `pinning` for
  every stage, so two AA-3 half-million-deadline sets pinned to different cores could sum into a
  passing million-deadline verdict though the PMU/skid environment changed. Pinning is now part
  of comparability at every stage EXCEPT AA-1, whose sweep legitimately varies the pinning
  posture (the sanctioned bounded migration probe runs unpinned).

Miri item stays adjudicated-settled. Gates re-run green: harness 100 tests, floor-check 55 + 27
+ 3 (two new lib tests), oracle-model 22 + 2 (unchanged), clippy `-D warnings` native +
aarch64-linux, three `cargo deny`, the opcode-checked window scan, TCG smoke, kernel-patch
format/parse, and arm-harness Miri (floor-check is safe Rust — its unit suite is the gate
there; oracle-model unchanged, its r17 Miri stands).

## Round-20 review fixes (PR #108, cross-model pass r20)

Four P1 + one P2 (the sixth listed item, "add Miri paths for the unsafe payload crates",
stays adjudicated-settled — bare-metal `global_asm!` crates cannot build under Miri).

- **Decode FEAT_NV from the right field.** The AA-0 probe read `ID_AA64MMFR2_EL1[35:32]` (the
  `AT` field) as `NV`; NV is `[27:24]`. On an N1 with AT present but NV absent this reported
  nested virtualization present, blocking or bogusly deviation-ruling a mandatory row. Fixed to
  shift 24.
- **Measure churn only inside the armed interval.** The migration probe compared the churner's
  move count across the WHOLE sample — VM/vGIC creation, image load, perf setup, and guest boot
  all precede `arm_overflow`, and the churner moves every 200µs, so boot-time moves vacuously
  satisfied AA-1's required armed-migration probe. `run_sample` now snapshots the move count at
  the arm and again at the landing (via a new `ArmedMigrationProbe` threaded through
  `SampleSpec`), so only a move STRICTLY between `arm_overflow` and the landing counts.
- **Bound generated deadlines to each payload's window.** Every cell drew a delta in
  `1..=100_000`, but a delta past a payload's window branch budget never fires (the guest exits
  first, recording `deliveries: 0`) — fatal for WFI's shortened scales and any smoke run. The
  plan now clamps the drawn ceiling per (payload, scale) to `oracle_model::max_landable_delta`
  (half the trip count — a sound lower bound on window branches, ≥1 backedge/trip), so a
  deadline always lands inside.
- **Require replay identity for AA-4.** AA-4 (the LSE-only contract) was omitted from
  `requires_replay_identity` and the acceptance-bearing set, so a singleton AA-4 run took the
  generic PASS path without showing the LSE landing replays under the same injection schedule.
  AA-4 now requires a repeated armed landing like AA-3; a singleton reads NOT-REQUESTED.
- **Confine records_file to the run-set directory (P2).** `dir.join(records_file)` followed an
  absolute path or `..` out of the selected directory, so a manifest could pass every check on
  records outside the retained package (or an arbitrary external file). `load_run_set` now
  rejects any `records_file` that is empty, absolute, or contains `..` before touching the
  filesystem (`RecordsPathEscapesDir`).

Miri item stays adjudicated-settled. Gates re-run green: harness 101 tests, floor-check 57 + 27
+ 3, oracle-model 23 + 2 (all with new tests), clippy `-D warnings` native + aarch64-linux,
three `cargo deny`, the opcode-checked window scan, TCG smoke, kernel-patch format/parse, and
arm-harness Miri.

## Round-21 review fixes (PR #108, cross-model pass r21)

Four P1 + one P2 (the sixth listed item, "add Miri paths for every unsafe payload crate",
stays adjudicated-settled — bare-metal `global_asm!` crates cannot build under Miri).

- **AA-5 Linux guest must attest work-derived time.** `check_clockpage_mode` graded only
  `ClockPage` records, so a package with valid work-derived clock-page records plus Linux-guest
  records whose `clockpage_mode` was absent/self-seeded passed — presence of the guest stood in
  for evidence it consumed the clock. The graded set is now the clock-ATTESTING records
  (`ClockPage` **and** `LinuxGuest`); each must attest work-derived (or managed-static →
  NOT-REQUESTED). A clock-page record must still exist (the mechanism was exercised).
- **Align the PMCEID row with the code.** The harness validates `BR_RETIRED` via `PMCEID1`
  (event 0x21 is bit 1 of `PMCEID1_EL0`, which covers 0x20..0x3f; `PMCEID0_EL0` covers only
  0x00..0x1f), but the binding table (`docs/ARM-ALTRA.md`) and `schemas/README.md` still said
  `PMCEID0`. Both docs updated to `PMCEID1` with the bit rationale; the code (`sys.rs`, the
  `ident` payload, `truth-table.schema.json`) was already right. (`docs/ARM-ALTRA.md` is the
  repo-root binding doc, edited this round at the reviewer's + Paul's direction to keep the
  normative table and the code in agreement.)
- **Normal AA-1 sets share one pinning posture.** The r20 pinning comparability exempted the
  whole AA-1 stage, so two ordinary AA-1 condition sets on different cores could aggregate into
  one population. The exemption is now per-SET: only a `migration_probe` set may differ; every
  normal set (AA-1's condition sets included) is held to the first NON-probe set's posture — so
  a probe that sorts first cannot let the normal sets diverge.
- **Reject symlinked records files (P2).** The lexical `..`/absolute check passed a plain name
  that was a SYMLINK to a file outside the run-set directory, and `std::fs::read` followed it.
  `load_run_set` now canonicalizes both the directory and the records path (resolving symlinks)
  and requires the real file to stay beneath the real directory before reading.
- **Checked CPU-range arithmetic (P2).** `parse_cpu_list` did `b - a + 1` on host sysfs input;
  a range like `0-4294967295` overflowed and panicked in debug (and multiple ranges could
  overflow the running sum). It now uses checked arithmetic and returns `SysError::Protocol`.

Miri item stays adjudicated-settled. Gates re-run green: harness 101 tests, floor-check 58 + 27
+ 3, oracle-model 23 + 2 (unchanged this round), clippy `-D warnings` native + aarch64-linux,
three `cargo deny`, the opcode-checked window scan, TCG smoke, kernel-patch format/parse, and
arm-harness Miri.

## Round-22 review fixes (PR #108, cross-model pass r22)

Five P1 + one P2 (no Miri item this round).

- **Enumerate the whole writable-ID surface.** The probe returned `Ok(true)` on the first
  reducible `ID_AA64PFR0_EL1` nibble, so a host with a writable PFR0 but frozen
  `ISAR0`/`MMFR*`/`DFR0` was green-lit even though AA-6's shrunk-ID model needs the whole
  surface. It now runs the reduce-and-read-back probe (factored into
  `reduce_and_readback_id_field`) over every relevant register and requires each to accept a
  below-host value; a partial surface reads FALSE.
- **Complete AA-4 evidence set.** Any armed delivered overflow — straight-line included — was
  acceptance-bearing at AA-4, so repeated non-LSE landings could pass the atomics contract.
  `is_acceptance_bearing(Aa4)` now requires an armed **lse-atomics** landing, and a new
  `check_aa4_contract` reads NOT-REQUESTED enumerating AA-4's structured evidence (LSE
  invariance under injection, LL/SC divergence, the three enforcement levels, the recorded
  ruling) — the last three arrival-day and not representable in the record schema, so AA-4
  never PASSes pre-silicon.
- **Reject an all-probe AA-1 aggregate.** An aggregate where every set was a migration probe
  found no non-probe reference and skipped pinning entirely, while the condition matrix counted
  the probes' records. `check_aggregation` now requires at most one probe and at least one
  pinned set at AA-1, and `check_aa1_condition_matrix` EXCLUDES migration-probe records from
  both condition-coverage sums.
- **Reject records that mix step and overflow modes.** A record with both `step` and an armed
  `overflow` made `comparison_digest` early-return `step_digest` while divergent landed digests
  went unchecked. `comparison_digest` now selects by acceptance mode (an armed landing takes
  precedence over `step`, AA-6 excepted), and `check_well_formed` rejects any record carrying
  both.
- **Bind AA-2 step evidence to Debug exits.** `check_debug_evidence` accepted any `step:
  Some` record regardless of exit reason, so an `mmio`-labelled step matrix passed. Every step
  record must now carry a `Debug` exit, and a `Debug` exit with no step measurement is rejected.
- **Preserve a pending perf kick across foreign signals (P2).** The kick handler stored
  `from_fd` unconditionally, so a foreign SIGUSR1 racing in after a real overflow erased the
  pending kick (a hang / apparent lost PMI). It now only SETS the flag on a perf-sourced
  signal, leaving an already-pending `true` for the run loop (the sole consumer) to swap out.

Gates re-run green: harness 101 tests, floor-check 62 + 27 + 3, oracle-model 23 + 2 (unchanged
this round), clippy `-D warnings` native + aarch64-linux, three `cargo deny`, the opcode-checked
window scan, TCG smoke, kernel-patch format/parse, and arm-harness Miri.

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
