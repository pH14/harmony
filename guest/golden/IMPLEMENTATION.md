# tasks/113 — insn-cpuid golden refresh (`hm-zc2`)

Refresh of the stale `guest/golden/insn-cpuid.digest` O2 conformance golden.
Full capture metadata + the leaf-level diff are in
[`insn-cpuid.provenance.md`](insn-cpuid.provenance.md); this file is the
task-worker record (what changed, what was ruled out, integrator notes).

## What changed

- `guest/golden/insn-cpuid.digest`: `746d8bbb…16a9` → `cd321ad6…82f5`
  (one line; the only content change).
- `guest/golden/insn-cpuid.provenance.md`: new adjacent provenance doc.

No harness/crate/manifest/payload code changed (ground rule: golden + provenance
only).

## Diagnosis (diagnose-before-regenerate)

Root cause = **stale golden after a legitimate contract correction**, not a
regression, not microcode, not the box-image rebuild:

- Guest CPUID is a **frozen table harmony installs via `KVM_SET_CPUID2`** from
  `docs/cpu-msr-contract.toml` (host-independent), so the digest moves only when
  that model changes.
- The model was corrected in commit `9d60c75` (task 49/56 MADT+ARAT, PR #36),
  contract v3 → v4. The **sole** CPUID delta: `CPUID.06H:EAX 0x00000000 →
  0x00000004` (bit 2 = ARAT). The `det-cfl-v1` host genuinely reports ARAT
  (leaf-6 EAX `0x27f7`), so v4 is a hardware-faithful correction.
- The box golden was captured at the 2026-06-25 release squash (v3) and never
  re-blessed after v3 → v4 → stale (`hm-zc2`).
- **Ruled out:** microcode (host unchanged at `0xf8` vs the `det-cfl-v1`
  baseline); the 2026-07-09 postgres-image rebuild (`hm-xdp`/`hm-2nt` — CPUID is
  harmony-injected, image-independent; this confirms the hm-xdp note "insn-cpuid
  = stale-golden, not image drift").
- **No escalation trigger:** no contract-frozen identity leaf (vendor string,
  family/model, max-leaf, brand string) moved; the one changed bit is exactly
  the intended v4 correction.

## Box evidence (Paul ruled "load patched, run the real gate")

Determinism box (i9-9900K, kernel 6.12.90, microcode 0xf8), patched KVM
(`kvm.ko` 1400832) loaded via `scripts/box-window.sh` on core 2, reverted to
stock after. Fresh clone at main HEAD `9d6778d`.

- **Reproduce** (gate, no bless): `insn-cpuid O1=PASS O2=FAIL digest=cd321ad6…
  identical` — deterministic run-to-run, ≠ committed `746d8bbb…`; rdtsc/rng pass.
- **Bless** (`DETCORPUS_BLESS=1`): `blessed insn-cpuid -> cd321ad6…`; `git diff`
  showed **only** `insn-cpuid.digest` changed (the other 5 re-blessed identical).
- **Verify** (gate, no bless): all 6 conformance items `O1=PASS O2=PASS`,
  deterministic twice (aggregate `e5c7432a…` both sweeps), `test result: ok`.
- Box reverted to stock (`REVERT OK`, kvm 1396736).

The blessed value `cd321ad6…` independently matches three prior box captures
(nested-x86 spike 2026-07-10, task-108 differential 2026-07-14, PR-110 window
2026-07-15).

## Deviations considered and rejected

- **Stock-KVM-only capture** (honoring the spec's "stock KVM is fine" note): the
  committed O2 gate (`box_corpus`) hard-requires the patched module; a stock
  proxy would not be "the box det-corpus O2 gate green." Raised the tension; Paul
  ruled to load patched and run the real gate. Done.
- **Physical reboot to prove stability**: unnecessary and disruptive — the one
  differing register is a compile-time constant in the injected CPUID table
  (reboot-invariant by construction), and it is corroborated by
  `box_corpus`'s deterministic-twice plus three independent prior captures.
- **Running the full `box_corpus` test file**: its two localizer diagnostics
  (`c1_corpus_o1_repeat_diagnostic`, 40 VM-boot iterations) are slow and add no
  signal here; targeted the single gate test instead.

## Portable gates

The `.digest` content is consumed only by the box-only `box_corpus` gate (empty
test on macOS); det-corpus manifest tests use placeholder golden paths and only
require conformance items to *have* a golden. So the portable checks are:
det-corpus builds/tests green, manifest still validates, and the golden is a
well-formed 64-hex line. `run-tests.sh` (serial-shape `.txt` goldens) is
unaffected — it is name-keyed and `insn-cpuid.txt` is unchanged.

## Integrator notes

- Refresh recipe (for the next time the frozen CPUID/MSR model changes): on the
  patched box, `DETCORPUS_BLESS=1 … box_corpus`, review `git diff
  guest/golden/*.digest`, update `insn-cpuid.provenance.md`, commit. Never
  hand-edit a `.digest`.
- A scratch clone at `/root/harmony-t113` on the box holds the run logs
  (`t113b-*.log`, `t113-reproduce-run1-EVIDENCE.log`) for independent review;
  it can be reclaimed.
- Not pushed (worker discipline). Foreman opens the PR; `bd` note left on
  `hm-zc2`.
