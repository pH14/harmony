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
| Repo | main HEAD `9d6778d` (fresh clone `/root/harmony-t113`) |
| Pinned core / gov | core 2 (SMT sibling cpu10 idle); governor `powersave` — CPUID content is frequency-independent, recorded for completeness |

### Capture inputs (content hashes)

The digest is a function of the executed payload bytes + backend, so the capture
is pinned by these (all under the `/root/harmony-t113` clone at commit `9d6778d`):

| Input | SHA-256 |
|---|---|
| Payload ELF `insn-cpuid` (the one that emits the CPUID sweep) | `e57784e483d3add5c67ee2b06803b0ba96ebc54691179de71df842aecdf471a9` |
| Payload source `consonance/acceptance-suite/payloads/insn-cpuid/src/main.rs` | `485222d05668e526ac702d657ffbddd6400307ac2ddd8e0c00f3ba366957de93` |
| Frozen model `docs/cpu-msr-contract.toml` (contract v4) | `c116b4487137c3e3481c45a5944349fa00223a0d815889ffaf07341b0ebac25a` |

The other five conformance payload ELFs in the same sweep (unchanged goldens):
`insn-rdtsc 50ca9d66…`, `insn-rng be594df1…`, `insn-rdpmc 9f73388f…`,
`msr-allowed 9ab4e2de…`, `msr-denied 7330e178…`. (There is no separate guest OS
image — the C1 micro-payloads are the executed image, loaded into 256 MiB of
guest RAM.)

## Why it drifted (root cause: legitimate contract correction, not a regression)

`observable_digest` is **not** a pure function of the TOML. It is
`SHA256("OBSV" ‖ report-stream dwords ‖ serial banner)`
(`consonance/vmm-core/src/corpus.rs::observable_digest_of`), where the report
stream folds in, for every swept leaf, the **live** guest `eax/ebx/ecx/edx`. Its
inputs are therefore:

1. the **frozen base model** harmony installs via `KVM_SET_CPUID2` from
   `docs/cpu-msr-contract.toml` (host-independent) — **this is where the drift
   was**;
2. the **three KVM-runtime dynamic cells** the base table does *not* carry —
   OSXSAVE mirror (`1:ECX[27]`), the `0xB/0x1F` level echo, and the `0xD.0`
   XSAVE-area size — which KVM recomputes from the guest's live `CR4`/`XCR0`
   (`kvm_update_cpuid_runtime`), i.e. a function of the guest state **and the host
   KVM**, not the TOML;
3. the fixed **serial banner** (`PAYLOAD insn-cpuid START` / `OK cpuid-stable` /
   `PAYLOAD insn-cpuid PASS`).

Inputs (2) and (3) were **stable across every capture** (same guest `CR4`/`XCR0`,
same patched-KVM recompute, same banner) — the cross-reboot match below is exactly
what confirms the host-KVM-dependent part (2) did not move. The digest changed
only because of input (1): a single frozen-base cell.

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
  identical; both halted at 1`, and the two-sweep verify aggregate matches
  (`e5c7432a…` both sweeps).
- **Cross-reboot**: the box last rebooted **2026-07-10 22:02:15 CEST =
  20:02:15 UTC** (`uptime -s`, box-local). The nested-x86 spike captured
  `cd321ad6…` on the **pre-reboot**
  boot — its metal-corpus run `env.json` records
  `"started": "2026-07-10T04:11:46Z"` (BARE METAL patched modules),
  ~16 h before the reboot — and this task re-captured the identical `cd321ad6…`
  on the **post-reboot** boot (2026-07-15). Those two legs straddle the reboot, so
  the digest (including the host-KVM-dependent dynamic cells) is stable across a
  fresh KVM/host init, not just run-to-run. The drifted cell itself (leaf-6 EAX)
  is additionally a compile-time constant in the injected table, so it cannot vary
  across boots.
- **Cross-corroborated**: `cd321ad6…` also matches the task-108 box differential
  (2026-07-14) and the PR-110 box window (2026-07-15) — four independent captures
  agree.
