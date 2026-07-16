# IMPLEMENTATION.md — task 06 (CPU/MSR determinism contract)

Deliverables: `docs/CPU-MSR-CONTRACT.md` (normative contract) and
`docs/cpu-msr-contract.toml` (machine-readable mirror). This is a research-and-writing
task — no crate, no cargo gates. Supporting per-domain rationale lives in
`docs/fragments/` (construction artifacts the contract cites).

## What this revision did

The contract was first authored by an earlier pass; this revision closes the **GPT-5.5
cross-model review** of PR #10 (and the foreman's RTC-CMOS finding). Every blocking item
was worked through:

1. **Missing §4 (instruction & VMX-control dispositions) and §5 (timer/time-device
   surface).** The document previously jumped from §3 (MSRs) straight to Versioning, with a
   note that the instruction/timer tables were "assembled separately / not yet part of this
   document." Both sections are now written in full:
   - **§4** dispositions CPUID, RDTSC/RDTSCP/RDPID, RDRAND/RDSEED, RDPMC, MONITOR/MWAIT,
     UMWAIT/TPAUSE/UMONITOR, XGETBV(0/1)/XSETBV, HLT, the TSX family (XBEGIN/XEND/XTEST),
     the XSAVE variants (XSAVEOPT/XSAVEC/XSAVES) + plain XSAVE/FXSAVE, SERIALIZE, SHA,
     PCONFIG — each with a normative mechanism token (`vmx-exit(...)`, `ud-by-control`,
     `gp-by-cpuid`, `permit-emulate`, `permit-native`, `host-pin`) and result. Consistent
     with docs/PLAN.md's trap table and gate 5.
   - **§5** disposes of the whole time-source surface: PIT (0x40–0x43), RTC/CMOS
     (0x70/0x71), HPET MMIO, ACPI PM timer, LAPIC timer, plus an xAPIC-MMIO sub-table for
     the timer registers (LVT-timer 0x320, TMICT 0x380, **TMCCT 0x390 = emulate-vtime**,
     TDCR 0x3E0).
   - Versioning → §6, Citations → §7. All internal cross-references retargeted (the
     ambiguous "§4" that means INTEGRATION §4 / vm_state was left untouched; only
     contract-internal versioning refs moved to §6).

2. **Missing MSR rows.** Added `IA32_APIC_BASE` (0x1b → allow-fixed(0xFEE00900) /
   deny-ignore-write) and `IA32_MTRRCAP` (0xFE → allow-fixed(0x508) / deny-gp), both of
   which the guest reads because CPUID advertises the feature (APIC, MTRR). Verified against
   linux-6.18 KVM source that 0x1b is serviced from `vcpu->arch.apic_base` even without an
   in-kernel irqchip and is a normal filterable MSR, and that KVM reports MTRRCAP = 0x508
   (`0x500 | KVM_NR_VAR_MTRR(8)`).

3. **CPUID↔MSR contradictions resolved:**
   - CPUID.7.0:EDX[29] is now **set** (`0x20000000`) so the `allow-fixed`
     `IA32_ARCH_CAPABILITIES` (0x10a) is properly enumerated (it is an enumeration MSR, not
     a control — exposing it leaks nothing; the `*_NO` bits keep guest mitigation code
     quiescent). The speculation *control* bits (IBRS/STIBP/L1D_FLUSH/SSBD) stay clear.
   - MCE/MCA stay hidden in CPUID.1:EDX, and the whole MCG/MCi MSR surface (0x179, 0x17a,
     0x400–0x427) is now **deny-gp** to match (the guest builds without `CONFIG_X86_MCE`, so
     it never probes them).
   - `MSR_IA32_MISC_ENABLE` (0x1a0) changed from raw `allow-stateful` to
     `allow-fixed(0x1801)` / `deny-ignore-write`. Verified that under `allow-stateful` a
     guest WRMSR to bit 18 makes KVM flip CPUID.1:ECX[3] (`cpuid_dynamic_bits_dirty` →
     `kvm_update_cpuid_runtime`), which would shift the frozen CPUID model. Freezing it
     closes that.

4. **CPUID unlisted-leaf fallback (#4) — rebutted, not a bug.** GPT-5.5 worried that
   `CPUID(0x80000009)` falls back to leaf 0x80000008's data. It does not: KVM's
   `get_out_of_range_cpuid_entry()` redirects out-of-range *extended* leaves to the max
   *basic* leaf (`*fn_ptr = basic->eax` = 0x20, all-zero), **not** the max extended leaf.
   Verified against linux-6.18 `arch/x86/kvm/cpuid.c`. The §2 wording now states this
   explicitly with the code trace, so the all-zero claim is airtight.

5. **MXCSR_MASK pinned.** Added a normative FPU/XSAVE save-image pin to §2
   (`MXCSR_MASK = 0x0000FFFF`, asserted host-equal at VM start) plus a `[question]`, and a
   header constant in the canonical form. FXSAVE/XSAVE write this host constant at offset 28
   and cannot be intercepted, so it is enforced by host-homogeneity (same posture as
   MAXPHYADDR).

6. **TOML canonicalized.** Rewritten to a strict, ASCII-only, mechanically-expandable
   grammar: `index` (single) | `index-lo`/`index-hi` (contiguous) | `index-members`
   (sparse); CPUID consolidated to one record per (leaf,subleaf) with explicit
   `eax/ebx/ecx/edx` (dyn cells preserved as `dyn:<expr>`); brand string expanded to
   concrete per-leaf words; en-dashes/`×4`/comma-lists removed from machine fields. The
   MTRR row is now an explicit 28-member set (no longer the contiguous `0x200–0x2FF`), so it
   **no longer overlaps** MCi_CTL2 (0x280–0x289). Added `[insn]`, `[timer]`, and `[mmio]`
   sections. Expansion to one-record-per-index covers **1013 MSR indices with zero
   overlap** (validated at generation).

7. **Microcode 0x79 (#7) decided.** Converted the open `[question]` into a binding: the
   deny-gp write is boot-safe because the pinned tinyconfig+fragment does **not** set
   `CONFIG_MICROCODE` and the task-04 initramfs spec carries **no** `kernel/x86/microcode/`
   cpio entry (verified in `guest/linux/build-initramfs.sh`). The contract binds the task-04
   image manifest accordingly. Exposing the hypervisor bit was rejected (it reopens the
   kvmclock probe vector).

## Round 2 — second GPT-5.5 cross-model review

The round-1 fixes landed and surfaced deeper issues; all 9 blocking + 1 suggestion were
addressed (kernel facts re-verified against linux-6.18 KVM source):

1. **RDPID (§4)** — corrected: virtual CPUID.7.0:ECX[22]=0 does **not** force #UD and there
   is no VMX RDPID control, so presence is host-variable. Moved to
   `host-pin(host-homogeneity)` with an explicit VM-start assertion; the value-when-present
   (vm_state TSC_AUX) stays deterministic. [question] 4 rewritten.
2. **RTC/CMOS (§5)** — the blanket `deny-ignore-write` broke the `out 0x70,idx; in 0x71`
   protocol. Added a CMOS register sub-table: **port 0x70 index write is honored** (latched
   into emulated state); time regs `emulate-vtime` (frozen epoch + V-time, BCD); Status A
   (UIP=0, frozen 0x26), Status B (24h+BCD, 0x02), Status C (0x00, read-clears), Status D
   (VRT=1, 0x80) all `allow-fixed`; CMOS RAM frozen; IRQ8 only ever fired V-time-driven.
   Verified `mc146818_get_time` is read-only at boot and reads exactly Status A / time regs /
   Status B `RTC_DM_BINARY`.
3. **PIT port 0x61 (§5)** — added: bit 4 (refresh toggle) and bit 5 (ch-2 OUT) are
   `emulate-vtime` (V-time, not host time), gate/speaker writes `emulate-timerqueue`.
4. **MSR_MISC_FEATURES_ENABLES 0x140 (§3.13)** — changed `allow-stateful` → `deny-gp`:
   PLATFORM_INFO[31]=0 means CPUID-faulting is unadvertised, so the MSR must not exist;
   otherwise a guest could enable CPL3 CPUID #GP and break the frozen model. Verified KVM
   gates the enabling write on `supports_cpuid_fault()` = PLATFORM_INFO[31].
5. **IA32_TSC write → TSC_ADJUST (§3.3)** — added the SDM coupling: WRMSR 0x10 adds
   `(new−old)` to IA32_TSC_ADJUST in vm_state (verified KVM does `ia32_tsc_adjust_msr +=
   adj`), so RDMSR 0x3b reflects it.
6. **xAPIC non-timer MMIO (§5)** — promoted the full register set to normative, hashable
   `mmio` rows (ID 0x20 = fixed 0, Version 0x30 = fixed 0x00050014 for our no-CMCI 6-LVT
   model, TPR/PPR/EOI/LDR/DFR/SIVR, ISR/TMR/IRR banks, ESR, ICR, all LVTs, timer regs).
7. **§6 dyn cells** — defined an enumerated rule-token serialization
   (`dyn:osxsave:<base>`, `dyn:level-echo:<type>`, `dyn:xcr0-xsavesize`) so the hash is
   well-defined: the canonical form binds the *rule*, not an unknowable per-state value.
8. **TOML strict grammar** — expanded comma leaf-lists to one record per leaf, dropped the
   `(any unlisted)` catch-all (covered by `[cpuid].default`), 8-hex leaf/subleaf (tokens
   `*`/`N+`/`a-b` documented), lowercase hex, allow-fixed params padded to 16-hex.
9. **TOML [insn] mirror** — added XABORT, XRSTOR, FXRSTOR, XRSTORS; RDPID → host-pin.
10. **APIC_BASE EXTD (suggestion)** — reconciled §3.12 prose with the `deny-ignore-write`
    disposition: an EXTD-set write is dropped (EXTD stays 0, enforced by value), the
    in-kernel reserved-bit #GP is the defense-in-depth fallback.

## Round 3 — third GPT-5.5 cross-model review

Two structural themes; fixed at the root, in the **hashed/normative** surface (§6 + TOML),
not Rationale prose:

**Theme A — land fixes in the hashed surface:**
- **TSC↔TSC_ADJUST coupling (A1)** — §6 now defines an enumerated set of `emulate-*` formula
  ids (`vclock.tsc`, `vclock.tsc.write`, `vclock.tsc_adjust(.write)`,
  `timerqueue.tsc_deadline(.write)`); the write-delta `(new−old)` **and** the recompute of
  armed TSC-deadline TimerQueue entries are part of those hashed definitions. TOML carries the
  ids as read-param/write-param on 0x10/0x3b/0x6e0.
- **RTC epoch (A2)** — added hashed header constant `rtc-epoch=1577836800` (2020-01-01Z);
  `read_persistent_clock64()` is now reproducible. Alarm regs 0x01/0x03/0x05 reconciled to
  `allow-fixed(0)` (was a table/Rationale mismatch); time regs `emulate-vtime`.
- **PIT port 0x61 (A3)** — its own canonical record `timer pit-portb` (no longer collapsed
  into `pit`); bit-4 refresh-toggle bound to hashed `pit-refresh-ns=15085` with initial
  phase 0; bit-5 = ch-2 OUT from V-time.
- **xAPIC reserved offsets + EOI (A4)** — added normative `mmio` rows for APR 0x90, RRD 0xC0,
  LVT_CMCI 0x2F0, self-IPI 0x3F0, plus a hashed `mmio-default` (read allow-fixed(0), write
  deny-ignore-write) for the rest of the page. EOI 0xB0 read fixed from the invalid
  `deny-ignore-write` (a write token) to `allow-fixed(0)`.

**Theme B — instruction presence isn't enforced by CPUID masking:**
- **VMCALL (B1)** — added: `vmx-exit(vmcall-unconditional)` → hypercall dispatcher (it exits
  in VMX non-root regardless of CPUID.HYPERVISOR=0).
- **XGETBV(ECX=1)/XSAVE-variants/HRESET (B2)** — each given an explicit disposition. The §4
  host-homogeneity section now states **three explicit dispositions** (none "undefined"):
  #UD-by-baseline-absence (RDPID, PCONFIG, HRESET — all post-Skylake), scoped-to-the-pinned-
  stack (XSAVEOPT/XSAVEC/XSAVES, XGETBV ECX=1 — present on SKX), and output-deterministic
  (SERIALIZE/SHA). XGETBV(ECX=1) moved from the unenforceable `gp-by-cpuid` to host-homogeneity.
- **RDPID on SKX (B3)** — Skylake-SP does **not** implement RDPID (Ice Lake+), so the baseline
  answer is **#UD** by absence; the row and [question] 4 are now consistent.

**Suggestion — exhaustive Hyper-V SynIC/stimer (S1):** added explicit `deny-gp` rows for
SVERSION/SIEFP/SIMP/EOM (0x40000081–84), SINT0–15 (0x90–9f), and STIMER0_COUNT/STIMER1–3
(0xb1–b7). These are macro-defined in `hvgdk_mini.h` but not separate `emulated_msrs_all`
entries (KVM services them via its per-MSR switch — verified); the §3.2 match rule was
extended to row the full SynIC/stimer set for exhaustiveness.

TOML now: 53 cpuid + 289 msr (**1040 disjoint indices**) + 33 insn + 6 timer + 14 cmos +
48 mmio.

## Round 4 — integrator ruling: host-homogeneity is the model

The integrator ratified host-homogeneity (single-tenant, identical model+stepping+microcode,
pinned cores) as *the* determinism model. Two requirements, both done:

1. **Documented prominently** — new **§1.1 "Host-homogeneity assumption — the determinism
   domain"** near the top: defines the fleet precisely (vendor + family/model/stepping 06_55H/4
   + identical microcode rev + single pinned core, single-tenant), the VM-start assertions
   vmm-core makes (and refuses to run on mismatch), and — critically — **what homogeneity does
   NOT give**: it does not make an op deterministic if its result depends on hidden µarch state
   that varies run-to-run *on one core* (XINUSE, XSAVE init/modified tracking, predictor
   history, cache/TLB). Every relying row references §1.1.
2. **Proved no op injects run-to-run nondeterminism even within the fleet** — a normative
   five-class framework, one class per op (in the hashed `insn` record):
   - **(a) `arch`** — output is a pure function of reproduced architectural/vm_state:
     XGETBV(ECX=0), plain FXSAVE/XSAVE/XRSTOR/FXRSTOR. (RDPID's value path also qualifies —
     §3.3 now *requires* the guest TSC_AUX be loaded into the physical MSR during guest
     execution, so a native read never sees the host per-core value.)
   - **(b) `fault-absent`** — #UD because the baseline host *physically lacks* it; vmm-core
     asserts absence. **Verified against Skylake-SP**: RDPID, SERIALIZE, SHA-NI, HRESET,
     PCONFIG, WAITPKG (UMWAIT/TPAUSE/UMONITOR) are all post-Skylake → genuine #UD. (Not a
     "CPUID hides it" false-fault.)
   - **(c) `intercept`** — VMX exit / unconditional exit / host MSR pin: CPUID, VMCALL,
     RDTSC/RDTSCP, RDRAND/RDSEED, RDPMC, MONITOR/MWAIT, XSETBV, HLT, TSX.
   - **(d) `invisible`** — only timing/µarch effect, V-time-hidden (robustness note for
     SERIALIZE/HRESET on a future baseline that adds them).
   - **(e) `scope`** — the sharp case: **XGETBV(ECX=1) and XSAVEOPT/XSAVEC/XSAVES/XRSTORS are
     verified PRESENT on SKX**, return hidden-µarch state, and are **uninterceptable**. They
     are NOT fixed by homogeneity. The guarantee is scoped to the pinned image, proved by
     (i) the kernel selecting its code path from the *virtual* CPUID it reads (cleared in §2)
     and (ii) a **build-time opcode scan** of the guest image that vmm-core enforces — not a
     hidden CPUID bit. An arbitrary guest executing one is stated to be out of the guarantee.

   The **danger-zone false-fault** the ruling warned about was fixed: §2's leaf-0xD.1 row no
   longer claims XGETBV(ECX=1) "#GPs architecturally" (false on a host with XGETBV1) — it is
   now class (e).

The hashed surface gained: per-`insn` `determinism` class, and a `[host-assert]` record
(family/model/stepping, microcode rev, MXCSR_MASK, MAXPHYADDR-min, the `host-absent`
instruction set, and the `image-scan-forbid` opcode set). A self-check enforces that the
fault-absent insn set == host-absent and the scope insn set == image-scan-forbid.

## Round 5 — integrator ruling: cooperative-guest threat model

1. **Threat model documented; unsound opcode-scan removed.** New prominent **§1.2 "Threat
   model — cooperative guest"**: the determinism guarantee is for the project's own
   CPUID-respecting Linux payload. The class-(e) ops (XGETBV ECX=1, XSAVEOPT/XSAVEC/XSAVES/
   XRSTORS) are not VMX-trappable and not absent on SKX, so the only defense is that the
   cooperative guest never executes them (it honors the frozen CPUID). The earlier
   `image-scan-forbid` "opcode scan" is **removed as unsound** — XGETBV(ECX=1) shares opcode
   `0f 01 d0` with the allowed XGETBV(ECX=0), and Linux carries the XSAVE encodings as
   CPUID-gated alternatives a static scan can't reason about. The **residual risk** (an
   adversarial guest executing the hidden opcode → hidden XSAVE/XINUSE tracking →
   run-to-run divergence) is stated explicitly as **out of scope**, not overclaimed.
2. **TSX neutralized in hardware (hashed).** Added `host-assert rtm-absent true`: vmm-core
   pins `IA32_TSX_CTRL = RTM_DISABLE|TSX_CPUID_CLEAR` and refuses to start if the host lacks
   IA32_TSX_CTRL — so a CPUID-hidden XBEGIN faults rather than executing. This is the one
   danger-zone op fully disable-able in hardware; it is now a hashed assertion, not prose.
3. **Microcode split.** `host-assert host-microcode-rev` (the *physical* host's fleet-pinned
   revision) is now distinct from `guest-ucode-rev` (the guest-visible fake BIOS_SIGN_ID
   0x100000000, MSR 0x8b) — a literal impl no longer rejects real SKX hosts.
4. **Instruction vocabulary reconciled** into one closed set used identically in §4, §6, and
   the TOML: mechanism ∈ {`vmx-exit(...)`, `host-pin(...)`, `permit-native`, `fault-absent`,
   `native-uninterceptable`}; determinism ∈ {arch, fault-absent, intercept, invisible,
   scope}. RDPID/SERIALIZE/SHA/UMWAIT/TPAUSE/UMONITOR are `fault-absent` everywhere; the
   removed `gp-by-cpuid`/`ud-by-control`/`permit-emulate`/`scope`-as-mechanism tokens are gone.
5. **Stale RDPID prose fixed** ([question] 4): SKX lacks RDPID → #UD by absence, consistent
   with the normative table/TOML.
6. **Timer/xAPIC/CMOS semantics hash-bound.** A bare `emulate` token is no longer allowed;
   every emulate cell carries a closed §6 formula id, defined normatively (hashed):
   `pit.ch0`, `pit.portb`, `cmos.index-latch`, `cmos.tod`, `apic.ppr`, `apic.eoi`,
   `apic.esr`, `apic.icr`, `apic.tmcct`, `apic.timer-arm`. The PIT channel frequency/mode,
   xAPIC PPR/EOI/ESR/ICR semantics, and CMOS read values now live in the hashed canonical
   form, not Rationale prose.

The self-check now enforces: the closed mechanism vocabulary; fault-absent insn set ==
host-absent; no `image-scan-forbid`/`microcode-rev` conflation; host-microcode-rev ≠
guest-ucode-rev; rtm-absent true; no bare `emulate` in timer/cmos/mmio; and the absence of
the stale XGETBV1 false-fault claim. TOML: 53 cpuid + 289 msr (1040 disjoint) + 33 insn +
host-assert + 6 timer + 14 cmos + 48 mmio.

## Round 6 — mechanical cleanup (framing accepted)

Cross-model review accepted the cooperative-guest + homogeneity framing; round 6 was
completeness/consistency/grammar. All 5 blocking + the questions/suggestions:

1. **Stale §4 opcode-scan text deleted** — the §4 preamble no longer says "kernel
   CPUID-gating + build-time opcode scan"; it points to §1.2 (which records the scan as
   removed/unsound).
2. **VMX/EPT instruction row added** — VMXON/VMXOFF/VMLAUNCH/VMRESUME/VMPTRLD/VMPTRST/
   VMCLEAR/VMREAD/VMWRITE/INVEPT/INVVPID → `vmx-exit` → **#UD** (class intercept): VMX
   hidden, CR4.VMXE reserved, nested VMX not exposed, so KVM injects #UD; the MSR 0x480–0x491
   deny-gp rows don't cover the raw opcodes. 11 per-mnemonic TOML insn records.
3. **PIT channels 1/2 + command/read-back + port-0x61 ch2 model** — the single `pit` record
   became five hashed records (`pit-ch0` 0x40, `pit-ch1` 0x41, `pit-ch2` 0x42, `pit-cmd`
   0x43, `pit-portb` 0x61), each with a closed formula id (`pit.ch0/ch1/ch2/cmd/portb`); the
   channel-2 model behind port 0x61 is now explicit.
4. **Microcode split finished in §6** — the §6 canonical form pins the concrete
   `host-microcode-rev 0x000000000200005e` (no placeholder) and lists `guest-ucode-rev` in
   the fixed host-assert key order, matching the TOML exactly.
5. **TOML token canonicalization (systematic)** — added the closed `emulate-device` token;
   every read/write token in `[timer]`/`[cmos]`/`[mmio]` is now a member of the closed §6 set
   (`allow-fixed`, `allow-stateful`, `emulate-vtime`, `emulate-timerqueue`, `emulate-device`,
   `deny-gp`, `deny-ignore-write`) with a formula-id where it's an emulate. The open tokens
   `emulate`, `route-by-index`, `see-cmos-subtable` are **gone** (a self-check enforces it).
6. **TSX scoped + de-overclaimed** — XBEGIN/XEND/XABORT now state `RTM_DISABLE` → **#UD**,
   XTEST → 0 (no claimed EAX/ZF abort values); the cooperative guest never emits them (RTM
   CPUID-hidden) and the hashed `rtm-absent` pin neuters them even if executed.
7. **Questions/suggestions:** added HV synthetic-APIC MSRs `HV_X64_MSR_EOI/ICR/TPR`
   (0x40000070–72) deny-gp; **cleared DOITM** (ARCH_CAPABILITIES bit 12 = 0,
   `0x400000000d10e171`) and made `IA32_UARCH_MISC_CTL` 0x1b01 deny-gp (SKX lacks it, no host
   pin to mirror); fixed the ARCH_CAPABILITIES "no userspace read emulation" rationale (it IS
   answered by userspace); fixed the §4 "SERIALIZE/SHA baseline-present" prose (they're SKX
   `fault-absent`).

TOML: 53 cpuid + 290 msr (1043 disjoint) + 44 insn (incl. 11 VMX) + host-assert (host/guest
ucode split, rtm-absent, host-absent) + 9 timer (5 PIT + RTC/HPET/ACPI-PM/LAPIC) + 14 cmos +
48 mmio. Self-check enforces the closed token set and the round-6 invariants.

## Round 7 — fragment reconciliation + PKRU closure + PKRS/PASID verification

**F3 — fragments reconciled / marked non-normative.** Every `docs/fragments/*.md` now carries
a prominent **NON-NORMATIVE construction-artifact** banner pointing at the spine + TOML as
authoritative (a self-check enforces the banner on all 17). The three the review flagged were
also corrected to the round-6 decisions so they no longer contradict: `msr-speculation.md`
(ARCH_CAPABILITIES → `0x400000000D10E171` DOITM-clear, 0x1b01 → deny-gp/deny-gp, DOITM
[question] resolved), `cpuid-model.md` (leaf-7 EDX → `const(0x20000000)`, ARCH_CAP[29]=1),
`msr-arch-stateful.md` (MISC_ENABLE → allow-fixed(0x1801)/deny-ignore-write,
MISC_FEATURES_ENABLES → deny-gp/deny-gp). Spine §2 leaf-7 EDX was already `0x20000000`; the TOML
already carried the round-6 values.

**F1 — RDPKRU/WRPKRU + PKRU hard-closed.** Hiding PKU (CPUID.7.0:ECX[3]=0) makes `CR4.PKE` a
**KVM-reserved bit** (verified: `__cr4_reserved_bits` adds `X86_CR4_PKE` when the guest lacks
`X86_FEATURE_PKU`), so a guest MOV-CR4 setting PKE=1 #GPs and CR4.PKE stays 0 ⇒ RDPKRU/WRPKRU
**#UD unconditionally** (SDM). Added: a new closed mechanism token `cr4-pin(pke=0)`, a §4
RDPKRU/WRPKRU instruction row (class intercept — a *hard* closure, not cooperative-scoped), a
hashed `host-assert cr4-force-reserved [PKE, PKS]`, and §1.2 prose that names PKRU and explains
it is hard-closed (so the class-(e) cooperative residual is exactly XGETBV(ECX=1) +
XSAVEOPT/XSAVEC/XSAVES/XRSTORS — the §1 overclaim is fixed). PKRU is also not an XCR0 component
here, so it is never in the XSAVE image / vm_state.

**F2 — PKRS (0x6e1) / PASID (0xd93) verified out of the reference set.** Verified against
v6.18.35: `IA32_PKRS` is **not even defined** in `msr-index.h` (PKS reverted upstream) and
`IA32_PASID` is defined but in **none** of KVM's static MSR arrays. Neither is named in §7 nor
matched by a class rule, so by the reference-set definition they get **no explicit row** — the
§1 default-deny catch-all denies-and-logs them. They are also unreachable (CR4.PKS permanently
reserved in KVM; ENQCMD has no CR4 bit and PASID MSR denied). Documented as a §3.13
reference-set note rather than fabricated rows.

**C1 — CMOS range grammar (second reviewer).** The TOML `[cmos]` used `idx:0x06-0x09` /
`idx:0x0e-0x7f` ranges, but the §6 CMOS grammar only allowed a single `idx:0xNN`. Fixed by
**documenting the `idx:0xLO-0xHI` range form** in the §6 grammar (expands one record per index,
exactly like an MSR `index-lo`/`index-hi` range) so grammar and usage agree; the self-check now
validates every cmos `where` against the grammar.

**C2 — trap-backend dependency (second reviewer; most consequential).** The contract's
`RDTSC/RDTSCP → f(V-time)` and `RDRAND/RDSEED → seeded stream` traps require RDTSC-exiting and
RDRAND/RDSEED-exiting — VMX controls **stock upstream KVM does not surface to a userspace VMM**
(stock KVM virtualizes the TSC in-kernel on the *host* TSC, and lets RDRAND/RDSEED hit the
hardware RNG). docs/PLAN.md asserts these controls are available but names a stock `kvm-ioctls`
backend and never says how the exits are surfaced; INTEGRATION.md §6 defers the kernel-patch
question — i.e. **docs/PLAN.md is underspecified here**. Rather than resolve silently, I (1) added a
normative **"Enforcement backend dependency"** note to §1 stating exactly which traps are
backend-dependent (only RDTSC/RDTSCP + RDRAND/RDSEED; CPUID/HLT/RDPMC/MONITOR/MWAIT/XSETBV/
VMCALL/VMX-opcodes are stock-serviceable), (2) tagged those §4 rows and the `vmx-exit` vocab +
leaf-1 ECX RDRAND prose as backend-dependent, and (3) raised **[question] Backend** for the
integrator: (a) patched KVM, (b) direct-VMX, or (c) stock-only (which **breaks** the V-time/
entropy determinism and forces a redesign). The contract is written for (a)/(b); this is the
one load-bearing open question.

TOML: 53 cpuid + 290 msr (1043 disjoint) + 46 insn (incl. RDPKRU/WRPKRU + 11 VMX) +
`[host-assert]` (now with `cr4-force-reserved`) + 9 timer + 14 cmos + 48 mmio. The fragments
are non-normative; the spine + TOML remain the authoritative, self-checked surface. (C1/C2 are
spine grammar/prose + a [question]; no hashed-value change, so the TOML is unchanged.)

## Round 7 finding #6 — TSC-deadline hidden + 0x6e0 deny-gp (§6 version bump → v2)

A determinism-correctness fix (investigated against v6.18.35 KVM source; aligns with Ruling
R1, PR #21). The round-6 "expose CPUID.1:ECX[24]=1 + IA32_TSC_DEADLINE emulate-timerqueue" is
**unimplementable on the contract's stated stock-KVM backend**: under `KVM_IRQCHIP_NONE` a guest
`WRMSR 0x6e0` is taken by the in-kernel WRMSR **fastpath before the userspace MSR filter**
(`vmx.c handle_fastpath_wrmsr`; the TSC_DEADLINE case lacks the `lapic_in_kernel` bail the
x2APIC-ICR case has), and `kvm_set_lapic_tscdeadline_msr` no-ops with no in-kernel apic — so
`emulate-timerqueue` never runs and Linux's TSC-deadline clockevent arms-but-never-fires
(latent hang). Fix (hide + deny; nothing lost — the LAPIC timer is the xAPIC LVT one-shot/
periodic MMIO model, §5, already V-time-emulated):
- **CPUID leaf-1 ECX `0x77DA3203 → 0x76DA3203`** (clear bit 24, TSC-Deadline hidden) — spine §2,
  TOML (`dyn:osxsave:0x76da3203`), `cpuid-model.md`.
- **`IA32_TSC_DEADLINE` (0x6e0) `emulate-timerqueue → deny-gp/deny-gp`** — spine §3.3, TOML,
  `msr-tsc.md`. The `timerqueue.tsc_deadline[.write]` formula ids are **removed** from §6.
- Deleted the false "TSC-deadline remains exposable independently (api.rst permits ECX[24])"
  premise in the spine §3.12 preamble and `msr-x2apic.md` (api.rst permitting the CPUID bit
  does not make the WRMSR serviceable — the fastpath bypasses the filter).
- Reworded the LVT-timer rows (spine §3.12 + §5 + §5 mode note, `msr-x2apic.md`) to
  "one-shot/periodic only; TSC-deadline mode unavailable (ECX[24]=0)".
- Updated the coverage lines; reconciled the IA32_TSC/TSC_ADJUST write formulas (no
  TSC-deadline TimerQueue entries to recompute).
- **§6 version bump: contract-version 1 → 2** (spine header, TOML `version = 2`) — a CPUID-model
  byte and an MSR row changed, so the contract body/hash changes; added a §6 **Version history**
  (v1 → v2) recording the body deltas. The self-check now asserts spine/TOML version == 2,
  leaf-1 ECX == `0x76da3203`, 0x6e0 == deny-gp, and that no `0x77da3203` / removed formula id
  remains.

x2APIC needed no change (already hidden + 0x800–0x8FF deny-gp + APIC_BASE.EXTD=0, R1-consistent).

## Round 8 — precision/consistency before freeze (4 × P2, no blocking)

1. **0x6e0 `deny-gp` is backend-dependent** (not plain enforcement). The same in-kernel WRMSR
   fastpath that defeats deadline-mode *emulation* also swallows an *adversarial* `WRMSR 0x6e0`
   before the MSR filter under stock KVM, so the logged #GP can't be delivered. Added 0x6e0 to
   the §1 "Enforcement backend dependency" list (now three surfaces: RDTSC/RDTSCP, RDRAND/RDSEED,
   0x6e0): `deny-gp` holds under the patched-KVM/direct-VMX backend; under stock KVM it degrades
   to a silent swallow for an out-of-scope adversarial guest (the cooperative guest never writes
   it, CPUID[24]=0). The §3.3 row carries the caveat (token stays the clean `deny-gp`).
2. **TSX result corrected: always-abort, not #UD.** On the TSX-capable SKX baseline with
   `IA32_TSX_CTRL.RTM_DISABLE`, RTM stays decodable-but-always-aborting (so RTM-fallback software
   keeps working) — `XBEGIN` deterministically aborts to its fallback with a **fixed
   non-retryable EAX** (the transaction never executes → no µarch dependence), `XTEST`=0, `XEND`
   #GP; `#UD` applies only on a genuinely TSX-absent host. Fixed the §4 row, §1.2 prose, the
   §3.9 + §4 TSX `[question]`s, and the TOML (`XBEGIN`/`XABORT` → `deterministic-abort`,
   `XEND` → `gp`, `XTEST` → `zero`). This corrects the round-6/7 over-correction to "#UD".
3. **§5 markdown tokens closed to match the TOML.** Replaced every bare `emulate` /
   `emulate:apic.*` / bare `emulate-timerqueue`/`emulate-vtime` cell in the §5 device, CMOS, and
   xAPIC-MMIO tables with the closed `<token>:<formula-id>` forms the TOML serializes
   (`emulate-device:cmos.index-latch`, `emulate-device:cmos.data-window`, `emulate-vtime:cmos.tod`,
   `emulate-device:apic.ppr/eoi/esr/icr`, `emulate-timerqueue:apic.timer-arm`,
   `emulate-vtime:pit.portb`/`emulate-device:pit.portb`, `emulate-device:cmos.subtable`,
   `emulate-vtime:apic.tmcct`) — so the authoritative markdown can serialize and markdown↔TOML
   agree. A self-check now fails on any bare `emulate` in §5.
4. **Body-hash registry made honest.** §6 previously asserted a `(contract-version, body-hash)`
   registry that CI/startup check; none is committed and a real body-hash needs the §6 canonical
   serializer (vmm-core tooling not in this docs deliverable). Reworded §6 to say so plainly: the
   registry is seeded with `(2, <v2 body-hash>)` and wired into CI **when the serializer lands**;
   until then the anchor is (contract-version + the §6 Version history). No false "the check
   already runs" claim.

No contract-version change (v2 holds — the TSX *result* tokens are body changes, but they were
introduced and corrected within the same unfrozen v2; the contract is not yet frozen/registered).
TOML: 53 cpuid + 290 msr (1043 disjoint) + 46 insn + host-assert + 9 timer + 14 cmos + 48 mmio.

## Round 9 — both-pass (codex + pi) blocking-clean; 2 pre-freeze residuals

1. **TSX consistency propagated** (the always-abort result was only in the §4 row + TOML insn
   rows; stale "→ #UD" survived in three non-normative spots). Fixed all three: the §1.1
   class-summary table ("TSX … → #UD" → "→ deterministic always-abort … not #UD"), the §6
   `[host-assert]` text, and the TOML host-assert comment. Also **renamed the host-assert key
   `rtm-absent` → `rtm-disabled`** (the old name implied absence/#UD; on the TSX-present SKX
   baseline `IA32_TSX_CTRL.RTM_DISABLE` makes RTM *decodable-but-always-aborting*, not absent)
   across spine §1.1/§1.2/§4/§6, the TOML, and the self-check. `#UD` now appears for TSX only as
   "applies only on a genuinely TSX-absent host."
2. **Formula-id immutability rule** (closes the hash-vs-semantics gap): the canonical record
   hashes the formula *id*, not its §6-prose definition, so a silent semantic correction under a
   fixed id would leave `contract_hash` unchanged while behaviour changed. Added a normative §6
   rule: **any semantic change to a formula definition requires a new formula id (and a version
   bump)** — ids are immutable in meaning; `<id> → <semantics>` is a frozen append-only mapping;
   only value-preserving wording edits are allowed under a fixed id. Extended to the `dyn:*`
   CPUID rule ids and the `insn` mechanism/result tokens. This makes the hashed id a sound proxy
   for the unhashed definition.

Self-check extended (no stale `rtm-absent`, §1.1 TSX reworded, formula-id rule present). No
contract-version change (pre-freeze v2). TOML changed only the host-assert key rename.

## How the TOML was regenerated (reproducibility)

The TOML was produced by a one-off generator that parses the prior TOML, applies the
disposition/value edits above, normalises to the strict grammar, fully expands and
**validates** MSR index disjointness, and appends the insn/timer/mmio records. The script
is not a deliverable (task 06 touches only the two doc files) and is not committed; the
committed TOML is the artifact. A `selfcheck` pass confirms: every disposition is in the §3
vocabulary, `allow-fixed` is read-only with a `read-param`, sections 1–7 are present and
ordered, the changed rows agree between markdown and TOML, CPUID 7.0:EDX = 0x20000000, and
no `TBD`/hedge remains. vmm-core's contract parser/serializer (§6) will subsume this
generation+validation logic.

## Known limitations / integrator must-know

- **Host-homogeneity assumption (§4).** A few instructions are neither interceptable nor
  stopped by virtual CPUID (the silicon checks *physical* CPUID): XSAVEOPT/XSAVEC/XSAVES,
  SERIALIZE, SHA, PCONFIG. Determinism for these rests on (a) the pinned kernel+busybox
  stack not emitting them and (b) a homogeneous host fleet matching the baseline µarch — the
  same assumption already made for MAXPHYADDR ≥ 46 and MXCSR_MASK. A non-conforming guest
  that deliberately executes one escapes the guarantee. Recorded as a `[question]`.
- **`[question]` rows need integrator ratification** (safe-by-default deny/pin with a
  documented flip condition, not TBDs): x2APIC hidden, MAXPHYADDR=46, frozen freq constants
  (2.0 GHz / 25 MHz / 100 MHz), **MXCSR_MASK=0x0000FFFF**, **TSX** (host pins
  IA32_TSX_CTRL=RTM_DISABLE+CPUID_CLEAR, or a TSX-free baseline), microcode revision 0x79/0x8b,
  CET/XSS exposure (would touch the §4-INTEGRATION XSAVE vm_state set), RDPID enforcement
  gap, and the instruction-presence residual above.
- **Kernel citations** are to v6.18.35 line numbers as authored; the specific code-path
  facts new in this revision (CPUID out-of-range redirect, 0x1b serviceability, MTRRCAP
  value, MISC_ENABLE bit-18 CPUID coupling) were re-verified against the linux-6.18 source
  (base release; the 6.18.35 point release is extremely unlikely to differ in these paths).
- **vmm-core obligations introduced:** assert `host MXCSR_MASK == 0x0000FFFF`,
  `guest-MAXPHYADDR ≤ host-MAXPHYADDR`, and a fixed **RDPID** disposition across the host
  fleet at VM start; pin host IA32_TSX_CTRL and host DOITM (IA32_UARCH_MISC_CTL bit 0)
  around KVM_RUN; service MSR 0x1b / 0xfe / 0x140 / 0x1a0 from the contract values via the
  userspace MSR-exit path (denied in the KVM filter, not allow-listed); on WRMSR IA32_TSC
  also fold `(new−old)` into the vm_state IA32_TSC_ADJUST; emulate the **CMOS** index/data
  ports (latch the 0x70 index so the 0x71 access reads the selected register) and **PIT port
  0x61** (refresh-toggle/ch-2-out from V-time); serve the whole xAPIC MMIO page from the
  userspace LAPIC per the §5 sub-table (TMCCT from V-time, never `ktime_get()`).
