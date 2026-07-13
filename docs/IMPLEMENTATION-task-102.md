# Task 102 — implementation notes: `docs/AMD-EPYC.md` (bead `hm-wv8`)

Doc-only task. One new file, `docs/AMD-EPYC.md` — the AMD sibling of `docs/NESTED-X86.md` and
`docs/ARM-ALTRA.md`: a risk-ordered, GO/NO-GO-gated spike program for the AMD cells of the reach
matrix, targeting an incoming Epyc server whose exact Zen generation stage AE-0 discovers and
pins. All six content requirements from the spec are front-and-center in the spec's order
(§1–§6 of the doc); the stages are AE-0…AE-6; the PR-98 evidence-integrity countermeasures are a
binding section every stage's acceptance inherits verbatim.

## The organizing decision (for reviewer veto)

**The doc's spine is "same ISA → substrate deltas," stated in the first two paragraphs and
carried throughout.** Unlike ARM (a genuinely new architecture with an untrappable-counter
centerpiece), Intel↔AMD share the `Arch` vocabulary; the Arch seam does *not* split them. I made
this the opening thesis and derived its consequences everywhere: no new `Arch` is created (AMD is
a second *substrate* under the existing R-Backend seam); the x86 engine/boot/LAPIC/payloads carry
over; the guest images (pr44 postgres pair) are reused unchanged; the time-virtualization story
(RDTSC/RDRAND trap-and-map) is inherited from Intel and is explicitly **not** a load-bearing
question (the paravirt clock is an AMD *performance* lever à la NESTED-X86 N-4, not a correctness
necessity as it was on ARM). This is the single most important framing difference from
ARM-ALTRA and the spec calls for it explicitly ("say this explicitly and early").

## Judgment calls

1. **Stage naming AE-0…AE-6** (AMD Epyc), parallel to ARM's AA-0…AA-6 and NESTED's N-0…N-5.
   Seven stages: AE-0 bring-up/Zen-generation truth table, AE-1 work clock (existential trio +
   SpecLockMap probe), **AE-2 single-step without MTF (the hard one, its own stage)**, AE-3
   `svm.c` force-exit + exact landing, AE-4 contract vendor column + enforcement truth table,
   AE-5 bare-metal mini determinism gate (the AMD×metal GO, with a one-command demo), AE-6 nested
   SVM (gated on AE-5). Ordering rationale: AE-1 stays the immediate-focus existential
   measurement; the single-step question, though the *hardest*, depends on nothing AE-1 doesn't
   and is characterizable offline, so it sits at AE-2 and can proceed in parallel, with AE-3's
   landing loop consuming its ruling.

2. **Single-step (question 1) gets the most care, argued from AMD64 APM specifics, not analogy.**
   Four candidates ranked with per-candidate APM chapter + mechanism + failure modes:
   (A) `DebugCtl.BTF` branch-single-step as the *possible natural fit* — our V-time event is the
   retired taken branch, so BTF lands on the granularity we count; (B) `RFLAGS.TF` classic
   single-step as the general primitive, with the three concrete hazards spelled out
   (interrupt-shadow deferral via VMCB `GUEST_INTERRUPT_SHADOW`; `iret`/exception boundaries; and
   **guest visibility of TF via PUSHF/POPF as a contract leak**, not merely a counting bug — MTF
   is invisible, TF is not); (C) `#DB` + DR7 as a targeted-landing *aid* not a stepper (address-
   based, our target is work-based, only 4 slots); (D) instruction-retired PMC-step as the
   *skid-limited* fallback (inherits the same skid we fight for the work clock, so it cannot land
   exactly). APM citations are chapter-level (Vol 2 ch. 13 Debug/Perf, ch. 15 SVM) plus the named
   mechanism, deliberately not sub-section numbers (those drift across APM revisions and asserting
   a wrong §number would be worse than a chapter cite). AE-2's mandatory deliverable is the
   **ranked ruling** ("TF and hope" explicitly disallowed).

3. **`ex_ret_brn_tkn` event encoding is pinned per Zen generation at AE-0, never hard-asserted.**
   The spec says the incoming SKU is unknown and AA-0-equivalent discovers it; I therefore refer
   to the encoding as "`0xC4`-family, pinned per generation" rather than committing to a single
   raw value, and made the per-generation encoding a first-class AE-0 deliverable. Same treatment
   for the PMU *programming model*: legacy per-counter `PERF_CTL`/`PERF_CTR` EN-bit vs PerfMonV2
   (Zen 4+) global control — discovered at AE-0, a per-generation contract fact, not an AMD
   constant.

4. **SpecLockMap/`LS_CFG` is the AMD structural twin of ARM's rr #3607 pinning-as-correctness.**
   rr disables speculative locking via an `LS_CFG` MSR write to stop retired-branch overcount on
   locked instructions; I made this a standing host-side condition recorded in the box baseline
   (the AMD analogue of the Intel box's revert-KVM-to-stock discipline), with AE-1's bounded
   workaround-**off** probe reproducing the overcount to evidence the mitigation — mirroring how
   ARM AA-1's bounded migration probe evidences its pinning condition. "Requires `LS_CFG`" is a
   recorded PROVISIONAL-GO condition, not a NO-GO.

5. **AVIC-off is a recorded limitation, not a NO-GO — the spec's own example.** §3 argues from
   the mechanism (AVIC accelerates interrupt delivery *bypassing the VM-exit* our force-exit
   depends on, and moves interrupt state to a hardware page), concludes AVIC likely gets disabled,
   and says why disabling it is free for a design that already keeps the interrupt fabric in
   userspace on Intel. The bet section and decision ladder both encode "works only with AVIC off"
   as a recorded standing condition under PROVISIONAL GO.

6. **The 0004-analogue is on `svm.c` (not `vmx.c`); the patch shape largely transfers.** §3 states
   what changes (PMI delivery via LVTPC, the hook at SVM's exit/VMRUN boundary, AVIC in the way)
   and what carries (the determinism ABI is substrate-agnostic above interrupt-delivery detail).
   AE-3 owns the trait-freeze memo `docs/ARCH-BOUNDARY.md` deferred, now framed as "does
   late-only-stop hold on SVM PMI delivery."

7. **Contract = a vendor COLUMN on the one contract, never a fork.** §4 enumerates the concrete
   deltas (AuthenticAMD vendor string + `0x8000_00xx` extended leaves; the AMD MSR set incl.
   `LS_CFG`/`HWCR`/the already-denied SVM `VM_HSAVE_PA` at 0xc0010117; PERF_CTL/PERF_CTR vs
   PERF_GLOBAL_CTRL; the VMCB MSR-permission-bitmap as the enforcement backend vs
   `KVM_X86_SET_MSR_FILTER`) and cross-references the GLOSSARY never-fork-the-one-reproducer rule.
   I verified against `docs/CPU-MSR-CONTRACT.md` that AMD/SVM MSRs are *today* default-denied as
   out-of-scope on the `det-cfl-v1` Intel baseline (e.g. the `VM_HSAVE_PA` row at line 1454) — the
   AMD column *flips* those rows under an AuthenticAMD baseline rather than authoring a second
   document. The full AMD contract doc is downstream port work; AE-4 delivers the enforcement
   truth table + column skeleton only.

8. **Nested SVM (question 5) is a deferred, GO-gated stage (AE-6), not an omission.** Unlike ARM
   (nested deferred *entirely* — N1 has no nested-virt hardware), KVM's nested SVM is mature, so
   the AMD×virtualized cell is reachable; I transposed NESTED-X86's program shape (the three
   by-construction survivors + the vPMU-through-one-layer bet + the portability gate) and gated it
   on the AE-5 bare-metal GO, per the reach-matrix sequencing the spec names.

9. **Definition of done reaches a working demo (like NESTED-X86 N-5), not mechanism-only (like
   ARM).** Because the x86 stack already exists, AE-5 can ship a one-command build-boot-gate demo
   of the AMD×metal cell (the spec's "one-command demo stage"), and AE-6 its nested twin — where
   ARM's AA-6 could only deliver a mini-gate proof-of-mechanism because the ARM backend didn't
   exist yet.

10. **SMT caveat in box discipline.** Epyc cores are SMT-2 (unlike ARM N1's single-threaded
    cores), so the sibling-hyperthread confound class the Intel box also has is present; the box
    discipline requires idling/offlining the measurement core's SMT sibling and recording its
    state per run. Otherwise the box discipline is rewritten for a fresh un-provisioned server
    (baseline-manifest-as-restore-target, provisioning part of AE-0), `AMD_BOX_SSH` proposed
    alongside `DET_BOX_SSH`, and the pkill/content-hash/reachability/state-based-wait rules
    transfer verbatim.

11. **Evidence-integrity section inherited verbatim** (spec: "inherit its evidence-integrity
    section verbatim as binding stage criteria"). The six countermeasures (gate-RC propagation,
    machine-checked floors, content-hash-verified boots, mechanism attestation, independent
    oracle, multiplicity+totality) are copied with only incidental noun swaps (`kvm_amd`, the AVIC
    posture, the single-step primitive as attested mechanisms). "No Beads during execution",
    worktree/serialization/smoke-once, and the decision ladder are carried from the siblings.

## Gate verification

- **Doc exists**: `docs/AMD-EPYC.md`.
- **"vendor" never "personality"**: grep confirms "personality" appears only inside the rename
  ruling itself (the vocab note and the ARCH-BOUNDARY read-as reference) — which the spec itself
  instructs the doc to note; every role use says vendor (34 "vendor" occurrences).
- **Single-step argues from SVM architectural specifics, not hand-wave**: §1 cites the APM
  chapter + mechanism for each of the four candidates (BTF, TF, `#DB`/DR7, PMC-step) with their
  failure modes and states which stage (AE-2) produces the deciding evidence.
- **Internal consistency** (checked against the named siblings): patch identities (0004 force-exit
  `KVM_ARM_PREEMPT_EXIT`→`KVM_EXIT_PREEMPT`; 0005 MTF `KVM_ARM_MTF_STEP`→`KVM_EXIT_DET_STEP`) from
  NESTED-X86/ARCH-BOUNDARY; Intel event `0x1c4` = `BR_INST_RETIRED.CONDITIONAL`; `det-cfl-v1` =
  Coffee Lake i9-9900K; `CpuBackend` monotonic 0-or-1/instruction `u64` and its `planner.rs`
  home; the R-Backend substrate seam vs the Arch ISA seam; `WorkSource` trait seam; the
  `live_dirty_remap.rs` `guest_images()`/`verify_pin` content-hash pattern; the pr44 postgres
  pair; the task-69 M2 divergence-is-P0 principle — all used consistently.
- **PR opening**: left to the foreman — this worker's instructions are no-push, and a PR requires
  the branch on origin. Branch `task/amd-vendor-spike-doc` is ready to push as-is.

## Known limitations / for the integrator

- Hardware specifics that only silicon can settle (exact Epyc SKU/Zen generation, the pinned
  `ex_ret_brn_tkn` encoding, PerfMonV2-vs-legacy PMU model, AVIC presence/posture, VMCB feature
  surface, whether the delivered kernel's `kvm_amd` already exposes the intercept controls the
  patch needs) are deliberately written as AE-0 expect-vs-found rows, not asserted facts.
- The single-step ruling (§1/AE-2) is the mechanism most likely to force a REDESIGN; the doc
  ranks the candidates and names the deciding evidence but cannot pre-decide the winner without
  silicon. If BTF proves the natural fit, the landing loop and the counted event coincide — worth
  flagging to whoever executes.
- APM citations are chapter-level by design (sub-section numbers drift across APM revisions);
  the executor should resolve them against the specific APM edition in hand.
- `docs/BOX-PINNING.md` gains its Epyc standing-assignments section only at AE-0 (needs real
  topology + SMT map); this doc says so rather than pre-inventing a core map. `AMD_BOX_SSH` is
  proposed alongside `DET_BOX_SSH` but BOX-PINNING.md is not edited here.
- Bead `hm-wv8` closes on merge (foreman-owned per the task spec). The paravirt clock
  (`docs/PARAVIRT-CLOCK.md`/`hm-8h8`) is referenced as an out-of-scope AMD performance lever, not
  validated by this program.
</content>
