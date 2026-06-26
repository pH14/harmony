> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment — class `debug-lbr`

Debug-store and last-branch-record surface: IA32_DEBUGCTL and everything it arms (legacy
LBR stacks, LBR_SELECT/TOS, LBR_INFO, architectural LBR, last-exception records, and the
silicon-debug interface). Match rule against `arch/x86/include/asm/msr-index.h` @ v6.18.35:
every name matching `MSR_LBR_*`, `MSR_ARCH_LBR_*`, `MSR_IA32_DEBUGCTLMSR`,
`MSR_IA32_LASTBRANCH*`, or `MSR_IA32_LASTINT*`, plus the SDM Vol 4 Table 2-2 ranges those
bases expand to (legacy LBR stacks 0x680-0x69F / 0x6C0-0x6DF, LBR_INFO 0xDC0-0xDDF,
architectural LBR 0x1200-0x121F / 0x1500-0x151F / 0x1600-0x161F) and IA32_DEBUG_INTERFACE
(0xC80). Blanket policy: **deny-gp on both directions for every entry**, logged loudly per
contract §1. Rationale for the class: INTEGRATION.md §7's PMU vector says the host owns the
PMU and no vPMU is exposed — KVM's LBR virtualization is vPMU-gated and its record format
is host-model-dependent (IA32_PERF_CAPABILITIES[5:0]), so any allow would leak host
identity; LBR_INFO entries additionally carry cycle counts since the last branch, a covert
timebase that bypasses the RDTSC trap; and branch-history records are recent control-flow
state that is either stale host data or a replay-divergence channel. Because the guest can
never establish state in these MSRs, none are captured in `vm_state` (INTEGRATION.md §4) —
KVM listing DEBUGCTL/LASTBRANCH/LASTINT in `msrs_to_save_base` governs host-side
save/restore ioctls only, which the MSR filter does not affect. Range rows below subsume
the base-register rows from `msr-index.h` (the file defines stack bases only; depth is
model-dependent, so the deny covers the maximal architectural span and the §1 default-deny
filter catches any wider model-specific layout). For gate-5 consistency the frozen CPUID
model must hide Arch LBR (CPUID.(EAX=07H,ECX=0):EDX[19] = 0, leaf 0x1C absent/zero) and
report no LBR format (no IA32_PERF_CAPABILITIES exposure), so no exposed feature bit
implies these MSRs.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_DEBUGCTLMSR | 0x1D9 | deny-gp | deny-gp | §7 PMU: DEBUGCTL arms LBR/BTS/BTF and freeze-on-PMI; host owns PMU and branch tracing, LBR format is host-dependent. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35; SDM Vol 3B §17.4.1 |
| MSR_LBR_SELECT | 0x1C8 | deny-gp | deny-gp | §7 PMU: LBR filtering control for a facility the guest must not see; no vPMU/LBR is virtualized. | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.2 |
| MSR_LBR_TOS | 0x1C9 | deny-gp | deny-gp | §7 PMU: top-of-stack pointer would expose host LBR depth and rotation state. | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.3 |
| MSR_IA32_LASTBRANCHFROMIP | 0x1DB | deny-gp | deny-gp | §7 PMU: legacy last-branch source IP leaks host/stale control-flow history. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTBRANCHTOIP | 0x1DC | deny-gp | deny-gp | §7 PMU: legacy last-branch target IP, same channel as FROM_IP. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTINTFROMIP | 0x1DD | deny-gp | deny-gp | §7 PMU: last-interrupt/exception source IP is asynchronous-event history, a replay-divergence channel. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_IA32_LASTINTTOIP | 0x1DE | deny-gp | deny-gp | §7 PMU: last-interrupt/exception target IP, paired with LASTINTFROMIP. | arch/x86/kvm/x86.c:msrs_to_save_base @ v6.18.35; msr-index.h @ v6.18.35 |
| MSR_LER_FROM_IP / MSR_LER_TO_IP | 0x1DD-0x1DE | deny-gp | deny-gp | §7 PMU: SDM names for the Last Exception Record pair — same registers as the two LASTINT rows above; deny. | SDM Vol 3B §17.4; SDM Vol 4 Table 2-2 |
| MSR_LBR_CORE_FROM | 0x40 | deny-gp | deny-gp | §7 PMU: base of legacy Core LBR from-stack (depth model-dependent; file defines base only); branch history denied. | msr-index.h @ v6.18.35 |
| MSR_LBR_CORE_TO | 0x60 | deny-gp | deny-gp | §7 PMU: base of legacy Core LBR to-stack; branch history denied. | msr-index.h @ v6.18.35 |
| MSR_LBR_NHM_FROM | 0x680 | deny-gp | deny-gp | §7 PMU: base of NHM+ LBR from-stack, up to 32-deep (0x680-0x69F) on the SKL-era baseline; file defines base only. | msr-index.h @ v6.18.35 |
| MSR_LBR_NHM_TO | 0x6C0 | deny-gp | deny-gp | §7 PMU: base of NHM+ LBR to-stack (0x6C0-0x6DF maximal span). | msr-index.h @ v6.18.35 |
| MSR_LASTBRANCH_0-15_FROM_IP | 0x680-0x68F | deny-gp | deny-gp | §7 PMU: legacy LBR-stack source IPs (Skylake layout) — recent control-flow history; subsumes the MSR_LBR_NHM_FROM base row. | SDM Vol 3B §17.4.8 |
| MSR_LASTBRANCH_0-15_TO_IP | 0x6C0-0x6CF | deny-gp | deny-gp | §7 PMU: legacy LBR-stack target IPs paired with FROM_IP; subsumes the MSR_LBR_NHM_TO base row. | SDM Vol 3B §17.4.8 |
| MSR_LBR_INFO_0 | 0xDC0-0xDDF | deny-gp | deny-gp | §7 PMU + TSC plumbing: per-entry LBR info carries cycle counts since last branch — a covert timebase bypassing the RDTSC trap; range per in-file comment "... 0xddf for _31". | msr-index.h @ v6.18.35; SDM Vol 3B §17.4.8.1 |
| MSR_ARCH_LBR_CTL | 0x14CE | deny-gp | deny-gp | §7 PMU: architectural-LBR enable/filter control; the facility is hidden (CPUID.7.0:EDX[19] = 0), so control access must #GP. | msr-index.h @ v6.18.35; SDM Vol 4 Table 2-2; arch/x86/events/intel/lbr.c |
| MSR_ARCH_LBR_DEPTH | 0x14CF | deny-gp | deny-gp | §7 PMU: arch-LBR depth select; reading would reveal host-supported depths (CPUID 0x1C), which is hidden host identity. | msr-index.h @ v6.18.35; SDM Vol 4 Table 2-2 |
| MSR_ARCH_LBR_FROM_0 | 0x1500 | deny-gp | deny-gp | §7 PMU: arch-LBR from-stack base; branch history denied (depth set by MSR_ARCH_LBR_DEPTH / CPUID 0x1C, max 64). | msr-index.h @ v6.18.35 |
| MSR_ARCH_LBR_TO_0 | 0x1600 | deny-gp | deny-gp | §7 PMU: arch-LBR to-stack base; branch history denied. | msr-index.h @ v6.18.35 |
| MSR_ARCH_LBR_INFO_0 | 0x1200 | deny-gp | deny-gp | §7 PMU + TSC plumbing: arch-LBR info base — per-entry cycle counts are a timebase; denied. | msr-index.h @ v6.18.35 |
| IA32_LBR_x_FROM_IP (architectural LBR) | 0x1500-0x151F | deny-gp | deny-gp | §7 PMU: architectural LBR entry source IPs; subsumes the MSR_ARCH_LBR_FROM_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_LBR_x_TO_IP (architectural LBR) | 0x1600-0x161F | deny-gp | deny-gp | §7 PMU: architectural LBR entry target IPs; subsumes the MSR_ARCH_LBR_TO_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_LBR_x_INFO (architectural LBR) | 0x1200-0x121F | deny-gp | deny-gp | §7 PMU + TSC plumbing: per-entry info includes cycle counts since last branch — timing leak; subsumes the MSR_ARCH_LBR_INFO_0 base row. | SDM Vol 3B §17.5; SDM Vol 4 Table 2-2 |
| IA32_DEBUG_INTERFACE | 0xC80 | deny-gp | deny-gp | §7 default-deny: silicon-debug enable/lock is host platform state and must not be probeable by the guest. | SDM Vol 4 Table 2-2 |

## Questions

[question] MSR_IA32_DEBUGCTLMSR (0x1D9): should a future contract revision permit a
guest-stateful mask limited to deterministic bits (e.g. BTF, bit 1, single-step-on-branches
for guest-side debugging) while keeping all LBR/BTS/freeze bits at #GP? Denied wholesale
for now (safe default); loosening requires proof that no host-dependent LBR/BTS state or
host-format dependency becomes guest-reachable, and a matching vm_state capture rule per
INTEGRATION.md §4.
