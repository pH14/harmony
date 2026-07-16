# Task 102 — AMD vendor spike program doc: SVM on Epyc

Bead: `hm-wv8` (P2). Doc-only task: author `docs/AMD-EPYC.md`, the AMD sibling of
`docs/NESTED-X86.md` and `docs/ARM-ALTRA.md` — a risk-ordered, GO/NO-GO-gated spike program
for the AMD cells of the reach matrix. Target hardware: an incoming **Epyc server** (exact
SKU unknown — the doc's AA-0-equivalent stage discovers and pins the microarch/Zen
generation). Execution gated on hardware arrival; day one must be experiment day.

## Read first (binding)

`docs/ARM-ALTRA.md` — the freshest sibling: inherit its **evidence-integrity section
verbatim as binding stage criteria** (gate-RC propagation, machine-checked floors,
hash-verified boots, mechanism attestation, independent oracles, multiplicity+totality
accounting) and its structural shape (load-bearing questions → bet/kill conditions →
execution constraints → box discipline → stages → decision ladder → execution packet).
`docs/NESTED-X86.md` (the x86 sibling), `docs/ARM-PORT.md` (mechanism-analysis style),
`docs/ARCH-BOUNDARY.md` (the seam; **"vendor" never "personality"**). Note: Intel↔AMD is
the SAME ISA — the Arch seam does NOT split them; the vendor split lives in the
substrate/backend (VMX vs SVM), the contract tables, and the PMU event pins. Say this
explicitly and early, and derive its consequence: most of the x86 engine, boot path, and
payloads carry over — the spike measures the substrate deltas, not a new architecture.

## The load-bearing questions (front and center, risk-ordered)

1. **Exact landing without MTF (the hard one — give it the most care).** SVM has **no
   Monitor Trap Flag**; the patch-0005 analogue needs a different single-step primitive.
   Enumerate and rank the candidates with their failure modes: RFLAGS.TF-based stepping
   (interrupt-shadow/`iret`/`mov ss` hazards, guest visibility of TF via `pushf`, the
   virtualized-TF cleanliness question under SVM), `DebugCtl.BTF` (branch single-step —
   possibly the natural fit for a branch-counted clock), #DB intercepts + DR7 controls,
   and instruction-retired PMC single-stepping as a fallback. State what evidence decides
   between them and which stage produces it.
2. **The work clock on Zen.** Retired-branch counting is rr-proven on Zen, but with known
   erratum workarounds — the SpecLockMap / lock-instruction overcount issue (rr disables
   speculative lock-map via an MSR write, `LS_CFG`), and per-generation event-encoding
   differences (`ex_ret_brn_tkn` vs Intel's `0x1c4`; note it counts *taken* branches —
   the SAME semantic family as ARM's BR_RETIRED, different from Intel's conditional
   branches — so the ARM re-measurement discipline applies: every skid/density constant
   re-measured, never inherited). The doc pins per-Zen-generation event identity as an
   AA-0-equivalent deliverable.
3. **The 0004-analogue (deterministic in-kernel force-exit at PMI)** on the SVM side of
   KVM: what the patch touches (svm.c vs vmx.c), whether the existing patch's shape
   transfers, and the PMI→exit delivery path differences (SVM's virtualized LAPIC /
   AVIC considerations — AVIC likely gets disabled; say so and why).
4. **Contract deltas.** AuthenticAMD CPUID leaves (0x8000_00xx space), the AMD MSR set,
   PERF_CTL/PERF_CTR vs Intel's PERF_GLOBAL_CTRL programming model, and the
   default-deny consequences (the frozen contract doc gets a vendor column, not a fork —
   never fork the one-reproducer artifact; cross-reference the GLOSSARY rule).
5. **Nested SVM as the virtualized-form cell.** KVM's nested SVM is mature (unlike
   nested-ARM); the NESTED-X86 program shape transposes nearly verbatim, including its
   vPMU-through-one-layer bet. Defer it to a separate follow-on stage gated on the
   bare-metal GO — same sequencing the reach matrix uses everywhere.
6. **Fallbacks and siblings**: rentable AMD metal (Hetzner AMD lines) as the
   zero-procurement fallback; the qualification harness carries over from the doctor/
   preflight work when that lands.

## Structural requirements

Inherit ARM-ALTRA.md's structure and rigor wholesale: bet + kill conditions (one
unexplained count mismatch is blocking; "works only with AVIC off" is a recorded
limitation, not a NO-GO), definition of done (dispositions + machine-readable evidence +
a one-command demo stage), execution constraints (worktree, no-Beads-during-execution,
serialization, smoke-once), box discipline adapted for a fresh un-provisioned server
(provisioning is part of AA-0, record-then-modify from first boot), the six
evidence-integrity countermeasures verbatim, decision ladder, repository layout under
`spikes/amd-epyc/`, and an execution packet.

## Gates (doc task)

- Internally consistent with NESTED-X86/ARM-ALTRA/ARCH-BOUNDARY; "vendor" throughout;
  the single-step section (question 1) must argue from SVM architectural specifics, not
  hand-wave — cite the APM chapter/mechanism for each candidate.
- Open a PR on `task/amd-vendor-spike-doc`; foreman review follows.
- Close `hm-wv8` on merge (foreman-owned).
