# Task 100 — ARM vendor spike program doc: Linux/KVM on Ampere Altra

Bead: `hm-x8g` (P1). Doc-only task: author `docs/ARM-ALTRA.md`, the ARM sibling of
`docs/NESTED-X86.md` — a risk-ordered, GO/NO-GO-gated spike program for the bare-metal ARM
cell of the reach matrix (vendors × forms; see the north star in `docs/QUEUE.md`). Target
hardware: an incoming **Ampere Altra** box (Neoverse N1, Armv8.2). Execution is gated on
hardware arrival; the doc's job is that day one is experiment day.

## Read first (binding context)

`docs/NESTED-X86.md` (the template — structure, execution constraints, evidence discipline,
decision ladder), `docs/ARM-PORT.md` (the cross-ARM mechanism analysis — hardware facts
stand), `docs/ARCH-BOUNDARY.md` (the seam; D-list build stays spike-gated; note
"personality" is being renamed "vendor" — use **vendor** throughout), `docs/BOX-PINNING.md`
(pinning discipline transfers). Review lesson (2026-07-12, PR #98): the nested-x86 spike's
harnesses could report green on failed gates and missed their own acceptance floors — this
doc must bake in the countermeasures below from the start.

## Content requirements (front and center, in this order)

1. **Time virtualization is the centerpiece.** Altra/N1 (Armv8.2) has **no FEAT_ECV** —
   guest `CNTVCT` reads cannot be trapped. The design answer is the **paravirt
   work-derived clock** (companion spec bead `hm-8h8`; cross-reference, don't duplicate):
   we own the guest kernel, so its time reads route through a work-derived clock page and
   raw counter access is closed at the contract level (denied/undef). State explicitly
   which stage validates this and what its kill condition is.
2. **The work clock bet**: `BR_RETIRED` (retired *taken* branches, raw 0x21) on N1 — the
   best rr-characterized aarch64 lineage, but a **different event** than x86 conditional
   branches: every `skid_margin`/density constant re-measured, never inherited. Name the
   N1-lineage missed-PMI-on-core-migration kernel bug (rr issue #3607) and its mitigation
   (hard pinning) as a standing condition.
3. **Kernel patch analogues**: the 0004-analogue (deterministic in-kernel force-exit at
   PMI) is real arm64 KVM patch work; the 0005-analogue may be nearly free via
   `KVM_GUESTDBG_SINGLESTEP` (`MDSCR_EL1.SS`) — both get their own stages with acceptance
   criteria.
4. **LL/SC vs LSE ruling stage**: LSE-only guest contract + enforcement levels (kernel
   config guarantee → opcode scan with W^X/rescan-on-exec → trap/emulate fallback); the
   doc must require the final ruling to state whether LL/SC is mechanically unreachable or
   a cooperative residual risk.
5. **New contract, new payloads**: `ID_AA64*` freeze + trapped-sysreg tables (the x86
   CPU/MSR contract is the rigor template, not the content); payloads are new-by-purpose,
   not ports. GIC v3 + generic-timer model replace LAPIC/PIT in the device row.
6. **Fallbacks + siblings**: Graviton `.metal` instances (c7g/c8g — Neoverse V1/V2, real
   bare-metal EL2, rentable hourly) as the zero-procurement fallback and second microarch
   data point; **nested-on-ARM explicitly deferred** (needs FEAT_NV2 silicon + very fresh
   KVM nested-arm64 — its own future gate, never an assumption).

## Structural requirements (inherit from NESTED-X86.md, tightened)

- Risk-ordered stages, each with question / method / acceptance / stop condition; the
  decision ladder (GO / PROVISIONAL GO / REDESIGN / NO-GO) verbatim.
- **Evidence-integrity countermeasures are mandatory stage criteria** (the PR-98 lesson):
  every harness must propagate gate RCs (a done-marker is never a success condition);
  acceptance floors are machine-checked against retained evidence, not asserted; every
  boot artifact content-hash-verified before execution, not merely recorded; the apparatus
  must be shown to exercise the *claimed* mechanism (patched vs stock) as part of the
  stage's own acceptance.
- Machine-readable evidence manifests; every attempted sample accounted for; unsupported
  is a result.
- Repository layout under `spikes/arm-altra/`; worktree + no-push discipline mirroring the
  x86 spike's execution constraints.

## Gates (doc task)

- The doc exists, is internally consistent with ARM-PORT.md's hardware facts, uses
  "vendor" (never "personality"), and cross-references hm-8h8 for the clock design.
- Open a PR on branch `task/arm-vendor-spike-doc`; foreman review (light tier + a
  technical read) follows.
- Close `hm-x8g` on merge (foreman-owned).
