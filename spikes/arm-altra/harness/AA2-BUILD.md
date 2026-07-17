# AA-2 single-step run path — build plan (arrival-day executor work)

`docs/ARM-ALTRA.md` §AA-2 characterizes stock `KVM_GUESTDBG_SINGLESTEP`
(`MDSCR_EL1.SS`/`PSTATE.SS`) exactness. The offline apparatus deliberately did **not**
build the stepping run path — it would presume AA-2's own unknown single-step result
(the pre-build ruling forbids inventing AA-1/AA-2 unknowns; the counting loop
`run.rs` even *refuses* an unrequested `KVM_EXIT_DEBUG`, and `check_debug_evidence`
reads AA-2 as `NOT-REQUESTED` until real stepped records exist). This is the work to do
when AA-1(c) frees the box. It is offline-buildable and native-testable against the
scripted-vCPU seam; only the *measured semantics* need the box.

## What a step record is (already defined — do not change)

`evidence::StepRecord { pc_before, pc_after, insn_retired, br_retired_delta,
transition: StepTransition, step_digest }`, carried as `RunRecord::step: Some(..)`, with
`exit_reason == Debug`. One record = **one measured step**. The checker
(`check_debug_evidence`, check.rs:2495) enforces, per record: `exit_reason == Debug`;
`pc_after != pc_before`; `insn_retired == 1`; and `br_retired_delta` vs the recorded
`transition` class —
- `Sequential`  → delta 0 **and** `pc_after == pc_before + 4` (fixed 4-byte insns; a
  larger jump = skipped insn, equal/smaller = doubled/stalled — the exact miss AA-2 hunts);
- `TakenBranch` → delta exactly 1;
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
     `0x4048_AE9B` (verify the struct size on the box: `control:u32 + pad:u32 + arch`;
     arm64 `kvm_guest_debug_arch` is `dbg_bcr/bvr/wcr/wvr[16]` = 64×u64 → total 0x208;
     pin it with a size assertion like the other ioctls).
   - `KVM_GUESTDBG_ENABLE = 0x1`, `KVM_GUESTDBG_SINGLESTEP = 0x2`; control =
     `ENABLE | SINGLESTEP`.
   - `Machine::arm_single_step()` (set the debug control once) and a `step_once()` that
     does one `KVM_RUN`, expects `KVM_EXIT_DEBUG`, and returns `(pc_before, pc_after,
     br_retired_delta)` by reading `PC` (one-reg `REG_ARM64_CORE + PC`) before/after and
     the work counter before/after. `insn_retired` is 1 by construction of a single step
     — but VERIFY on the box against the oracle (that is the AA-2 measurement).

2. **Transition classification** (reuse `scan.rs`, no new decode). Decode the word at
   `pc_before` (read 4 bytes of guest RAM at `pc_before - RAM_BASE`):
   - `scan::is_exclusive(word)` → `LlscExclusive`.
   - `scan::decode_branch(word)` `Some(kind)`: taken iff `pc_after == scan::branch_target(word, pc_before)`
     (for immediate branches) or `pc_after != pc_before + 4` (for register branches
     BR/RET); taken → `TakenBranch`, else `Sequential`.
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

3. **Step-run mode + CLI** (`run.rs` + `arm_spike.rs`). A `step_run()` sibling of
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

Stock KVM, pinned core 60, smoke payloads. Confirm on real N1: exactly one instruction
per step vs the oracle; `BR_RETIRED` moves only on stepped taken branches; step behaviour
across `SVC`/abort entry, `ERET`, `WFI`, and — deliberately — **stepping an LL/SC
sequence** (does each step clear the monitor and livelock the retry? that is direct AA-4
input). Acceptance: exact step counts vs oracle across all classes; replay-identical
`step_digest` across repeated inputs; the LL/SC-stepping behaviour documented. Stop/REDESIGN
if stepping skips/doubles instructions or interacts nondeterministically with injection —
AA-3 depends on a trustworthy step primitive.
