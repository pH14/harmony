# AA-2 single-step run path — implementation and verification record

`docs/ARM-ALTRA.md` §AA-2 characterizes stock `KVM_GUESTDBG_SINGLESTEP`
(`MDSCR_EL1.SS`/`PSTATE.SS`) exactness. The path described below was implemented and exercised
on N1. The retained physical transcript is schema v3; new runs use schema v4 so stable planned
sample identity is part of every step record.

## What a step record is

`evidence::StepRecord { planned_sample_id, step_index, pc_before, pc_after, insn_retired,
br_retired_delta, transition: StepTransition, step_digest }`, carried as
`RunRecord::step: Some(..)`, with
`exit_reason == Debug`. One record = **one measured step**. The checker
(`check_debug_evidence`, check.rs:2495) enforces, per record: `exit_reason == Debug`;
`pc_after != pc_before`; `insn_retired == 1`; and `br_retired_delta` vs the recorded
`transition` class —
- `Sequential`  → delta 0 **and** `pc_after == pc_before + 4` (fixed 4-byte insns; a
  larger jump = skipped insn, equal/smaller = doubled/stalled — the exact miss AA-2 hunts);
- `TakenBranch` → delta exactly 1;
- `NotTakenBranch` → delta exactly 1 and `pc_after == pc_before + 4` (AA1-F1: the branch
  instruction retires regardless of direction);
- `LlscExclusive` (LDXR/STXR) → delta 0 (a load/store, not a branch; the retry `CBNZ`
  steps as its own `TakenBranch`);
- `ExceptionEntry`/`ExceptionReturn`/`Wfi`/`Injection` → delta measured, bounded 0-or-1.
Coverage: an AA-2 run that never stepped an exclusive has not measured it
(`StepTransition::LlscExclusive` doc). `replay-identity` compares `step_digest` (the
state *at the step Moment*, not the exit sentinel — divergent step states can converge by
the sentinel).

## The three pieces to build

1. **KVM guest-debug ioctl seam** (`sys/machine.rs`). Only the *capability* exists
   (`kvm::CAP_SET_GUEST_DEBUG = 23`, probed in AA-0). Add:
   - `KVM_SET_GUEST_DEBUG` ioctl number `_IOW(KVMIO, 0x9b, struct kvm_guest_debug)` =
     **`0x4208_AE9B`** — arm64 `struct kvm_guest_debug` is 0x208 bytes (`control:u32 +
     pad:u32 + kvm_guest_debug_arch`, whose arm64 form is `dbg_bcr/bvr/wcr/wvr[16]` =
     64×u64 = 0x200), so the size field is 0x208, giving `_IOW(0xAE, 0x9b, 0x208)`.
     (An earlier draft of this line wrote the x86 value `0x4048_AE9B`, whose 0x48 is
     x86's smaller `kvm_guest_debug_arch` — the kernel dispatches on the full command
     number, so the arm64 size is required.) Pinned by a `size_of == 0x208`
     const-assertion; `TODO(box-verify)` confirms the running kernel accepts it.
   - `KVM_GUESTDBG_ENABLE = 0x1`, `KVM_GUESTDBG_SINGLESTEP = 0x2`; control =
     `ENABLE | SINGLESTEP`.
   - `Machine::arm_single_step()` (set the debug control once) and a `step_once()` that
     does one `KVM_RUN`, expects `KVM_EXIT_DEBUG`, and returns `(pc_before, pc_after,
     br_retired_delta)` by reading `PC` (one-reg `REG_ARM64_CORE + PC`) before/after and
     the work counter before/after. `insn_retired` is 1 by construction of a single step
     — but VERIFY on the box against the oracle (that is the AA-2 measurement).

2. **Transition classification** (reuses `scan.rs`, no duplicate decoder). Decode the word at
   `pc_before` (read 4 bytes of guest RAM at `pc_before - RAM_BASE`):
   - `scan::is_exclusive(word)` → `LlscExclusive`.
   - `scan::decode_branch(word)` `Some(kind)`: taken iff `pc_after == scan::branch_target(word, pc_before)`
     (for immediate branches) or `pc_after != pc_before + 4` (for register branches
     BR/RET); taken → `TakenBranch`, conditional fall-through → `NotTakenBranch`.
   - `SVC` (`word & 0xFFE0001F == 0xD4000001`) → `ExceptionEntry`; the abort payloads
     enter via a faulting load/store → also `ExceptionEntry` (detect by `pc_after` in the
     vector page, `VBAR`-based, rather than by opcode).
   - `ERET` (`0xD69F03E0`) → `ExceptionReturn`; `WFI` (`0xD503207F`) → `Wfi`.
   - injected-IRQ boundary (AA-6) → `Injection` (the step where an armed IRQ is taken;
     detect by `pc_after` in the IRQ vector while no synchronous cause).
   - else `Sequential`.
   Classification is a *hypothesis* the box measurement confirms; where the opcode and
   the observed `pc_after` disagree with the expected class, that disagreement **is** the
   AA-2 finding — record it, do not force it.

3. **Step-run mode + CLI** (`run.rs` + `arm_spike.rs`). The `step_run()` sibling of
   `run_sample`: arm single-step, loop `step_once()` emitting one `RunRecord` per step
   (with its `StepRecord`) until the console sentinel, on a **smoke-scale** payload (a
   1e6 payload is millions of steps — smoke keeps a full stepped run to ~10⁴ records).
   Gate it behind `--single-step` (or `--stage aa2` implying it); the counting loop stays
   untouched (it must keep refusing an unrequested debug exit). Emit the run-set exactly
   as today so `floor-check --stage aa2` grades it.

## Tests (native, before the box)

- Scripted-vCPU tests (the `run.rs` pattern): feed a scripted sequence of `KVM_EXIT_DEBUG`
  exits with known PC deltas + opcodes and assert the emitted `StepRecord`s classify and
  validate (each of the six transition classes; a skipped-insn scripted step must FAIL
  `Sequential`'s `pc+4`; a doubled step must fail `insn_retired==1`).
- Miri over the new pointer path (reading the opcode word from guest RAM), per the
  unsafe⇒Miri bar, using the existing `guest_ram` seam.
- `floor-check`: extend the AA-2 accept fixture to a valid stepped run (currently AA-2 is
  NOT-REQUESTED because no run emits steps); keep a reject fixture for each malformed step.

## Box validation (AA-2 proper, when the lock frees)

Stock KVM, pinned core 60, smoke payloads. The real N1 run confirmed exactly one instruction
per step vs the oracle; `BR_RETIRED` moves on every stepped branch instruction, taken or not,
and not on ordinary non-branches; step behaviour
across `SVC`/abort entry, `ERET`, `WFI`, and — deliberately — **stepping an LL/SC
sequence** (does each step clear the monitor and livelock the retry? that is direct AA-4
input). Acceptance: exact step counts vs oracle across all classes; replay-identical
`step_digest` across repeated inputs; the LL/SC-stepping behaviour documented. Stop/REDESIGN
if stepping skips/doubles instructions or interacts nondeterministically with injection —
AA-3 depends on a trustworthy step primitive.
