> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment — class `power-thermal`

The `power-thermal` class is INTEGRATION.md §7's "Power/frequency" vector ("APERF/MPERF,
MPERF-adjacent, thermal/turbo MSRs — deny") expanded into a stated, mechanically checkable
match rule, plus the energy/idle-residency/hardware-feedback side channels that ride the
same bus: every MSR here either reads back real host physics (die temperature, consumed
energy, actual-vs-reference frequency, productive cycles, wall-clock idle residency,
Thread-Director feedback tables) or lets the guest *change* real host physics (P-state
requests, clock modulation, power limits, C-state policy, HWP requests) — both directions
are fatal to determinism, so **every row except the cross-referenced `MSR_PLATFORM_INFO`
is `deny-gp` for both reads and writes**: the access exits to userspace via
`KVM_X86_SET_MSR_FILTER` + `KVM_CAP_X86_USER_SPACE_MSR` (`KVM_MSR_EXIT_REASON_FILTER`),
is logged with MSR index and RIP, and only then is #GP injected — never a silent
passthrough, and deliberately #GP rather than a fixed zero because Linux probes these via
`rdmsr_safe` behind CPUID feature gates that the frozen model clears (CPUID.6:EAX[0]
digital thermal sensor, EAX[7..11] HWP, EAX[19] HFI, EAX[23] Thread Director,
CPUID.6:ECX[0] APERF/MPERF hardware-coordination feedback, ECX[3] EPB, CPUID.7.1:EAX[22]
HRESET, CPUID.1:EDX[22]/ECX[8] TM/TM2 — leaf 6 is otherwise zeroed; only the CPUID-model
fragment's ARAT bit, CPUID.6:EAX[2], may be set), so #GP is the architecturally consistent
answer; as defense in depth the task-04 pinned guest config leaves `CONFIG_CPU_FREQ`
unset and hides MWAIT so `intel_pstate`/`intel_idle` never bind. The RAPL energy counters
are denied as a family: they are a proven physical side channel (Platypus) and
monotonically reveal real work and real time done by the host. Class match rule (all
kernel citations at Linux tag v6.18.35, cross-checked against
`guest/linux/versions.lock` KERNEL_VERSION=6.18.35): the union of (a) exact names
{`MSR_IA32_APERF`, `MSR_IA32_MPERF`, `MSR_IA32_PERF_CTL`, `MSR_IA32_PERF_STATUS`,
`MSR_PLATFORM_INFO`, `MSR_PM_ENABLE`, `MSR_IA32_TEMPERATURE_TARGET`,
`MSR_IA32_ENERGY_PERF_BIAS`, `MSR_CORE_PERF_LIMIT_REASONS`}; (b) prefixes
{`MSR_IA32_THERM_*`, `MSR_IA32_PACKAGE_THERM_*`, `MSR_THERM2_*`, `MSR_HWP_*`}; (c)
substring {`*TURBO_RATIO_LIMIT*`}; (d) exact-name additions for the energy/idle/feedback
side channels {`MSR_IA32_POWER_CTL` (also in `x86.c:emulated_msrs_all`, i.e. reference-set
clause (a) of task 06 §3), `MSR_PKG_CST_CONFIG_CONTROL`, `MSR_RAPL_POWER_UNIT`,
`MSR_PKG_POWER_LIMIT`, `MSR_PKG_ENERGY_STATUS`, `MSR_DRAM_ENERGY_STATUS`,
`MSR_PP0_ENERGY_STATUS`, `MSR_PP1_ENERGY_STATUS`, `MSR_PLATFORM_ENERGY_STATUS`,
`MSR_PPERF`, `MSR_PERF_LIMIT_REASONS`, `MSR_PKG_C2_RESIDENCY`, `MSR_PKG_C3_RESIDENCY`,
`MSR_PKG_C6_RESIDENCY`, `MSR_PKG_C7_RESIDENCY`/`MSR_ATOM_PKG_C6_RESIDENCY`,
`MSR_CORE_C3_RESIDENCY`, `MSR_CORE_C6_RESIDENCY`, `MSR_CORE_C7_RESIDENCY`,
`MSR_KNL_CORE_C6_RESIDENCY`, `MSR_PKG_C8_RESIDENCY`, `MSR_PKG_C9_RESIDENCY`,
`MSR_PKG_C10_RESIDENCY`, `MSR_IA32_HW_FEEDBACK_PTR`, `MSR_IA32_HW_FEEDBACK_CONFIG`}, all
of (a)–(d) resolved against `arch/x86/include/asm/msr-index.h` at the pinned tag; plus
(e) two SDM/ISE-documented blocks with no `msr-index.h` define at this tag, listed
explicitly so the Thread-Director surface is visibly closed (the default-deny catch-all
covers them regardless): IA32_THREAD_FEEDBACK_CHAR/IA32_HW_FEEDBACK_CHAR (0x17d2–0x17d3,
inside the 0x17d0–0x17d4 range row) and IA32_HRESET_ENABLE (0x17da). `MSR_PLATFORM_INFO`
matches rule (a) but its normative row is owned by the `boot-baseline` fragment; it is
duplicated below with identical dispositions (harmless redundancy, same convention as the
`pmu` fragment's overlapping range rows). Note one naming correction versus upstream
notes: 0x64f is `MSR_PERF_LIMIT_REASONS` in msr-index.h at this tag —
`MSR_CORE_PERF_LIMIT_REASONS` is 0x690. Column grammar: `Read`/`Write` are drawn verbatim
from the task-06 §3 disposition vocabulary; `Rationale` is one line beginning with the §7
leak vector it closes (`§7 Power/frequency`); kernel citations are `file:line` at
v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_PLATFORM_INFO | 0xce | allow-fixed(bits 15:8 = frozen max non-turbo ratio = frozen-TSC-Hz / 100 MHz; all other bits 0) | deny-gp | §7 Power/frequency: hides host base/turbo ratios; matched by this class's exact-name rule but the normative row is owned by the boot-baseline fragment — dispositions identical by construction | x86.c:431 (emulated_msrs_all), x86.c:475 (msr_based_features_all_except_vmx); msr-index.h:98; SDM Vol.4 Table 2-2; docs/fragments/msr-boot-baseline.md |
| MSR_PKG_CST_CONFIG_CONTROL | 0xe2 | deny-gp | deny-gp | §7 Power/frequency: package C-state limit/demotion config — host idle policy is real-time state, and a guest write would change real host idle behavior; guest has no C-states (MWAIT hidden, HLT idle-skips per INTEGRATION.md §3) | msr-index.h:118; SDM Vol.4 model-specific MSR table |
| MSR_IA32_MPERF | 0xe7 | deny-gp | deny-gp | §7 Power/frequency: named verbatim in §7 — MPERF reference-cycle counter; APERF/MPERF ratio reveals real host frequency/utilization and reads are serializing on real counters; #GP not zero — Linux probes via rdmsr_safe only when CPUID.6:ECX[0]=1 (hidden) | msr-index.h:960; SDM Vol.3B 15.2; SDM Vol.4 Table 2-2; INTEGRATION.md §7 (Power/frequency) |
| MSR_IA32_APERF | 0xe8 | deny-gp | deny-gp | §7 Power/frequency: named verbatim in §7 — APERF actual-cycle counter; same canonical frequency/utilization side door as MPERF | msr-index.h:961; SDM Vol.3B 15.2; SDM Vol.4 Table 2-2; INTEGRATION.md §7 (Power/frequency) |
| MSR_IA32_PERF_STATUS | 0x198 | deny-gp | deny-gp | §7 Power/frequency: current P-state/voltage — real host frequency readout; pinned guest config leaves CONFIG_CPU_FREQ unset so no guest cpufreq driver issues unchecked reads | msr-index.h:950; SDM Vol.3B ch.15; SDM Vol.4 Table 2-2; guest/linux/config-fragment (CONFIG_CPU_FREQ unset) |
| MSR_IA32_PERF_CTL | 0x199 | deny-gp | deny-gp | §7 Power/frequency: P-state request — a guest write would change the real host clock, perturbing the V-time instrument itself | msr-index.h:951; SDM Vol.3B ch.15; SDM Vol.4 Table 2-2 |
| MSR_IA32_THERM_CONTROL | 0x19a | deny-gp | deny-gp | §7 Power/frequency: software clock-modulation duty cycle — guest write would throttle the real clock; read reflects host throttle policy | msr-index.h:963; SDM Vol.3B 15.8.3 |
| MSR_IA32_THERM_INTERRUPT | 0x19b | deny-gp | deny-gp | §7 Power/frequency: thermal interrupt thresholds keyed to real die temperature — both directions tie guest state to host physics | msr-index.h:964; SDM Vol.3B 15.8.2 |
| MSR_IA32_THERM_STATUS | 0x19c | deny-gp | deny-gp | §7 Power/frequency: real die temperature readout and sticky throttle log — classic host-physics side channel; CPUID.6:EAX[0] and CPUID.1:EDX[22] hidden, so #GP is architectural | msr-index.h:970; SDM Vol.3B 15.8.2; SDM Vol.4 Table 2-2 |
| MSR_THERM2_CTL | 0x19d | deny-gp | deny-gp | §7 Power/frequency: TM2 thermal-monitor control; matched by the MSR_THERM2_* prefix added to the class rule (the §7 example rule MSR_IA32_THERM_* alone would miss it) | msr-index.h:975; SDM Vol.4 model-specific MSR table |
| MSR_IA32_TEMPERATURE_TARGET | 0x1a2 | deny-gp | deny-gp | §7 Power/frequency: Tj target — host-identifying thermal constant; exact-name addition since the prefix rules miss it though it sits inside the thermal block | msr-index.h:981; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT | 0x1ad | deny-gp | deny-gp | §7 Power/frequency: per-core-count max turbo ratios — host-identifying and frequency-policy state; frozen model advertises no turbo (PLATFORM_INFO turbo fields zeroed) | msr-index.h:255; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT1 | 0x1ae | deny-gp | deny-gp | §7 Power/frequency: turbo ratio limits, higher core counts — same as 0x1ad | msr-index.h:256; SDM Vol.4 model-specific MSR table |
| MSR_TURBO_RATIO_LIMIT2 | 0x1af | deny-gp | deny-gp | §7 Power/frequency: turbo ratio limits, highest core counts — same as 0x1ad | msr-index.h:257; SDM Vol.4 model-specific MSR table |
| MSR_IA32_ENERGY_PERF_BIAS | 0x1b0 | deny-gp | deny-gp | §7 Power/frequency: EPB energy-vs-performance hint — guest write would change real host frequency/energy policy; CPUID.6:ECX[3] hidden so Linux never touches it; exact-name addition (sits in the thermal index range 0x19a–0x1b2) | msr-index.h:986; SDM Vol.3B 15.4.4; SDM Vol.4 Table 2-2 |
| MSR_IA32_PACKAGE_THERM_STATUS | 0x1b1 | deny-gp | deny-gp | §7 Power/frequency: package-level real temperature/throttle status — host-physics side channel | msr-index.h:994; SDM Vol.3B 15.8.4 |
| MSR_IA32_PACKAGE_THERM_INTERRUPT | 0x1b2 | deny-gp | deny-gp | §7 Power/frequency: package thermal interrupt thresholds — ties guest-visible interrupts to real die temperature | msr-index.h:1000; SDM Vol.3B 15.8.4 |
| MSR_IA32_POWER_CTL | 0x1fc | deny-gp | deny-gp | §7 Power/frequency: C1E-promotion / energy-efficiency enable bits — host idle/energy policy; in reference set via emulated_msrs_all; guest intel_idle never loads (MWAIT hidden), so nothing in the pinned guest reads it unchecked | x86.c:435 (emulated_msrs_all); msr-index.h:265; SDM Vol.4 model-specific MSR table |
| MSR_PKG_C3_RESIDENCY / MSR_PKG_C6_RESIDENCY | 0x3f8–0x3f9 | deny-gp | deny-gp | §7 Power/frequency: package C-state residency counters — real wall-clock idle time, directly breaking V-time | msr-index.h:437–438; turbostat.c; SDM Vol.4 model-specific MSR table |
| MSR_PKG_C7_RESIDENCY (= MSR_ATOM_PKG_C6_RESIDENCY) | 0x3fa | deny-gp | deny-gp | §7 Power/frequency: package C7 residency (Atom: alternate pkg-C6) — same real-idle-time leak; range extension so the 0x3f8–0x3fa block is contiguous | msr-index.h:439–440; turbostat.c |
| MSR_CORE_C3_RESIDENCY / MSR_CORE_C6_RESIDENCY / MSR_CORE_C7_RESIDENCY | 0x3fc–0x3fe | deny-gp | deny-gp | §7 Power/frequency: core C-state residency counters — directly reveal real idle time spent, breaking V-time | msr-index.h:441–443; turbostat.c; SDM Vol.4 model-specific MSR table |
| MSR_KNL_CORE_C6_RESIDENCY | 0x3ff | deny-gp | deny-gp | §7 Power/frequency: KNL variant of core C6 residency — same real-idle-time leak; range extension making 0x3fc–0x3ff contiguous | msr-index.h:444; turbostat.c |
| MSR_RAPL_POWER_UNIT | 0x606 | deny-gp | deny-gp | §7 Power/frequency: scaling units for all RAPL energy/power/time readouts — denied with the whole RAPL family (its presence invites energy probing) | msr-index.h:461; SDM Vol.3B 15.10.1; qemu.org/docs/master/specs/rapl-msr.html |
| MSR_PKG_C2_RESIDENCY | 0x60d | deny-gp | deny-gp | §7 Power/frequency: package C2 residency counter — real idle-time leak | msr-index.h:445; turbostat.c; arch/x86/events/intel/cstate.c |
| MSR_PKG_POWER_LIMIT | 0x610 | deny-gp | deny-gp | §7 Power/frequency: RAPL package power-limit/clamp config — observable (and guest-writable) real host power policy | msr-index.h:463; SDM Vol.3B 15.10.3; qemu.org rapl-msr spec |
| MSR_PKG_ENERGY_STATUS | 0x611 | deny-gp | deny-gp | §7 Power/frequency: package consumed-energy counter (~61 µJ units, ~1 ms update) — monotonically reveals real work/time done; proven physical side channel (Platypus) | msr-index.h:464; SDM Vol.3B 15.10.3; platypusattack.com; qemu.org rapl-msr spec; web.eece.maine.edu rapl-read.c |
| MSR_DRAM_ENERGY_STATUS | 0x619 | deny-gp | deny-gp | §7 Power/frequency: DRAM-domain energy counter — same energy side channel as PKG | msr-index.h:469; SDM Vol.3B 15.10.5; libmsr msr_rapl.c |
| MSR_PKG_C8_RESIDENCY / MSR_PKG_C9_RESIDENCY / MSR_PKG_C10_RESIDENCY | 0x630–0x632 | deny-gp | deny-gp | §7 Power/frequency: deep package C-state residency counters — real-time idle accounting | msr-index.h:446–448; arch/x86/events/intel/cstate.c; turbostat.c |
| MSR_PP0_ENERGY_STATUS | 0x639 | deny-gp | deny-gp | §7 Power/frequency: core/PP0-domain energy counter — same energy side channel as PKG | msr-index.h:474; SDM Vol.3B 15.10.4; libmsr msr_rapl.c |
| MSR_PP1_ENERGY_STATUS | 0x641 | deny-gp | deny-gp | §7 Power/frequency: PP1 (uncore/graphics) energy counter — energy side channel | msr-index.h:479; libmsr msr_rapl.c |
| MSR_PLATFORM_ENERGY_STATUS (PSYS) | 0x64d | deny-gp | deny-gp | §7 Power/frequency: whole-platform energy counter — energy side channel | msr-index.h:493; web.eece.maine.edu rapl-read.c |
| MSR_PPERF | 0x64e | deny-gp | deny-gp | §7 Power/frequency: productive-performance cycle counter (MPERF-adjacent) — reveals real productive time vs stalls | msr-index.h:536; SDM Vol.3B ch.15 (HWP); turbostat.c |
| MSR_PERF_LIMIT_REASONS | 0x64f | deny-gp | deny-gp | §7 Power/frequency: bitmap of why frequency was throttled (thermal/power/PROCHOT) — real-time host state; note: 0x64f is MSR_PERF_LIMIT_REASONS at this tag, not MSR_CORE_PERF_LIMIT_REASONS (that is 0x690) | msr-index.h:537; SDM Vol.4 model-specific MSR table |
| MSR_SECONDARY_TURBO_RATIO_LIMIT | 0x650 | deny-gp | deny-gp | §7 Power/frequency: secondary (e.g. E-core) turbo ratio table — host-identifying frequency policy; matched by the *TURBO_RATIO_LIMIT* substring rule | msr-index.h:494; SDM Vol.4 model-specific MSR table |
| MSR_CORE_PERF_LIMIT_REASONS | 0x690 | deny-gp | deny-gp | §7 Power/frequency: per-core frequency-limit-reason status — real-time throttle state; placed here, not pmu (the pmu rule is restricted to MSR_CORE_PERF_FIXED_*/MSR_CORE_PERF_GLOBAL_*) | msr-index.h:512; SDM Vol.4 model-specific MSR table |
| MSR_PM_ENABLE | 0x770 | deny-gp | deny-gp | §7 Power/frequency: IA32_PM_ENABLE gates the whole HWP range — HWP never exists for the guest (CPUID.6:EAX[7] hidden), so #GP is architectural | msr-index.h:538; SDM Vol.3B 15.4.2; SDM Vol.4 Table 2-2 |
| MSR_HWP_CAPABILITIES | 0x771 | deny-gp | deny-gp | §7 Power/frequency: HWP highest/guaranteed/efficient performance levels — host-identifying frequency capabilities | msr-index.h:539; SDM Vol.3B 15.4.3 |
| MSR_HWP_REQUEST_PKG | 0x772 | deny-gp | deny-gp | §7 Power/frequency: package-wide HWP request — guest write would steer real host frequency | msr-index.h:540; SDM Vol.3B 15.4.4 |
| MSR_HWP_INTERRUPT | 0x773 | deny-gp | deny-gp | §7 Power/frequency: HWP notification enables keyed to real frequency excursions | msr-index.h:541; SDM Vol.3B 15.4.6 |
| MSR_HWP_REQUEST | 0x774 | deny-gp | deny-gp | §7 Power/frequency: per-logical-CPU HWP min/max/desired/EPP request — guest write would steer real host frequency | msr-index.h:542; SDM Vol.3B 15.4.4; SDM Vol.4 Table 2-2 |
| MSR_HWP_STATUS | 0x777 | deny-gp | deny-gp | §7 Power/frequency: HWP excursion status — real frequency-delivery events; note the gap: 0x775/0x776 (IA32_PECI_HWP_REQUEST_INFO etc.) have no msr-index.h define at this tag and fall to the default-deny catch-all | msr-index.h:543; SDM Vol.3B 15.4.5 |
| IA32_HW_FEEDBACK_PTR / IA32_HW_FEEDBACK_CONFIG / IA32_THREAD_FEEDBACK_CHAR / IA32_HW_FEEDBACK_CHAR | 0x17d0–0x17d4 | deny-gp | deny-gp | §7 Power/frequency: Hardware Feedback Interface / Thread Director — per-package perf/efficiency table pointer+config and per-thread class feedback driven by real thermals and scheduling; whole block denied; only 0x17d0/0x17d1 have msr-index.h defines at this tag, 0x17d2–0x17d4 are SDM-architectural and also caught by the default-deny catch-all; CPUID.6:EAX[19]/EAX[23] hidden | msr-index.h:1265–1266; docs.kernel.org/arch/x86/intel-hfi; SDM Vol.3B 14.9 |
| IA32_HRESET_ENABLE | 0x17da | deny-gp | deny-gp | §7 Power/frequency: enables HRESET history-reset of uarch predictor/Thread-Director state — guest control over real uarch state; CPUID.7.1:EAX[22] hidden so #GP is architectural; no msr-index.h define at this tag — covered by the catch-all, row kept to make the Thread-Director surface visibly closed | Intel ISE ref. 843860; qemu-devel HRESET RFC; SDM Vol.3B 14.9 |

[question] Adjacent non-matches left to the default-deny catch-all (denied-and-logged
either way; folding them into this class's match rule would only change
mechanical-checkability bookkeeping): MSR_TURBO_ACTIVATION_RATIO (0x64c, msr-index.h:491),
MSR_ATOM_CORE_TURBO_RATIOS/MSR_ATOM_CORE_TURBO_VIDS (0x66c/0x66d, msr-index.h:509–510)
and MSR_ATOM_CORE_RATIOS/VIDS (0x66a/0x66b), the RAPL config/limit/info/policy registers
(MSR_VR_CURRENT_CONFIG 0x601, MSR_PKG_PERF_STATUS 0x613, MSR_PKG_POWER_INFO 0x614,
MSR_DRAM_POWER_LIMIT/PERF_STATUS/POWER_INFO 0x618/0x61b/0x61c,
MSR_PP0_POWER_LIMIT/POLICY/PERF_STATUS 0x638/0x63a/0x63b, MSR_PP1_POWER_LIMIT/POLICY
0x640/0x642), the IRTL latency registers (0x60a–0x60c, 0x633–0x635), the C0-residency
and demotion family (0x658–0x65b, 0x660, 0x664, 0x668/0x669), and
MSR_GFX/RING_PERF_LIMIT_REASONS (0x6b0/0x6b1). Decide at merge whether to promote them to
explicit deny-gp rows in this class or leave them to the catch-all.

[question] IA32_HRESET_ENABLE (0x17da) and IA32_THREAD_FEEDBACK_CHAR/IA32_HW_FEEDBACK_CHAR
(0x17d2–0x17d3) have no `arch/x86/include/asm/msr-index.h` define at the pinned v6.18.35
tag, so — like MSR_CORE_THREAD_COUNT in the boot-baseline fragment — they fall outside the
strictly mechanical reference-set definition of task 06 §3. The rows are kept with
safe deny-gp/deny-gp dispositions; confirm at merge whether they stay as explicit rows
(recommended: they close the Thread-Director surface visibly) or are dropped to the
default-deny catch-all.

[question] MSR_PLATFORM_INFO (0xce) is matched by this class's exact-name rule but its
normative row (allow-fixed frozen-ratio read / deny-gp write, with the frozen constant
tied to the CPUID 0x15/0x16 model) is owned by the boot-baseline fragment; it is
duplicated here with identical dispositions. At merge, keep exactly one normative copy and
make the other a cross-reference so a value change cannot diverge.
