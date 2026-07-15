# Provenance — `insn-cpuid.digest` (O2 conformance golden)

`insn-cpuid.digest` is the 64-hex `observable_digest` (report-stream + serial
banner, SHA-256) that the box-only O2 conformance gate
(`consonance/vmm-core/tests/box_corpus.rs::c1_corpus_o1_o2_on_the_patched_backend`)
compares against. It is **captured on the patched determinism box only** — never
hand-edited — with `DETCORPUS_BLESS=1` (see that test's module docs and
`docs/corpus-manifest.toml`). The `.digest` file is a bare 64-hex line (the gate
reads it with `.trim()` and asserts `len == 64`), so this adjacent file carries
the provenance.

## Current capture (2026-07-15, tasks/113, `hm-zc2`)

| Field | Value |
|---|---|
| Digest | `cd321ad6f98a9b33f1277a243b06ff5eb5390b652b31f74bb8c39a55f87282f5` |
| Supersedes | `746d8bbbeb4591f8a2ef35eeefcb6dee306b4257999133f74eaf295f848216a9` (stale, contract v3) |
| Box | Intel Core i9-9900K (Coffee Lake-S; family 6 / model 0x9e / stepping 0xc), `det-cfl-v1` |
| Microcode | `0xf8` (unchanged vs the `det-cfl-v1` baseline — `docs/fragments/cfl-baseline/`) |
| Host kernel | `6.12.90+deb13.1-amd64` |
| Patched KVM | `kvm.ko` size **1400832** (stock is 1396736); Part-2 6.12.90 loadable build, `consonance/vmm-backend/kvm-patches/BUILD.md`, built 2026-06-30 |
| Capture tool | `DETCORPUS_BLESS=1 taskset -c 2 cargo test -p vmm-core --test box_corpus c1_corpus_o1_o2_on_the_patched_backend -- --ignored --nocapture` |
| Seed / RAM | `CORPUS_SEED = 0x0028_C0FF_EE5E_EDC0`, 256 MiB guest |
| Repo | main HEAD `9d6778d` (fresh clone) |
| Pinned core / gov | core 2 (SMT sibling cpu10 idle); governor `powersave` — CPUID content is frequency-independent, recorded for completeness |

## Why it drifted (root cause: legitimate contract correction, not a regression)

The guest-visible CPUID is a **frozen table harmony installs via `KVM_SET_CPUID2`
from `docs/cpu-msr-contract.toml`** — it is host-independent (not inherited from
the host's live CPUID). The digest therefore moves only when that frozen model
changes, not with microcode/image/KVM.

The model was legitimately corrected in commit `9d60c75` (task 49/56 MADT+ARAT
SMP bring-up, PR #36), which bumped the contract v3 → v4. The **sole** CPUID
delta is:

```
CPUID.06H (Thermal/Power Mgmt) EAX:  0x00000000  ->  0x00000004   (bit 2 = ARAT set)
```

ARAT (Always-Running APIC Timer) is genuinely present on this CPU — the
`det-cfl-v1` host baseline reports leaf-6 EAX `0x000027f7` ("ARAT always running
APIC timer = true", `docs/fragments/cfl-baseline/cpuid-decoded.txt`). v3 masked
it off (`0x0`); v4 exposes exactly that bit (`0x4`), which the guest's
MADT/APIC-timer path needs. No other swept leaf/bit moved, and no contract-frozen
identity leaf (vendor string, family/model, max-leaf, brand string) changed.

The box golden was captured at the 2026-06-25 public-release squash (contract v3)
and **never re-blessed after the v3 → v4 correction** — i.e. a stale golden
(`hm-zc2`), not microcode drift (host unchanged at `0xf8`) and not the 2026-07-09
box-image rebuild (`hm-xdp`/`hm-2nt`; CPUID is harmony-injected, image-independent).

## Determinism / stability

- **O1 deterministic** run-to-run: `box_corpus` reports insn-cpuid `O1=PASS …
  identical; both halted at 1`, and the two-sweep verify aggregate matches.
- **Reboot-invariant by construction**: the one differing register is a
  compile-time constant in the injected `KVM_SET_CPUID2` table (host-independent),
  so it cannot vary across reboots.
- **Cross-corroborated**: the same value `cd321ad6…` was captured independently in
  three prior box sessions on different days — the nested-x86 spike (2026-07-10,
  `spikes/nested-x86/results/n2/`), the task-108 box differential (2026-07-14), and
  the PR-110 box window (2026-07-15).
