<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AE-2 — single-step exactness without MTF: candidate analysis + ranked ruling

`docs/AMD-EPYC.md` §1 / Definition-of-done #3. The hardest mechanism in the program:
SVM has **no Monitor Trap Flag** (an Intel VMX-only control), so patch 0005's
MTF-based single-step (`KVM_EXIT_DET_STEP`) has no drop-in analogue. This stage rules
which `#DB`-based primitive the patch-0005 analogue implements, with the rejected
candidates' failure modes recorded — "TF and hope" is explicitly not a ruling.

## Hardware surface confirmed at AE-0 (this Zen 2 part, Ryzen 3600)

From `results/ae-0/capability-truth-table.json` (`single_step_surface`, `svm`):

| facility | present | bearing on AE-2 |
|---|---|---|
| `RFLAGS.TF` (instruction single-step) | yes (architectural) | candidate (B) |
| `DebugCtl.BTF` (branch single-step) | yes (architectural) | candidate (A) — the natural fit |
| DR0–DR3 / DR7 hardware breakpoints | 4 slots | candidate (C), targeted-landing aid only |
| NRIP save (`nrip_save`) | **yes** | exact next-RIP on intercept → clean re-arm after each `#DB` |
| DecodeAssists (`decode_assists`) | **yes** | faulting-instruction decode aid on intercepts |
| retired **taken**-branch event `ex_ret_brn_tkn` | exact (AE-1) | BTF's granularity == the V-time event's granularity |

The two favorable SVM features (NRIP-save, DecodeAssists) materially help a `#DB`-based
stepper: NRIP-save gives the exact address to resume/re-arm at after each single-step
`#DB` intercept, which is the re-arm hazard MTF papers over on Intel.

## The candidates, ranked (APM Vol 2 ch. 13/15)

### (A) `DebugCtl.BTF` — branch single-step — **PROVISIONAL LEAD**
With `DebugCtl.BTF=1` and `RFLAGS.TF=1`, `#DB` is raised only on the next **taken
branch**. This is the elegant fit: the V-time work event **is** the retired taken
branch (`ex_ret_brn_tkn`, AE-1), so "step to the next counted event" and "count one work
unit" become the same operation — the landing loop advances to a taken-branch boundary
near the overflow, exactly BTF's native behavior. NRIP-save (AE-0) gives the clean
re-arm point.
*Failure modes to characterize on-box (AE-2 driver):* BTF is **branch** granularity, so a
target between two taken branches needs a TF-residual finish (the §1(A) "BTF + TF
residual" landing); interrupt-shadow (`STI`/`MOV SS`) and `iret` `#DB` hazards apply as
to TF; and whether BTF's `#DB` reaches the VMCB `#DB` intercept **deterministically
across the VMRUN boundary** (and interacts cleanly with the guest's own `DebugCtl`) is
the open empirical question.

### (B) `RFLAGS.TF` — instruction single-step — **fallback / residual stepper**
`TF=1` raises a trap `#DB` after every instruction — the instruction-granularity
workhorse, used for the sub-branch residual under (A). Its hazards are the reason MTF
exists and each is a concrete risk here:
- **Interrupt-shadow deferral** after `MOV SS`/`POP SS`/`STI` (VMCB
  `GUEST_INTERRUPT_SHADOW`): the step `#DB` is deferred one instruction; a naive loop
  miscounts across the shadow.
- **`iret`/exception-entry** perturb `TF`/pending-`#DB`; each must be shown to step
  exactly once vs the oracle or be characterized.
- **Guest visibility of `TF` (a contract leak, not just a counting bug):** `PUSHF`
  exposes `TF`, `POPF` lets the guest clear it — a guest can branch on it (breaking the
  clean-architectural-state guarantee) or disarm the stepper. MTF is invisible by
  construction; TF is not. AE-2 must state whether SVM lets us **hide** it (intercept
  `PUSHF`/`POPF`, or a virtualized-`TF` posture) or whether it is a recorded contract
  limitation the frozen guest is built not to depend on — tested, not asserted.

### (C) `#DB` intercept + DR7 hardware breakpoints — targeted-landing aid, not a stepper
A DR0–DR3 breakpoint fires `#DB` at a **known RIP**. Useful to pin a landing at a
statically-known address or re-arm at a known re-entry point, but our target is a **work
count, not an address**, and there are only 4 slots. Ranked below A/B as a primary
mechanism; a useful assist.

### (D) instruction-retired PMC single-step — skid-limited last resort
Arm a second PMC (retired ops) to overflow after one, take the PMI, exit. **Fatal for
exactness:** PMC overflow is skid-prone — AE-1 measured the Zen HW PMI skid at **~1480
retired taken branches** (constant across periods), so a PMC-step cannot land exactly; it
is viable only as a bracket-and-re-measure fallback that pays for that skid, and only if
A and B both fail.

## Provisional ruling and the gating empirical step

**Provisional lead: (A) BTF as the primary step-to-next-work-unit primitive, with (B) TF
as the sub-branch residual finisher** — the §1(A) "BTF + TF residual" landing, which
aligns the stepper's granularity with the V-time event. This lean is **analysis, not yet
a ruling**: per the doc it is not ratified until `harness/singlestep-driver.c`
characterizes both primitives against the analytical oracle **under SVM guest context**
across straight-line / branch-dense / syscall / exception-entry / `iret` /
interrupt-shadow / injected-interrupt boundaries, and records the guest-`TF`-visibility
disposition. The apparatus is built (`harness/singlestep-driver.c`); the on-silicon
characterization is the remaining AE-2 box step, and until its records exist this file
records a **candidate ranking with the rejected modes**, not a final ruling.

**Disposition: REDESIGN-pending-characterization** — the primitive exists in hardware
(AE-0), the lead is well-founded, but the ranked ruling awaits the SVM `#DB` boundary
data. No kill condition is triggered (both A and B are present and plausible; only their
joint failure under SVM would trigger §Bet's single-step kill condition).
