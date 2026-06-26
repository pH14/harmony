> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

### MSR class: timing-instr (user-wait timing instructions)

This class covers the MSR control surface of the WAITPKG user-wait instructions
(UMWAIT/TPAUSE/UMONITOR), whose only MSR is `IA32_UMWAIT_CONTROL` (0xE1): it caps the
maximum wait of UMWAIT/TPAUSE in TSC quanta and gates the C0.2 sleep state, i.e. it
configures instructions that block until a deadline measured against the *real* TSC —
a direct real-time dependence the V-time design cannot tolerate. Match rule: every name
matching `MSR_IA32_UMWAIT_CONTROL*` in `arch/x86/include/asm/msr-index.h` at v6.18.35
(one MSR; the remaining `MSR_IA32_UMWAIT_CONTROL_*` defines are bit-field masks of it).
The frozen CPUID model hides WAITPKG (CPUID.7,0:ECX[5] = 0) per RESEARCH.md principle 5
("no waitpkg — control via CPUID filtering") and the contract's instruction table makes
UMWAIT/TPAUSE/UMONITOR #UD, so the MSR is denied in both directions; #GP on access is
also the architecturally mandated behavior when WAITPKG is not enumerated, so a correct
guest never touches it and any access is a loud, logged filter exit.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_UMWAIT_CONTROL | 0xE1 | deny-gp | deny-gp | Closes §7 TSC-plumbing vector: UMWAIT/TPAUSE wait on real-TSC deadlines bounded by this MSR; WAITPKG hidden in the frozen CPUID model, and #GP matches architectural behavior with CPUID.7,0:ECX[5]=0 | Intel SDM Vol. 4 Table 2-2 (IA32_UMWAIT_CONTROL, E1H) and Vol. 2 UMWAIT/TPAUSE; Linux v6.18.35 `arch/x86/kvm/x86.c` `msrs_to_save_base` (WAITPKG-gated) and `arch/x86/include/asm/msr-index.h:102`; lwn.net/Articles/791668; RESEARCH.md §principle 5 |
