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

## The candidates, ranked — with the on-silicon data (`harness/singlestep-driver.c`)

Measured on the box, pinned core 2, sibling idled, LS_CFG attested
(`results/ae-2/{tf,btf,tfg}-*.json`), against the by-construction oracle. Payloads:
`nop_sled` (straight-line), `loop` (conditional branch), `jmp_chain` (branch-dense),
`sti_shadow` (STI interrupt-shadow), `movss_shadow` (MOV SS shadow).

### (B) `RFLAGS.TF` via `KVM_GUESTDBG_SINGLESTEP` — instruction single-step — **CHOSEN**
Measured `#DB` (`KVM_EXIT_DEBUG`) exits vs oracle instruction count:

| payload | oracle instr | `#DB` exits | exact | note |
|---|---|---|---|---|
| `nop_sled` (64) | 64 | 64 | ✅ | straight-line exact |
| `loop` (64) | 129 | 129 | ✅ | conditional branch exact |
| `jmp_chain` (64) | 64 | 64 | ✅ | branch-dense exact |
| `sti_shadow` | 3 | 3 | ✅ | **STI shadow does NOT defer the `#DB`** (STI blocks IRQs, not `#DB`) |
| `movss_shadow` | 3 | **2** | ❌ | **MOV SS blocks `#DB` for one instruction → coalesced step (the §1B hazard, real)** |

Two decisive favorable findings resolve the doc's TF worries:
- **Guest-`TF`-visibility is a NON-issue here.** In every run `guest_tf_kept == 0`: KVM's
  `KVM_GUESTDBG_SINGLESTEP` is **transparent** — it manages `TF` invisibly (the guest's
  `RFLAGS.TF` reads 0), exactly the invisibility MTF has by construction. The §1(B)
  contract leak (`PUSHF` exposing `TF`, `POPF` disarming the stepper) does **not** occur:
  the guest never sees KVM's stepping `TF`. (A direct guest-`PUSHF` confirmation is the
  one residual, but the register readback already shows KVM hides it.)
- **The interrupt-shadow hazard is split, not fatal.** `STI` shadow steps exactly (3/3);
  only the `MOV SS`/`POP SS` shadow coalesces a `#DB` (it architecturally blocks debug
  exceptions for the shadowed instruction — AMD64 APM). This is a **bounded, recorded**
  hazard: the landing loop detects a `MOV SS`/`POP SS` boundary and accounts for the
  suppressed step (or the frozen guest kernel, which controls its own code, does not land
  inside a `MOV SS` shadow). It is not a general miscount.

### (A) `DebugCtl.BTF` — branch single-step — **REJECTED (unavailable through stock KVM)**
The provisional lead, refuted by data. Arming the guest's own `RFLAGS.TF` +
`DebugCtl.BTF` under `KVM_GUESTDBG_ENABLE` (no `SINGLESTEP`) delivered **zero
`KVM_EXIT_DEBUG` across every payload** (`all_db_zero == true`) even though the guest's
`TF` was kept (`guest_tf_kept == 1`). The `tfg` diagnostic (guest `TF`, no BTF) is
identical: **stock KVM does not report a guest-self-induced `TF`/BTF `#DB` to userspace
under SVM** — it manages debug exceptions only for its own `SINGLESTEP`/hardware-breakpoint
paths. So BTF branch-granularity, however elegant (its granularity == the `ex_ret_brn_tkn`
event), is **not a stock capability**: realizing it is itself patch-0005-analogue kernel
work (a new `KVM_GUESTDBG_BTF` path wiring guest `DebugCtl.BTF` `#DB` → `KVM_EXIT_DEBUG`),
not a config. Recorded rejected-mode: *BTF #DB not delivered via stock KVM guest-debug on
SVM.*

### (C) `#DB` + DR7 breakpoints — targeted-landing aid (unchanged): known-RIP, 4 slots; our target is a work count, not an address. A re-arm assist, not the stepper.
### (D) instruction-retired PMC step — last resort (unchanged): PMC overflow inherits the AE-1 skid (max 5043); cannot land exactly. Only if all else fails.

## Ruling

**The patch-0005 analogue uses `RFLAGS.TF` instruction-granularity single-step via the
`KVM_GUESTDBG_SINGLESTEP` semantics** (KVM-transparent `TF`), **not BTF.** The doc's
BTF-first lean was the right *analysis* (branch granularity matches the event) but is
**refuted on silicon**: BTF is not deliverable through stock KVM, whereas TF is exact and
guest-transparent. The landing strategy is **overflow-early (arm the work counter to stop
before the target, at target − skid_margin per AE-1's ~16384) then TF-step the residual**,
reading the `ex_ret_brn_tkn` work counter after each instruction step until it equals the
target — sound because the work counter advances 0-or-1 per instruction (AE-1). The
`MOV SS`/`POP SS` shadow is the one recorded stepping hazard the landing loop accounts for.

**Disposition: PROVISIONAL GO** — a single-step primitive that is exact and
guest-transparent under SVM exists and is ruled (TF); the two rejected candidates' failure
modes are recorded (BTF unavailable via stock KVM; MOV SS shadow coalesces a step). No kill
condition fires (TF lands exactly given the mov-ss discipline; TF is hidden, so the guest
cannot disarm it). Standing conditions: (1) the landing loop handles the MOV SS/POP SS
shadow; (2) BTF, if ever wanted for fewer residual steps, is a kernel change, not stock.
Remaining for the AE-3 integrated harness: the full `work == target` landing (single-step +
work-counter, the `CpuBackend` inversion) and the syscall/exception/`iret`/injected-interrupt
classes (need the protected/long-mode guest with an IDT — the AE-5 Subject), plus the direct
guest-`PUSHF` confirmation of TF-invisibility.
