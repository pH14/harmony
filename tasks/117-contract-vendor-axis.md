# Task 117 — Contract vendor axis: AMD draft column on the one frozen contract

**Bead:** `hm-0nf` (P2). **Dispatch authority:** the pre-build ruling (Paul, 2026-07-13;
`docs/ARCH-BOUNDARY.md` §Pre-build ruling — build-first; spikes gate trust, not
construction), queue lane 5 sibling — the contract vendor column, AE-4's shape, gated on
the `hm-b5n` seam keystone (tasks/108) having landed (the loader already lives in
`vendor/x86/contract/` because of that keystone). **Binding context:** `docs/AMD-EPYC.md`
§4 "Contract deltas: a vendor column, never a fork" — the frozen Intel contract
(`docs/CPU-MSR-CONTRACT.md`, `det-cfl-v1` baseline, Coffee Lake i9-9900K) is the rigor
template; most content transfers because the ISA is shared; the deltas are concentrated and
enumerable (AuthenticAMD CPUID + the `0x8000_00xx` extended leaf space, the AMD MSR set
`0xC000_00xx`/`0xC001_00xx`, the PerfMonV2-vs-legacy PMU model as a per-generation fact).
The rule, cross-referenced to `docs/GLOSSARY.md`: this is a **vendor column on the one
frozen contract, not a second forked document** — the enforcement machinery stays a single
data-driven pipeline with a vendor axis, and the Reproducer artifact is never forked.

Read first, in full: `docs/AMD-EPYC.md` (§4 is the spec you are executing; §2 for the
`ex_ret_brn_tkn` / `LS_CFG` / SpecLockMap facts the AMD MSR rows encode; §4's PMU-model
delta), `docs/CPU-MSR-CONTRACT.md` (the whole document — §2 CPUID tables, §3 MSR tables +
the disposition vocabulary, §4 instruction, §5 timer/CMOS/xAPIC-MMIO, §6 canonicalization/
expansion rules), `docs/cpu-msr-contract.toml` (the machine-readable mirror and its strict
grammar header), `consonance/vmm-core/src/vendor/x86/contract/{mod.rs,parse.rs,canonical.rs}`
(the loader + serializer you are extending), `docs/ARCH-BOUNDARY.md` §B (the
table→model→enforce shape; engine names no vendor specifics) and §Pre-build ruling, and
`tasks/00-CONVENTIONS.md`.

## Why now

The pre-build ruling reversed the cost hedge: everything pre-buildable gets built now so
box-wait converts into worker throughput and arrival day stays experiment day. The contract
vendor axis is pre-buildable by construction — it is pure data + a portable Rust loader,
no `/dev/kvm`, no silicon. AE-4 (the future on-silicon AMD spike stage) still owns the
**enforcement-mechanism truth table** that *ratifies* the draft column on real Epyc; this
task ships the column and the loader machinery that AE-4 will later fill with verified
dispositions, with every enforcement cell explicitly marked `verify-on-silicon` so the
draft is honest about what is and is not yet trusted. The `det-zenN-v1` baseline name is a
placeholder pinned at AE-0 (the generation-discovery stage); this task records the
placeholder, it does not invent the generation.

## Scope

Restructure the contract artifacts + the vmm-core `contract/*` loader for a **vendor axis**,
keeping the Intel column as current truth (byte-identical through the restructure, proven by
the existing contract gates) and adding an AMD **draft** column loadable and canonicalizable
but wired into no live enforcement path. Touch only:

- `docs/CPU-MSR-CONTRACT.md` — add the vendor-column framing and the AMD draft sections
  (additive; the Intel prose/tables are not rewritten, only re-framed as the Intel column).
- `docs/cpu-msr-contract.toml` — the Intel file, **zero body diffs** (additive header keys
  only if absolutely needed; prefer none).
- `docs/cpu-msr-contract-amd-draft.toml` — **new**, the AMD draft column, same grammar.
- `consonance/vmm-core/src/vendor/x86/contract/` — the loader/canonicalizer, extended with
  a vendor axis; plus its `testdata/` (new AMD golden) and tests.

No other crate, no enforcement backend, no live VM construction path. The AMD draft file is
**data only** — it is never `include_str!`-embedded into a live policy path.

## Deliverables

1. **Vendor axis in the contract grammar.** The `[contract]` header gains a `vendor`
   field — `"GenuineIntel"` for the existing file, `"AuthenticAMD"` for the draft. The
   baseline name **reuses the existing `cpuid-baseline` key** (`det-cfl-v1` today; the
   `det-zenN-v1` placeholder in the AMD file) — do NOT invent a second `baseline` key.
   The loader takes the vendor as a first-class axis: a `VendorId` enum
   (`GenuineIntel`/`AuthenticAMD`) carried on the parsed `Contract`, and a parser/serializer
   that **refuses a file whose `vendor` field disagrees with the axis it was loaded under**
   (and refuses any mixed-vendor artifact). Existing Intel-file behavior is unchanged: the
   `vendor` key is added to the Intel TOML header as a single additive line; **no other
   byte of the Intel file changes** — see Deliverable 6 for whether that line enters the
   hashed canonical form (the byte-identity ruling).

2. **`docs/cpu-msr-contract-amd-draft.toml` — the AMD draft column, full grammar.** Drafted
   from the APM and `docs/AMD-EPYC.md` §4:
   - **CPUID** (§2 analogue): AuthenticAMD vendor string at leaf 0; the extended leaf
     space `0x8000_0000`–`0x8000_00xx` enumerated (max-extended-leaf, processor brand/extended
     features, address-sizes/topology leaves as the APM defines), with every leaf not
     explicitly listed default-denied exactly as `det-cfl-v1` does. Standard leaves carried
     as the shared-ISA transfer where they match; below-host/feature-masked rows use the same
     masking vocabulary as the Intel file.
   - **MSR** (§3 analogue): the AMD MSR set — `0xC000_00xx` (EFER already shared; the
     `LS_CFG` `0xC001_1020` of §2, `HWCR`, `VM_HSAVE_PA` `0xC0010117` which the Intel column
     default-denies) flipped to enumerated/allowed under an AuthenticAMD baseline using the
     **same** read/write disposition vocabulary (`allow-fixed`, `allow-stateful`,
     `emulate-vtime`, `emulate-timerqueue`, `emulate-apic`, `deny-gp`, `deny-ignore-write`).
     The `0xC001_020x` core-perf-counter pairs (`PERF_CTL`/`PERF_CTR`) and the PerfMonV2
     global control/status MSRs (Zen 4+) each get their own **clearly-marked sections** with
     a generation-conditional marker (Deliverable 4); resolution of which applies is
     deferred to AE-0. MSR index sets remain **pairwise disjoint** within the file
     (validated by a gate).
   - **Instruction / timer / CMOS / xAPIC-MMIO** (§4/§5 analogue): carried as
     **explicitly-marked `transfers-unchanged-pending-AE4`** sections, **not** hand-copied.
     The grammar gains a single section-level marker (e.g. a `[insn-amd]` block with a
     `transfer = "unchanged-pending-AE4"` header, or per-section note — pick one and be
     consistent) that the canonicalizer records but does not expand into 3000 lines of
     near-duplicate rows. The point is to avoid a hand-maintained fork of the shared ISA
     surface; AE-4 decides whether any of these rows actually diverge on AMD.

3. **`verify-on-silicon` as a first-class disposition qualifier.** Every AMD **enforcement**
   cell (every MSR/CPUID row whose disposition claims a trap/freeze/allow the VMM will
   actually enforce, and every instruction/timer row that is *not* a pure
   `transfers-unchanged-pending-AE4` carry) carries an explicit qualifier marking it
   unverified until AE-4 — e.g. `verified = "on-silicon-pending-AE4"` on the row, vs Intel
   rows which are implicitly `verified = "det-cfl-v1"` (the frozen, gated baseline). The
   qualifier is part of the **hashed canonical form** for the AMD file (so a row silently
   losing its `verify-on-silicon` marker is a hash-breaking change, caught by the gate). The
   loader exposes the qualifier on the parsed disposition so a future enforcement path can
   refuse to act on an unverified AMD row.

4. **PerfMonV2-vs-legacy as a per-generation fact.** The AMD draft carries **both** the
   legacy `PERF_CTL`/`PERF_CTR` core pairs and the PerfMonV2 global-control MSRs as
   **separate, clearly-marked sections** with a `generation-conditional` marker (e.g.
   `applies-when = "zen4+"` / `applies-when = "legacy-perfmon"`). The loader parses both but
   resolves **neither** — which set is live for a given part is an AE-0 decision recorded
   against real silicon, not an AMD constant. The draft's `contract_hash` therefore covers
   both sections as draft data; the gate does not assert a single live PMU model.

5. **`det-zenN-v1` placeholder, pinned at AE-0.** The AMD file's `[contract]`
   `cpuid-baseline` field is the literal placeholder string `det-zenN-v1` (the `N` is a
   placeholder, **not** a guessed generation). A header comment in the AMD TOML records that AE-0 replaces this with
   the real `det-zenN-v1` name once the Zen generation is pinned, and that this replacement
   is a `contract-version` bump + `contract_hash` re-derivation, never a silent edit. The
   loader accepts the placeholder as a string (it is not parsed into a generation enum); no
   code branches on the `N`.

6. **Intel byte-identity, proven.** The Intel canonical serialized form and
   `contract_hash` are **byte-identical** through this restructure. If the vendor axis
   adds `vendor` to the hashed canonical header, the Intel canonical form gains
   that line; the existing committed `contract_hash`
   (`30839ae67142f265066be1051e93fcb4a1839c30bd3edd6d875ecdc1a37ddb67`) and the golden
   `testdata/canonical-v4.txt` are **regenerated deliberately** as part of this task's
   single reviewed change (a `contract-version` bump to 5, with the golden + committed hash
   updated together in one commit, the existing `regen_golden` path used), and the gate that
   pins computed-==committed is green on the new value. The PR states explicitly: "Intel
   dispositions are unchanged; the hash changed only because the canonical header now
   carries the vendor axis." If the chosen grammar can instead keep the Intel
   canonical form *truly* byte-identical (the vendor key as non-hashed header    metadata, hashed body unchanged), prefer that — zero Intel-file canonical drift is
   strictly better than a reviewed-bump drift. **This is a Paul veto point (below).**

7. **Loader restructure (vendor axis under `vendor/x86/contract`).** The `contract/*`
   module stays under `consonance/vmm-core/src/vendor/x86/contract/` — both Intel and AMD
   are x86 vendors, so the x86 module is the correct home; a vendor enum
   (`VendorId`) lives inside it. This is consistent with the tasks/108 engine/vendor split:
   the engine names no vendor specifics, and `vendor/x86/` is where both x86 vendors live.
   The module exposes:
   - `contract()` (Intel, unchanged behavior — the live policy path), and a new
     `contract_for(VendorId)` / or two named constructors (`contract_intel()` /
     `contract_amd_draft()`) — pick one shape; the live `contract()` path must remain the
     Intel file and the AMD path must be unreachable from any live VM construction.
   - The canonicalizer and parser become vendor-parameterized where they touch the
     `vendor`/`baseline`/`verify-on-silicon` qualifier; the shared table→model→enforce
     shape is unchanged.
   - The AMD file is embedded with its own `include_str!` **only behind a
     `cfg(test)`-gated test constructor**, never on the production policy path. (Or, if
     simpler, the AMD file is read by tests via `include_str!` in the test module only.)

8. **Tests (all portable, run on macOS):**
   - **Intel round-trip + hash pin**: `contract_hash()` for Intel equals the new committed
     `contract_hash` (byte-identity through the restructure); the golden
     `canonical-v5.txt` matches; the existing Intel disposition/CPUID/filter tests stay
     green unchanged.
   - **AMD round-trip + hash pin**: the AMD draft loads, canonicalizes, and produces a
     stable `contract_hash` (two calls agree; non-trivial); a committed AMD golden
     `canonical-amd-draft.txt` matches byte-for-byte; the AMD `contract_hash` is committed
     in the AMD TOML's `[contract]` and the computed-==committed gate is green.
   - **Grammar validation per file**: MSR index sets are pairwise disjoint **within each
     vendor file** (the existing disjointness check, generalized to the loaded vendor).
   - **Draft-only guard**: a test that the AMD draft **cannot be selected by any live VM
     construction path** — assert that `contract()` (the live path) is the Intel contract
     and that no public function the bringup/boot path calls returns the AMD contract.
     Structural, not merely behavioral: e.g. the AMD constructor is `#[cfg(test)]`-only or
     returns a type the live `boot` path cannot name.
   - **`verify-on-silicon` marker present on every AMD enforcement row**: a test that
     walks the AMD draft and fails if any non-`transfers-unchanged-pending-AE4` enforcement
     row lacks the qualifier (catches a silently-trusted AMD row).
   - **Mixed-vendor refusal**: a test that the loader rejects a file whose `vendor` field
     disagrees with the axis it was loaded under, and rejects an artifact mixing
     dispositions across vendors.
   - **Format-invariance** for the AMD file (the existing proptest, generalized to both
     files): incidental formatting noise does not change the canonical form / hash.

9. **`docs/CPU-MSR-CONTRACT.md` framing.** Update the document's header and §1 to state the
   vendor-column framing: the Intel tables are the `det-cfl-v1` column (current truth); the
   AMD draft column lives in `docs/cpu-msr-contract-amd-draft.toml` and is
   `verify-on-silicon` pending AE-4; the one-reproducer / never-fork rule is restated
   (`docs/GLOSSARY.md`). Add a short AMD §4-analogue subsection pointing at the TOML and
   recording the `transfers-unchanged-pending-AE4` carry for instruction/timer/CMOS/MMIO and
   the PerfMonV2-vs-legacy per-generation deferral. **No Intel table row changes.**

10. **`IMPLEMENTATION.md`** in `consonance/vmm-core/src/vendor/x86/contract/` noting: the
    grammar additions, the Intel canonical-form decision (bump-vs-zero-drift) and the new
    committed hashes, the draft-only guard mechanism, the known AE-4 ratification
    dependency, and the ARM contract analogue as explicitly future work (not built).

## Constraints (binding)

- **Rust, no `unsafe`, portable macOS + Linux.** This task is Mac-portable — no box, no
  SSH, no `/dev/kvm`. All gates run laptop-side.
- **Determinism discipline.** No `HashMap`/`HashSet` iteration reaching an output or the
  canonical bytes or the hash — `BTreeMap`/sorted only (the existing loader already does
  this; keep it). No floating point. No wall-clock. The canonical form is a pure function
  of the parsed tables, sorted, fixed layout — preserve that invariant for both files.
- **Never fork the one reproducer.** One data-driven pipeline with a vendor axis. No second
  loader, no second serializer, no second contract document that duplicates the shared ISA
  by hand. The AMD file is a *column* (a vendor axis value) on the same grammar, not a
  parallel artifact.
- **Intel byte-identity is the bar.** The Intel dispositions are byte-identical through the
  restructure; the only permitted Intel canonical-form change is the reviewed
  vendor-key header addition (Deliverable 6), and if a zero-drift grammar is feasible
  it is preferred. A silent Intel disposition change is a blocking defect.
- **The AMD draft is not trusted.** It is loadable + canonicalizable (its own
  `contract_hash`) but wired into **no live enforcement path**. The draft-only guard
  (Deliverable 8) is structural, not a comment.
- **No new dependencies.** The whitelist covers this (`thiserror`, `zerocopy`, `serde`,
  `sha2`, `proptest`, …); the parser is the existing tiny total TOML-subset reader — extend
  it, do not pull in a `toml` crate.
- **Grammar is additive for Intel.** New header keys (`vendor`, `baseline`) are the only
  permitted Intel-file additions; new optional row qualifiers (`verified`) default to the
  Intel-baseline-implicit value so Intel rows need no body edit. Prefer zero Intel-file
  body diffs.
- **GLOSSARY vocabulary**: "vendor" never "personality"; `Subject`, `Moment`/`Span`,
  `V-time`, `Reproducer` in prose; no "(formerly X)" comment residue.
- **Miri floor must not shrink.** vmm-core is on the `miri` job's `-p` list; the contract
  tests are already `#[cfg_attr(miri, ignore)]` where they are pure serialization over a
  48 KiB form (no UB value) — keep that discipline for the AMD golden/hash tests too. No new
  `unsafe` is introduced (there is none today).

## Gates

- Full portable suite for `vmm-core`: `cargo build -p vmm-core --all-features`,
  `cargo nextest run -p vmm-core --all-features`, `cargo clippy -p vmm-core --all-features
  --all-targets -- -D warnings`, `cargo fmt -p vmm-core -- --check`, `cargo deny check`.
- **Cross-target clippy** for the Linux side: `cargo clippy --target
  x86_64-unknown-linux-gnu -p vmm-core --all-features -- -D warnings` (Mac-only gates
  cannot see `cfg(linux)` breakage).
- **Miri**: `cargo +nightly miri test -p vmm-core` (pinned nightly + `MIRIFLAGS` per
  `quality.yml`'s `miri` job) — must not regress; the AMD serialization tests carry the
  same `#[cfg_attr(miri, ignore)]` discipline as the Intel ones.
- **Intel byte-identity gate**: computed `contract_hash()` == committed Intel
  `contract_hash`; golden `canonical-v5.txt` (or unchanged `canonical-v4.txt` under the
  zero-drift grammar) matches. **Existing Intel disposition/CPUID/filter tests green
  unchanged** — these are the proof of byte-identity through the restructure.
- **AMD draft gates**: AMD computed hash == committed AMD hash; AMD golden matches; draft
  guard green; `verify-on-silicon` coverage green; disjointness green; mixed-vendor refusal
  green; format-invariance proptest (≥256 cases native).

## Environment

Fully Mac-portable. No box, no SSH, no beads-DB requirements beyond the normal worker flow.
One worktree per the conventions:

```sh
git worktree add ../harmony-task-contract-vendor-axis -b task/contract-vendor-axis
```

All commits on `task/contract-vendor-axis`, touching only `docs/CPU-MSR-CONTRACT.md`,
`docs/cpu-msr-contract.toml` (header only), `docs/cpu-msr-contract-amd-draft.toml` (new),
and `consonance/vmm-core/src/vendor/x86/contract/`. Lands via a normal reviewed PR; the
spike-*execution* discipline in `docs/AMD-EPYC.md` governs the future AE-4 hardware run,
not this task.

## Done means

A `task/contract-vendor-axis` branch with: the vendor axis in the grammar; the AMD draft
TOML loadable + canonicalizable with its own pinned `contract_hash` and golden; the Intel
column byte-identical (proven by the unchanged Intel tests + the regenerated/unchanged
Intel hash and golden, per Deliverable 6); every AMD enforcement cell marked
`verify-on-silicon`; the AMD draft unreachable from any live VM construction path
(structural draft-only guard); all portable + cross-target + Miri gates green;
`IMPLEMENTATION.md` with the AE-4 ratification dependency and the ARM-analogue-is-future-work
note; `docs/CPU-MSR-CONTRACT.md` framing updated without touching an Intel table row.

## Non-goals

- **No SVM enforcement backend.** No VMCB MSR-permission-bitmap wiring, no VMCB CPUID
  intercept — that is AE-4's on-silicon truth table, not this task.
- **No `KVM_X86_SET_MSR_FILTER` changes.** The Intel filter path is unchanged; the AMD
  filter path is not built (the draft is not wired into enforcement).
- **No live AMD wiring.** No VM construction path selects the AMD contract; no `boot`
  change; no `bringup` change beyond what the loader restructure mechanically forces.
- **No ARM contract analogue.** The ARM contract (`docs/ARCH-BOUNDARY.md` §B —
  ID-reg freeze + trapped-sysreg table, a *different schema*) is explicitly future work
  (`hm-cbt`); do not build it, do not generalize the grammar toward it speculatively.
- **No second reproducer artifact.** Never fork the one-reproducer rule: no second loader,
  no second serializer, no hand-duplicated shared-ISA document.
- **No silicon measurements, no dispositions, no evidence manifests.** This is pre-build
  data + machinery; every AMD enforcement cell is explicitly unverified.
- **No `det-zenN-v1` generation guess.** The `N` is a placeholder pinned at AE-0; this task
  records the placeholder, it does not pick a Zen generation.

## Paul veto points (judgment calls for the foreman)

1. **File layout** — per-vendor TOML files sharing one grammar/loader
   (`docs/cpu-msr-contract.toml` Intel + `docs/cpu-msr-contract-amd-draft.toml` AMD, vendor
   axis in the `[contract]` header) vs one TOML with a vendor axis per entry. Spec decision:
   **per-vendor files** (smaller diff, Intel byte-identity easier to prove, the AMD draft's
   draft-only status is structurally a separate artifact). Veto if the foreman prefers a
   single multi-vendor file.
2. **`verify-on-silicon` grammar shape** — a row-level `verified = "on-silicon-pending-AE4"`
   qualifier in the hashed canonical form vs a section-level marker vs an out-of-band
   manifest. Spec decision: **row-level qualifier in the hashed form**, so a silently-trusted
   AMD row is hash-breaking. Veto if the foreman prefers it out-of-band (un-hashed).
3. **Where the vendor axis lives in the loader** — `VendorId` enum inside
   `vendor/x86/contract/` (both Intel and AMD are x86 vendors) vs a vendor-neutral
   `vendor/contract/` module parameterized by vendor. Spec decision: **inside
   `vendor/x86/contract/`** — smaller, consistent with the tasks/108 split (engine names no
   vendor specifics; `vendor/x86/` is where x86 vendors live), and the ARM analogue is a
   different schema handled later. Veto if the foreman wants the vendor-neutral location now.
4. **Intel canonical-form handling** (Deliverable 6) — reviewed `contract-version` bump to
   5 with regenerated golden + hash (the vendor key added to the hashed header) vs a
   zero-drift grammar (the vendor key as non-hashed header metadata, Intel canonical form
   truly byte-identical, no version bump). Spec decision: **prefer zero-drift if feasible;
   fall back to the reviewed bump.** Veto/confirm which path the foreman wants before the
   golden is regenerated.
5. **`transfers-unchanged-pending-AE4` carry shape** — a section-level `transfer = ...`
   header vs per-row markers vs a single `[amd-transfers]` block. Spec decision:
   **section-level header**, to avoid a 3000-line hand-maintained near-duplicate. Veto if
   the foreman wants the shared-ISA rows explicitly materialized in the AMD file.
