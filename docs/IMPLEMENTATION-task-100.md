# Task 100 — implementation notes: `docs/ARM-ALTRA.md` (bead `hm-x8g`)

Doc-only task. One new file, `docs/ARM-ALTRA.md` — the ARM sibling of `docs/NESTED-X86.md`:
a risk-ordered, GO/NO-GO-gated spike program for the bare-metal ARM cell of the reach
matrix, targeting the incoming Ampere Altra (Neoverse N1). All six content requirements are
front-and-center in the spec's order (§1–§6 of the doc), the stages are AA-0…AA-6, and the
PR-98 evidence-integrity countermeasures are a binding section that every stage's acceptance
inherits.

## Judgment calls (for reviewer veto)

1. **Stage granularity: seven stages (AA-0…AA-6), with 0004/0005 analogues split into AA-2
   (single-step, stock KVM, expected nearly free) and AA-3 (force-exit patch + exact
   landing).** The spec says "both get their own stages with acceptance criteria" — I
   ordered single-step *first* because it is stock-KVM-cheap and AA-3's landing loop
   depends on a trustworthy step primitive. The LL/SC ruling (AA-4) sits after AA-3 because
   its hazard demonstration needs the injection machinery; the clock (AA-5) after that
   because its Linux smoke rides everything prior. AA-1 (BR_RETIRED exactness) stays the
   immediate-focus stage per ARM-PORT.md's "spike #1" ruling.
2. **The spike measures mechanisms; it never builds the production backend.** Scope line
   drawn explicitly: spike apparatus (minimal ioctl-level KVM harness, arm64 payload
   runtime, oracle payloads, opcode scanners) lives under `spikes/arm-altra/`; the D-list
   (ARCH-BOUNDARY §D) unblocks only on ALL-GO. Consequence: the reach-matrix **cell-fill
   demo (one documented command) is assigned to the downstream port program, not this
   spike** — unlike NESTED-X86's N-5, which could package because the x86 stack already
   existed. AA-6's mini determinism gate (≥1,000 same-seed reps over the payload matrix +
   the Linux smoke guest) is the spike's proof-of-mechanism substitute.
3. **AA-1 measures PMI delivery pre-patch via a host-side signal-kick out of `KVM_RUN`,
   with the 0004-analogue moving it in-kernel at AA-3.** This keeps the existential
   measurement (count exactness + overflow reliability + skid) unblocked by kernel patch
   work, mirroring how rr itself operates. Mechanism attestation (PR-98 lesson #4) then
   makes AA-3 structurally unable to pass on the AA-1 fallback path.
4. **The paravirt-clock closure story is spelled to the register level** (CNTKCTL_EL1 EL0
   undef; CNTHCTL_EL2 physical-counter trap as backstop; EL1 CNTVCT as the untrappable
   residue closed by kernel ownership + opcode scan). The spec asked for "closed at the
   contract level (denied/undef)"; I made explicit *which* layer is hardware-denied and
   which is ownership-closed, because that split is exactly what AA-5(b) must test. The
   page layout itself is deferred to `hm-8h8` (cross-referenced ~10×, duplicated nowhere).
   AA-5 is named as the validating stage and its kill condition is stated twice (in §1 and
   verbatim in the stage) per the spec's "state explicitly" requirement.
5. **An AA-5 (clock) NO-GO is declared fallback-less and escalates as a strategy fact**,
   unlike the work-clock NO-GOs which get the Graviton/software-counter/emulation ladder.
   Rationale in-doc: no reachable ARM server silicon (N1, V1, V2 — per `hm-8h8`) can trap
   CNTVCT, so there is no trap-based tier to fall back to; pretending otherwise would be
   the "convert NO-GO into GO" anti-pattern.
6. **LL/SC hazard description extends ARM-PORT.md, consistently.** ARM-PORT names the
   injected-interrupt-clears-monitor path; I added the architecturally-permitted *spurious*
   STXR failure (rr's actual refusal reason, hedged in ARM-PORT as "a related reason") and
   the single-step-livelock interaction, because AA-4's ruling ("mechanically unreachable
   vs cooperative residual") is unanswerable without them. The "trap/emulate fallback"
   enforcement level is made concrete as stage-2 execute-deny + fault-and-emulate, since
   exclusives have no direct trap.
7. **"Verbatim" decision ladder = the four labels with their NESTED-X86 definitions,
   fallback content adapted to ARM.** The nested doc's NO-GO text names nested-specific
   fallback tiers; copying those literally would be wrong. Labels, semantics, and the
   "record, don't build, fallbacks" rule carry unchanged.
8. **Box discipline is rewritten for a new, incoming box.** Restore-manifest discipline
   becomes *baseline*-manifest discipline (day-one capture IS the restore target; the box
   has no prior role to restore to). The pkill landmine, content-hash boot verification,
   reachability rule, and state-based waits transfer verbatim; pinning is promoted from
   hygiene to a correctness condition (rr #3607), with AA-1's bounded migration probe as
   the one sanctioned unpinned run. `ARM_BOX_SSH` is proposed alongside `DET_BOX_SSH`
   (BOX-PINNING.md itself is not edited — its Altra section is deferred to AA-0's actual
   topology data).
9. **"No Beads during spike execution" is carried over from NESTED-X86.** The structural
   requirement says execution constraints mirror the x86 spike's; the beads rule was part
   of that constraint set and the same durable-state-in-evidence-dirs logic applies. Easy
   to strike in review if the foreman wants spike execution bead-tracked.
10. **Sample floors mirror the nested program:** ≥10⁶ armed overflows (AA-1) / armed
    deadlines (AA-3) cumulative, ≥1,000 same-seed mini-gate reps (AA-6) — now with the
    PR-98 rule that floors are machine-checked against retained records before a
    disposition may be written.

## Gate verification

- **Doc exists**: `docs/ARM-ALTRA.md`.
- **"vendor" never "personality"**: grep confirms "personality" appears only inside the
  rename ruling itself ("'vendor' replaces 'personality'" and the ARCH-BOUNDARY read-as
  note) — which the task spec itself instructs the doc to note. All role uses say vendor.
- **hm-8h8 cross-referenced, not duplicated**: 10 references; no page-layout/update-
  discipline content reproduced.
- **Consistency with ARM-PORT.md hardware facts** (checked line-by-line): ECV mandatory ≥
  v8.6; BR_RETIRED = raw 0x21 = retired *taken* branches (a different event than x86
  conditional branches; constants re-measured never inherited); N1 = best rr-characterized
  aarch64 lineage; rr #3607 missed-PMI-on-migration on the N1/V1 lineage, mitigated by
  pinning; LSE mandatory ≥ v8.1 (N1 has it); SVE absent on N1, present on the Graviton
  V1/V2 fallbacks (rr's non-faulting-load worry attaches there); Graviton3 = V1 (c7g),
  Graviton4 = V2 (c8g), V2 has zero rr-tested data; CpuBackend trait ports unchanged
  (ARCH-BOUNDARY's verification cited, trait freeze gated on AA-3 data).
- **PR opening**: the task's PR gate is left to the foreman — this worker's instructions
  are explicitly no-push, and a PR requires the branch on origin. Branch
  `task/arm-vendor-spike-doc` is ready to push as-is.

## Known limitations / for the integrator

- Hardware specifics that only silicon can settle (exact Altra SKU/core count, KVM
  VHE-vs-nVHE mode, writable-ID-reg surface breadth, whether KVM's current CNTHCTL_EL2
  posture already traps the physical counter or needs patch work) are deliberately written
  as AA-0 expect-vs-found rows rather than asserted facts.
- The claim that arm64 KVM couples the generic timer's PPI wiring to the in-kernel vGICv3
  (motivating AA-6(b)'s userspace-GIC decision input) reflects mainline KVM's known shape
  but should be re-verified against the host kernel actually deployed on arrival.
- `docs/BOX-PINNING.md` gains its Altra standing-assignments section only at AA-0 (needs
  real topology); this doc says so rather than pre-inventing a core map.
- Bead `hm-x8g` closes on merge (foreman-owned per the task spec); `hm-8h8` remains open —
  AA-5 validates its design but its ratification is its own path.
