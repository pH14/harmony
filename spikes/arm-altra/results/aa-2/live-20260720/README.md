<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AA-2 single-step exactness — live validation, N1, 2026-07-20

Stock KVM `KVM_GUESTDBG_SINGLESTEP` on `6.18.35-aa4guard`, core 60. The AA-2 apparatus
(`AA2-BUILD.md`) ran on silicon; `run-set.json` manifests + floor-check verdicts here,
`records.jsonl` content-addressed on the box (`records_sha256`).

`aa2-full-001` — 16 samples (8 payloads × reps 2), 170,330 step records:

- **debug-evidence PASS** — records cover the **full** AA-2 step matrix (sequential,
  taken/not-taken branch, exception entry, ERET, WFI, injection, LL/SC exclusive); every
  record is a valid single step (`insn_retired==1`) with a `BR_RETIRED` delta consistent
  with its transition class. Single-step retires **exactly one instruction per step** on N1.
- **replay-identity PASS** — 85,165 stepped groups each replayed on **bit-identical** state
  digests: stepping is deterministic.
- step-totality / well-formed / totality / multiplicity / mechanism-attestation / perf-config
  / image-pins / pinning / params-mode / condition-consistency / payload-status all PASS.

Only `weights-present` + `count-exactness` FAIL — no AA-1 measured-weights pack was passed
this session; AA-2 **step records are count-exempt**, so this does not bind the single-step
verdict. A normative AA-2 disposition adds the AA-1 weights pack (steps still exempt) and,
if desired, larger scales.

**Verdict: AA-2 single-step exactness DEMONSTRATED on silicon** — exact one-instruction
stepping across every transition class, replay-deterministic.
