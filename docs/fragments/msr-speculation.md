> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment: class `speculation`

Class `speculation` covers the speculation-control and capability-enumeration MSRs: the
IBRS/STIBP/SSBD control word (`IA32_SPEC_CTRL`), the IBPB and L1D-flush command MSRs
(`IA32_PRED_CMD`, `IA32_FLUSH_CMD`), the read-only enumeration MSRs
(`IA32_ARCH_CAPABILITIES`, `IA32_CORE_CAPABILITIES`), TSX control (`IA32_TSX_CTRL`), the
DOITM timing-mode control (`IA32_UARCH_MISC_CTL`), and AMD's counterparts
(`VIRT_SPEC_CTRL`, `DE_CFG`). None of these carries time, but every one is a host
fingerprint — presence and value are functions of host microarchitecture and microcode
revision (several exist only after specific microcode updates) — which is exactly what §7
"CPUID stability" forbids the guest from inheriting: a passthrough would fold host
microcode state into guest-visible values and hence into the determinism gate's state
hashes. The policy, decided explicitly and versioned per the contract's §6 rather than
left to KVM defaults: every speculation *control* feature is hidden in the frozen CPUID
model and its MSR #GPs under `KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER`
(logged with index and RIP in userspace before injection) — safe because these control
writes are semantically idempotent barriers with no architecturally readable effect, so
the guest loses nothing it could ever observe. The guest is instead told it is
*unaffected* via a frozen `IA32_ARCH_CAPABILITIES` whose `*_NO` baseline keeps guest
mitigation code quiescent so it never reaches for the denied control MSRs, and
data-operand-independent timing (DOITM) is pinned *on* as part of the frozen model. No row
in this class is `allow-stateful`, so the class contributes nothing to §4's `vm_state`
capture list — its only guest-visible values are constants, trivially coherent across
snapshot/restore.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_SPEC_CTRL | 0x48 | deny-gp | deny-gp | Closes §7 "CPUID stability": presence and semantics of IBRS/STIBP/SSBD depend on host microcode (CPUID.7.0:EDX[26,27,31]); the frozen model clears all three so the MSR architecturally does not exist — the emulate-as-no-op alternative was considered and rejected in favor of hide+deny (explicit, versioned), and the frozen ARCH_CAPABILITIES *_NO baseline ensures guest mitigation code never attempts the write. | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h (0x48); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[26/27/31]); Intel SDM Vol.4 Table 2-2; INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_PRED_CMD | 0x49 | deny-gp | deny-gp | Read: architectural — IA32_PRED_CMD is a write-only command MSR, RDMSR #GPs on real silicon; write: closes §7 "CPUID stability" — IBPB/SBPB are not enumerated (CPUID.7.0:EDX[26]=0), and an IBPB is a semantically idempotent predictor barrier with no architecturally readable effect, so the deny is guest-invisible apart from the architecturally correct #GP. | linux-6.18.35 arch/x86/include/asm/msr-index.h (0x49, PRED_CMD_IBPB/SBPB); arch/x86/kvm/x86.c (kvm_set_msr_common MSR_IA32_PRED_CMD: write-only, reserved-bit checked); Intel SDM Vol.4 Table 2-2 (WO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_CORE_CAPS | 0xcf | deny-gp | deny-gp | Closes §7 "CPUID stability": IA32_CORE_CAPABILITIES enumerates host-dependent machinery — notably split-lock detect, which implies MSR_TEST_CTRL and host-policy-dependent #AC fault semantics, i.e. host-varying guest fault behavior; CPUID.7.0:EDX[30]=0 in the frozen model so the MSR is absent (write deny is also architectural — read-only MSR). | linux-6.18.35 arch/x86/include/asm/msr-index.h (0xcf, CORE_CAPS_SPLIT_LOCK_DETECT); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[30]); Intel SDM Vol.4 Table 2-2 (RO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_ARCH_CAPABILITIES | 0x10a | allow-fixed(0x400000000D10E171) | deny-gp | (round-6: DOITM bit 12 **CLEAR** — superseding the old 0x...F171; authoritative row is spine §3.9) Closes §7 "CPUID stability": frozen per-host-microcode fingerprint — bits RDCL_NO(0), SSB_NO(4), MDS_NO(5), PSCHANGE_MC_NO(6), TAA_NO(8), SBDR_SSDP_NO(13), FBSDP_NO(14), PSDP_NO(15), BHI_NO(20), PBRSB_NO(24), GDS_NO(26), RFDS_NO(27), ITS_NO(62) set; **DOITM(12) clear** (SKX lacks IA32_UARCH_MISC_CTL); all control-advertising bits clear (gate 5); requires CPUID.7.0:EDX[29]=1; write deny-gp architectural; the frozen value is answered by the userspace MSR-exit handler like every allow-fixed row (not KVM's host-sampled value). | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all; msr_based_features_all_except_vmx); arch/x86/include/asm/msr-index.h (ARCH_CAP_* bits); Intel SDM Vol.4 Table 2-2; spine §3.9; INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_FLUSH_CMD | 0x10b | deny-gp | deny-gp | Read: architectural — write-only command MSR; write: closes §7 "CPUID stability" — L1D_FLUSH is not enumerated (CPUID.7.0:EDX[28]=0), and an L1D flush is a purely microarchitectural idempotent action with no architecturally readable effect, so the deny is guest-invisible apart from the correct #GP. | linux-6.18.35 arch/x86/include/asm/msr-index.h (0x10b, L1D_FLUSH); arch/x86/kvm/x86.c (kvm_set_msr_common MSR_IA32_FLUSH_CMD: write-only); arch/x86/include/asm/cpufeatures.h (CPUID.7.0:EDX[28]); Intel SDM Vol.4 Table 2-2 (WO); INTEGRATION.md §7 (CPUID stability) |
| MSR_IA32_TSX_CTRL | 0x122 | deny-gp | deny-gp | Closes §7 "CPUID stability" plus the TSX nondeterminism gap rr can only mask at CPUID level: frozen ARCH_CAPABILITIES bit 7 (TSX_CTRL_MSR)=0 and CPUID.7.0:EBX[11 RTM, 4 HLE]=0 so the MSR architecturally does not exist; the actual determinism enforcement is host-side — RTM abort/commit depends on microarchitectural events, so the host pins IA32_TSX_CTRL=RTM_DISABLE+CPUID_CLEAR (or the baseline is TSX-free hardware) per the [question] below, because CPUID hiding alone cannot stop a kernel-mode guest from executing XBEGIN. | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h (0x122, TSX_CTRL_RTM_DISABLE/CPUID_CLEAR); Intel SDM Vol.4 Table 2-2; Intel TAA guidance (TSX Async Abort deep dive); rr (RTM masking is CPUID-only at ptrace level — arXiv:1705.05937); INTEGRATION.md §7 (CPUID stability) |
| IA32_UARCH_MISC_CTL | 0x1b01 | deny-gp | deny-gp | (round-6: now **deny-gp/deny-gp**, superseding the old allow-fixed(0x1); authoritative row is spine §3.9) Skylake-SP physically lacks this MSR, so DOITM is not advertised (ARCH_CAPABILITIES bit 12 = 0) and 0x1b01 #GPs both directions — the earlier "pin DOITM=1 and mirror on the host pCPU" obligation could not be met on the baseline. Gate-5 pair with the 0x10a DOITM-clear. | Intel "Data Operand Independent Timing ISA Guidance" (DOITM = Ice Lake+/microcode); lwn.net/Articles/921232; spine §3.9; INTEGRATION.md §7 (CPUID stability) |
| MSR_AMD64_VIRT_SPEC_CTRL | 0xc001011f | deny-gp | deny-gp | Closes §7 "CPUID stability": AMD-only paravirtualized SSBD, enumerated by CPUID.8000_0008:EBX[25] (VIRT_SSBD); the frozen baseline is a single Intel microarchitecture and task 06 declares AMD a non-goal, so the MSR architecturally does not exist — #GP, loudly logged. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h (0xc001011f); arch/x86/include/asm/cpufeatures.h (CPUID.8000_0008:EBX[25]); AMD APM Vol.2 (virtualized VIRT_SPEC_CTRL); tasks/06 non-goals (AMD); INTEGRATION.md §7 (CPUID stability) |
| MSR_AMD64_DE_CFG | 0xc0011029 | deny-gp | deny-gp | Closes §7 "CPUID stability": AMD-only decode-engine config (LFENCE dispatch-serializing bit) — a KVM msr-based *feature* MSR whose value is host CPU policy; the frozen Intel baseline never enumerates it, and LFENCE serialization on the host is a vmm-core/host concern, never guest-visible state — #GP, loudly logged. | linux-6.18.35 arch/x86/kvm/x86.c (msr_based_features_all_except_vmx); arch/x86/include/asm/msr-index.h (0xc0011029, DE_CFG_LFENCE_SERIALIZE); Documentation/virt/kvm/api.rst (KVM_GET_MSR_FEATURE_INDEX_LIST); tasks/06 non-goals (AMD); INTEGRATION.md §7 (CPUID stability) |

## Questions

- [question] TSX enforcement is instruction-level, not MSR-level: hiding RTM/HLE in CPUID
  and denying IA32_TSX_CTRL (0x122) does not stop a kernel-mode guest from *executing*
  XBEGIN/XEND/XTEST, and those instructions are absent from the contract's
  instruction-disposition list (task 06 deliverable §4); transaction abort/commit depends
  on microarchitectural events (cache pressure, interrupt timing) and is a
  replay-divergence source rr can only mask via CPUID. The instruction table must add a
  TSX row — either pin host IA32_TSX_CTRL = RTM_DISABLE+CPUID_CLEAR while the guest runs
  (forces a deterministic always-abort per Intel TAA guidance) or restrict the supported
  baseline to TSX-free hardware. Which?
- **[resolved, round-6] IA32_UARCH_MISC_CTL/DOITM:** DOITM is **cleared** (ARCH_CAPABILITIES
  = 0x400000000D10E171, bit 12 = 0) and 0x1b01 is **deny-gp/deny-gp** — Skylake-SP lacks the
  MSR, so the host-pin obligation could not be met. The two rows moved together (gate 5). See
  spine §3.9. (This supersedes the earlier DOITM=1 pin proposal.)
