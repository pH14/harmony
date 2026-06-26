> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment — class `microcode`

The `microcode` class covers the microcode-update interface: the update trigger
IA32_BIOS_UPDT_TRIG and the signature/revision register IA32_BIOS_SIGN_ID. Match rule
against `arch/x86/include/asm/msr-index.h` @ v6.18.35: every name matching
`MSR_IA32_UCODE_*` — exactly two entries, `MSR_IA32_UCODE_WRITE` (0x79, msr-index.h:938)
and `MSR_IA32_UCODE_REV` (0x8b, msr-index.h:939); the AMD analog `MSR_AMD64_PATCH_LOADER`
is out of scope (AMD is a task-06 non-goal) and falls to the §1 default-deny filter.
Policy: the guest must never load microcode (an update would mutate CPU behavior mid-run,
host-dependently and irreversibly), and the revision it reads must be a frozen constant
from the versioned baseline — never the host's revision, which KVM's feature-MSR path
otherwise samples straight from the host CPU (`kvm_get_feature_msr` does
`rdmsrq_safe(MSR_IA32_UCODE_REV)`, x86.c:1714) and which KVM deliberately leaves mutable
(`kvm_is_immutable_feature_msr` exempts exactly this MSR, x86.c:495). vmm-core sets the
frozen value once, host-initiated, at vCPU creation and never again. The write disposition
on 0x8b is boot-critical and not negotiable: the task-04 pinned guest builds with
CONFIG_CPU_SUP_INTEL=y (default y, Kconfig.cpu:365), so `early_init_intel()` (intel.c:207)
runs the SDM signature sequence — `native_wrmsrq(MSR_IA32_UCODE_REV, 0)`, CPUID(1), RDMSR
(microcode.h:64–77) — with no exception fixup; a #GP there kills the guest in early boot,
so the write is dropped-and-logged (`deny-ignore-write`), which together with the fixed
read value reproduces the architectural readback exactly (real hardware reloads the
signature after the CPUID, SDM Vol 3A §9.11.7.1). Nothing in this class is captured in
`vm_state` per INTEGRATION.md §4: the guest can establish no state here — the fixed
revision is versioned config hashed into the determinism gate (§6), not state. Column
grammar: `Read`/`Write` are drawn verbatim from the task-06 §3 disposition vocabulary;
`Rationale` names the INTEGRATION.md §7 leak vector closed (or `architectural`); kernel
citations are `file:line` at the pinned tag v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_UCODE_REV (IA32_BIOS_SIGN_ID) | 0x8b | allow-fixed(0x0000_0001_0000_0000) | deny-ignore-write | CPUID stability: frozen revision 0x00000001 in bits 63:32 (bits 31:0 read 0), never the host revision KVM samples via rdmsrq (x86.c:1714) — KVM treats this as the one mutable feature MSR (x86.c:495), so the contract pins it; write must not #GP because early_init_intel()'s unguarded WRMSR-0/CPUID/RDMSR signature sequence runs in the pinned guest's early boot, and ignoring the write while reading back the fixed value is exactly the architectural reload semantics. | x86.c:436 (emulated_msrs_all), x86.c:472 (msr_based_features_all_except_vmx), x86.c:495 (kvm_is_immutable_feature_msr), x86.c:1714 (kvm_get_feature_msr host rdmsrq), x86.c:3958/4420 (guest write ignored / read returns microcode_version); msr-index.h:939; intel.c:207 (early_init_intel) + microcode.h:64–77 (intel_get_microcode_revision), all @ v6.18.35; Intel SDM Vol 3A §9.11.7.1 (update signature sequence); SDM Vol 4 Table 2-2 (IA32_BIOS_SIGN_ID); KVM api.rst KVM_GET_MSR_FEATURE_INDEX_LIST |
| MSR_IA32_UCODE_WRITE (IA32_BIOS_UPDT_TRIG) | 0x79 | deny-gp | deny-gp | CPUID stability + architectural: a write is a microcode-update attempt that would change CPU behavior mid-run host-dependently, so it must fail loudly — KVM's default is a silent drop (x86.c:3949), which §1 forbids; reads #GP architecturally (the MSR is write-only per SDM Vol 4) and KVM likewise rejects them (no get-side case); no pinned-guest boot path writes it — the loader self-disables on the hypervisor bit (core.c:111) or finds no update blob in the task-04 busybox initramfs. | msr-index.h:938; x86.c:3949 (kvm_set_msr_common ignored-writes group — contract diverges: loud #GP, not silent drop); core.c:111 (microcode_loader_disabled), all @ v6.18.35; Intel SDM Vol 3A §9.11.6 (microcode update loader / BIOS_UPDT_TRIG); SDM Vol 4 Table 2-2 (IA32_BIOS_UPDT_TRIG, write-only) |

## Questions

[question] MSR_IA32_UCODE_REV (0x8b): the pinned revision 0x00000001 must be ratified
jointly with the frozen CPUID family/model/stepping and the speculation class. If the
frozen model hides the hypervisor bit (CPUID.1:ECX[31]=0) and the chosen model/stepping
matches an entry in `spectre_bad_microcodes` (intel.c:106 @ v6.18.35),
`bad_spectre_microcode()` (intel.c:130) treats any revision ≤ the blacklisted one as bad
and the guest deterministically clears SPEC_CTRL/IBPB/STIBP feature bits with a boot
warning; with the hypervisor bit set the check short-circuits and 0x1 is inert. Decided
value stands (behavior is deterministic either way) — should a later revision instead pin
a value above the blacklist threshold for the chosen SKU so the speculation-class feature
bits survive guest feature detection?

[question] MSR_IA32_UCODE_WRITE (0x79): the deny-gp write disposition is boot-safe only
while no microcode blob ships in the task-04 initramfs whenever the hypervisor bit is
hidden — CONFIG_MICROCODE is `def_bool y` (arch/x86/Kconfig:1321 @ v6.18.35, cannot be
configured out while CPU_SUP_INTEL=y), and with CPUID.1:ECX[31]=0 the early loader does
not self-disable (core.c:111), so a `kernel/x86/microcode/GenuineIntel.bin` entry in the
image would reach an unguarded `native_wrmsrq(MSR_IA32_UCODE_WRITE, …)` and oops the guest
in early boot. Should the contract bind the task-04 image manifest to "no
kernel/x86/microcode/ cpio entries", or alternatively mandate hypervisor bit = 1 in the
frozen CPUID model so `microcode_loader_disabled()` short-circuits regardless of image
contents?
