# Contract vendor axis — implementation notes (tasks/117, hm-0nf)

The `contract/*` loader/serializer gains a **vendor axis**: one data-driven pipeline
(one parser, one canonical serializer, one hash function) that carries both the Intel
column (`docs/cpu-msr-contract.toml`, `det-cfl-v1`/GenuineIntel — current truth,
live-enforced) and a new AMD **draft** column (`docs/cpu-msr-contract-amd-draft.toml`,
`det-zenN-v1`/AuthenticAMD — loadable + canonicalizable, wired into **no** live
enforcement path). This is a *column on the one contract*, never a second forked
document (`docs/GLOSSARY.md`). AE-4 (`docs/AMD-EPYC.md` §4) later fills the draft's
enforcement cells with silicon-verified dispositions; this task ships the column and the
machinery, with every AMD enforcement cell marked `verify-on-silicon`.

## Grammar additions (all additive; Intel body untouched)

- **`[contract] vendor`** — `"GenuineIntel"` / `"AuthenticAMD"`. Parsed into
  `Contract.vendor: VendorId` and enforced at load time. **Not** emitted into the hashed
  canonical form (see "Intel canonical-form decision" below).
- **Per-row `verified = "on-silicon-pending-AE4"`** on `cpuid.entry` / `msr.entry`
  (`CpuidRow.verified` / `MsrRow.verified`). `None` for Intel rows (implicitly
  `verified = det-cfl-v1`); `Some(..)` on every AMD enforcement row. **Part of the hashed
  canonical form** (Deliverable 3, Paul veto point 2 — row-level, in the hash), so a row
  silently losing its marker is a hash-breaking byte diff.
- **Per-row `applies-when = "legacy-perfmon" | "zen4+"`** on `msr.entry`
  (`MsrRow.applies_when`) — the per-generation PMU marker (Deliverable 4). Hashed. The
  loader parses both PMU models and resolves neither; AE-0 pins which is live.
- **`[transfers]`** singleton (`Contract.transfers: BTreeMap<String,String>`) — section-
  level `transfers-unchanged-pending-AE4` markers for the shared-ISA surface
  (`cpuid-standard`, `insn`, `timer`, `cmos`, `mmio`) and the per-silicon `host-assert`
  block (`on-silicon-pending-AE4`). The canonicalizer emits `transfer <section>
  <disposition>` in place of the section's rows (Deliverable 2, Paul veto point 5 —
  section-level, not 3000 hand-copied rows).
- **`[[msr-shared.entry]]`** array (`Contract.msr_shared: Vec<IndexSpec>`) — the shared
  architectural MSR surface is an **explicit allowlist**, not a numeric range (round-4
  finding 2). Each entry is index-only (the disposition transfers, pending AE-4);
  canonicalized as `msr-shared <idx> unchanged-pending-AE4` records. Why an allowlist and
  not a `< 0xc000_0000` cutoff: the MSR index space has vendor-specific addresses
  (`IA32_ARCH_CAPABILITIES` `0x10a`, `IA32_TSX_CTRL` `0x122` are Intel-specific), so a bare
  numeric range would over-claim non-portable rows for a future AE-4 consumer. CPUID
  standard leaves stay a bounded `cpuid-standard` marker because the standard-leaf space is
  a *shared enumeration* (leaf N is parallel on both vendors), not vendor-specific numeric
  addresses — the asymmetry is deliberate.

The `[transfers]` / `[[msr-shared.entry]]` forms feed the same tiny total TOML-subset
reader — no `toml` crate, no new dependency (`thiserror` was already present).

## Loader shape (Deliverable 7 — under `vendor/x86/contract/`)

- `VendorId` (`pub(crate)`) is the first-class axis, and lives **inside**
  `vendor/x86/contract/` (Paul veto point 3), consistent with the tasks/108 engine/vendor
  split: both Intel and AMD are x86 vendors. The engine names no vendor specifics.
- `Contract::load(toml, expected: VendorId) -> Result<Contract, ContractError>` is the
  single validating entry point, and it is **fail-closed** — every ambiguity is a refusal,
  never a silent default:
  - `[contract] vendor` **absent** → allowed (legacy Intel fixtures, resolved to
    GenuineIntel; `parse` keeps the raw `vendor_declared` token so absent is distinguishable
    from present);
  - vendor **present but not a known token** → `UnknownVendor` (never defaulted to
    GenuineIntel — this was the round-1 fail-open hole);
  - vendor present, valid, disagreeing with the load axis → `VendorMismatch`;
  - **no** CPUID row covers leaf 0 subleaf 0 → mixed-vendor guard skipped (fixtures);
  - a covering row exists but is not the one canonical shape → `MalformedLeaf0`;
  - the canonical `(0,0)` row is well-formed but spells another vendor → `MixedVendor`.

  **Leaf-0 guard = positive validation of the one good shape** (round-4 REDESIGN). The
  guard was bypassed three ways across rounds (exact-match → range-form → dyn-EAX) while it
  enumerated *malformed* shapes; the fix inverts it. `covers_leaf0_subleaf0` collects every
  row whose (leaf, subleaf) range touches CPUID(0,0) — the inclusive range form
  (`leaf-lo = 0, leaf-hi > 0`) and the `*` / `N+` / `a-b` tokens included. If that set is
  non-empty, `canonical_leaf0_vendor_string` accepts it **only if** it is *exactly one*
  single `leaf = 0, subleaf = 0` row whose **all four** registers (EAX/EBX/ECX/EDX) are
  frozen constants with a UTF-8 EBX‖EDX‖ECX string; the string must then equal the declared
  vendor. Everything else — a range/`*`/`N+` form, a dynamic register *anywhere* (incl. EAX,
  the third bypass), non-UTF-8 bytes, or more than one covering row — is `MalformedLeaf0`;
  a well-formed row spelling the wrong vendor is `MixedVendor`. Validating the single good
  shape (rather than chasing malformed ones) closes the whole bypass class. `Contract::parse`
  stays infallible for the direct-token unit tests. Refusal tests cover `UnknownVendor`,
  `MalformedLeaf0` (dyn-EBX, dyn-EAX, non-UTF-8, range form ×2, two covering rows),
  `VendorMismatch`, `MixedVendor`, and the good-shape / non-zero-subleaf pass-through cases.
- Public API is unchanged: `contract()` (Intel, the live policy path) now routes through
  `load(.., GenuineIntel)`; the AMD constructor `contract_amd_draft()` is **`#[cfg(test)]`
  only**, and the AMD TOML is `include_str!`-embedded only under `cfg(test)`. `VendorId` /
  `ContractError` / the new fields are all `pub(crate)`. So the committed Linux
  `tests/public-api.txt` snapshot is **byte-unchanged** (no new public items).

## Draft-only guard (Deliverable 8 — structural, not a comment)

The only symbol that returns the AMD contract, `contract_amd_draft()`, does not exist
outside `cfg(test)`; the AMD TOML is embedded only under `cfg(test)`. A live VM
construction path (`bringup::boot`, `dispatch`, `vmm`) therefore **cannot name** the AMD
contract — it is structurally unreachable, not merely undialed. `contract()` /
`cpuid_model()` / `msr_filter_allow()` / `disp_map()` all operate on the Intel column only.
`amd_draft_is_unreachable_from_the_live_path` asserts the live path is GenuineIntel.

## Intel canonical-form decision (Deliverable 6, Paul veto point 4) — **zero-drift**

The spec offered two paths: a reviewed `contract-version` bump (vendor key hashed) vs a
zero-drift grammar (vendor key un-hashed header metadata, Intel canonical form truly
byte-identical). **Zero-drift was feasible and is strictly better, so it is what shipped.**
The `vendor` key is header metadata the serializer never emits; the AMD/Intel columns are
distinguished in the hash by their genuinely different content (the AuthenticAMD leaf-0
string, the `det-zenN-v1` baseline, the AMD MSR rows, the verify/applies-when/transfer
tokens), not by a `vendor=` line. Consequences:

- **Intel is byte-identical through the restructure.** `contract-version` stays `4`; the
  golden `testdata/canonical-v4.txt` is **unchanged**; the committed Intel
  `contract_hash = 30839ae6…` is **unchanged**. The existing Intel disposition / CPUID /
  filter / golden / registry-drift tests stay green **untouched** — they are the proof of
  byte-identity. The only Intel-file diff is the single additive `vendor` header line
  (plus its comment).
- **AMD hash committed:** `docs/cpu-msr-contract-amd-draft.toml` `[contract]
  contract_hash = 0e4e8daaa7bafe197396ebac8b42b0671453a7e1ab12f73136ee5e44533b5849`, with
  golden `testdata/canonical-amd-draft.txt` (regen: `contract::tests::regen_amd_golden`,
  ignored). The computed-==committed gate is green. The zen4+ PerfMonV2 section carries the
  full global control/status set `0xc000_0300`–`0xc000_0303` (GLOBAL_STATUS / CTL /
  STATUS_CLR / STATUS_SET), matching the Intel mirror's `AMD64_PERF_CNTR_GLOBAL_*` rows.

## AMD draft content — honesty about what is and is not pinned

The draft's **shape** is drafted from the AMD64 APM + `docs/AMD-EPYC.md` §4; its
per-silicon **values** are deliberately left unpinned (`0`) where they are
generation/silicon facts (CPUID family/model/stepping, feature masks, brand string, cache
geometry, MAXPHYADDR; and the silicon-frequency header scalars `tsc-hz`/`crystal-hz`/
`bus-hz`/`rtc-epoch`/`pit-refresh-ns`, omitted so they render as `<key>=0`). Only hard
architectural facts are frozen: the AuthenticAMD vendor string, the extended-leaf
enumeration bounds, the MSR index set + dispositions, and the ISA-level `mxcsr-mask`. Every
materialized row is `verify-on-silicon`. This keeps the draft honest — a placeholder is a
placeholder, never a guess — and matches the "`det-zenN-v1` generation guess" non-goal.

Two internal-consistency invariants make the draft's own claims agree with its data
(round-3 review), both machine-checked:

- **CPUID enumeration bound = transfer range.** Leaf-0 EAX (`max-basic-leaf`) is `0x10`, so
  the `cpuid-standard` transfer covers standard leaves `0x1..=0x10`; leaves above `0x10` are
  out of range and redirect to zeroed (`cpuid-default`), never "transferred". The prose and
  the frozen `0x10` bound name one truth (test `amd_leaf0_max_basic_leaf_is_the_transfer_bound`).
- **MSR ownership is explicit — a shared allowlist, not a numeric range** (round-4
  finding 2 sharpened the round-3 partition). The file materializes the **entire AMD-native
  MSR space, `≥ 0xc000_0000`** (including the syscall/segment MSRs `0xc000_0080`–`0xc000_0103`
  — AMD-native though architecturally shared, so owned by the materialized rows). The shared
  architectural MSRs that transfer are the **explicit `[[msr-shared.entry]]` allowlist**
  (`IA32_TSC`, `IA32_APIC_BASE`, `IA32_SYSENTER_{CS,ESP,EIP}`, `IA32_PAT`) — an enumerated
  list, never a `< 0xc000_0000` cutoff, so Intel-specific MSRs (`0x10a`, `0x122`) that live
  below the old cutoff are not claimed as shared. The allowlist is disjoint from the
  materialized rows and contains no Intel-specific MSR (test
  `amd_msr_shared_allowlist_is_disjoint_and_portable`) — no ambiguous ownership, no
  non-portable inheritance for a future AE-4 consumer.

## Known AE-4 ratification dependency

The AMD column is a draft: no disposition below it is trusted. AE-4 delivers the on-silicon
enforcement-mechanism truth table (each row → the SVM VMCB trap/freeze that enforces it, or
recorded as undeniable) and pins the real values, at which point the `verify-on-silicon`
markers are cleared and the baseline placeholder `det-zenN-v1` is replaced with the pinned
generation name — both a `version` bump + `contract_hash` re-derivation, never a silent
edit. Until then the draft is data + machinery only, reachable only from tests.

## Deviations considered and rejected

- **Reviewed version bump (vendor key hashed).** Rejected in favour of zero-drift (above) —
  the spec prefers zero-drift when feasible, and it is.
- **A vendor-neutral `vendor/contract/` module** parameterized by vendor (Paul veto
  point 3). Rejected: both Intel and AMD are x86 vendors, so `vendor/x86/contract/` is the
  correct home; the ARM contract is a *different schema* handled later (below).
- **Materializing the shared-ISA surface (standard CPUID leaves, the shared MSR space, the
  §4/§5 tables) into the AMD file.** Rejected: that forks the one reproducer. Section-level
  transfer markers carry the shared surface instead; AE-4 decides real divergence.
- **A public `contract_for(VendorId)` returning the AMD column.** Rejected: it would let a
  live path name the draft, defeating the structural draft-only guard. The AMD constructor
  is `#[cfg(test)]`.

## Future work — NOT built here

- **ARM contract analogue** (`docs/ARCH-BOUNDARY.md` §B, `hm-cbt`): a *different schema*
  (ID-register freeze + trapped-sysreg table), not a vendor column on the x86 CPUID/MSR
  grammar. The grammar was **not** generalized toward it speculatively (non-goal). When ARM
  lands it gets its own contract shape, not a bent version of this one.
- **The SVM enforcement backend** (VMCB MSR-permission bitmap, CPUID intercept) and any
  `KVM_X86_SET_MSR_FILTER` AMD path — AE-4's on-silicon work, not this task.

## Gates (all green, laptop-side)

`cargo build/nextest/clippy/fmt -p vmm-core --all-features`, `cargo deny check`,
cross-target `cargo clippy --target x86_64-unknown-linux-gnu -p vmm-core`, and
`cargo +nightly miri test -p vmm-core` (the AMD golden/hash/serialization tests carry the
same `#[cfg_attr(miri, ignore)]` discipline as the Intel ones; the parse/load/disjointness/
verify-coverage/mixed-vendor logic runs under Miri). Intel byte-identity is proven by the
unchanged Intel golden + hash gates staying green.
