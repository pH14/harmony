#!/usr/bin/env python3
"""Apply the nested guest/host counting filter to the AMD vPMU by string anchor."""
import pathlib
import sys

ROOT = pathlib.Path("/root/kbuild-618/linux-6.18.35")

EDITS = []


def edit(relpath, old, new):
    EDITS.append((relpath, old, new))


# ---------------------------------------------------------------- svm/pmu.c
edit(
    "arch/x86/kvm/svm/pmu.c",
    "\tpmu->reserved_bits = 0xfffffff000280000ull;\n",
    "\t/*\n"
    "\t * Bits 40 and 41 (GuestOnly, HostOnly) stay writable so that a guest\n"
    "\t * hypervisor's own counting filter survives the trap into KVM. They\n"
    "\t * never reach hardware: the config handed to perf is masked with\n"
    "\t * raw_event_mask, which excludes them. See pmc_counts_in_current_mode().\n"
    "\t */\n"
    "\tpmu->reserved_bits = 0xfffffcf000280000ull;\n",
)

# ------------------------------------------------------------------- pmu.c
edit(
    "arch/x86/kvm/pmu.c",
    '#include "x86.h"\n#include "cpuid.h"\n',
    '#include "x86.h"\n#include "cpuid.h"\n#include "kvm_cache_regs.h"\n',
)

edit(
    "arch/x86/kvm/pmu.c",
    "static int pmc_reprogram_counter(struct kvm_pmc *pmc, u32 type, u64 config,\n",
    """/*
 * Whether the guest asked for a counter that runs in one mode only. Setting
 * neither bit or both means count always, which is the rule hardware itself
 * applies to the pair.
 */
static bool pmc_has_mode_filter(struct kvm_pmc *pmc)
{
	return !!(pmc->eventsel & AMD64_EVENTSEL_GUESTONLY) !=
	       !!(pmc->eventsel & AMD64_EVENTSEL_HOSTONLY);
}

/*
 * Whether a counter should be running right now.
 *
 * Hardware applies GuestOnly and HostOnly relative to VMRUN. A guest and any
 * guest it runs in turn are both inside the same VMRUN, so hardware cannot tell
 * them apart and a guest asking to count only its own guest would otherwise
 * count itself as well. KVM applies the filter for it, by starting and stopping
 * the backing event at the nested transitions.
 */
static bool pmc_counts_in_current_mode(struct kvm_pmc *pmc)
{
	if (!pmc_has_mode_filter(pmc))
		return true;

	return !!(pmc->eventsel & AMD64_EVENTSEL_GUESTONLY) ==
	       is_guest_mode(pmc->vcpu);
}

static int pmc_reprogram_counter(struct kvm_pmc *pmc, u32 type, u64 config,
""",
)

edit(
    "arch/x86/kvm/pmu.c",
    "\tattr.sample_period = get_sample_period(pmc, pmc->counter);\n",
    "\tattr.sample_period = get_sample_period(pmc, pmc->counter);\n"
    "\tattr.disabled = !pmc_counts_in_current_mode(pmc);\n",
)

edit(
    "arch/x86/kvm/pmu.c",
    "\t/* reuse perf_event to serve as pmc_reprogram_counter() does*/\n"
    "\tperf_event_enable(pmc->perf_event);\n",
    "\t/* reuse perf_event to serve as pmc_reprogram_counter() does*/\n"
    "\tif (pmc_counts_in_current_mode(pmc))\n"
    "\t\tperf_event_enable(pmc->perf_event);\n"
    "\telse\n"
    "\t\tperf_event_disable(pmc->perf_event);\n",
)

edit(
    "arch/x86/kvm/pmu.c",
    "void kvm_pmu_handle_event(struct kvm_vcpu *vcpu)\n",
    """/*
 * Re-evaluate the mode-filtered counters after the guest entered or left a guest
 * of its own. The work is deferred to the reprogram pass, which runs before the
 * next entry and in a context where stopping and starting an event is allowed.
 */
void kvm_pmu_nested_transition(struct kvm_vcpu *vcpu)
{
	struct kvm_pmu *pmu = vcpu_to_pmu(vcpu);
	struct kvm_pmc *pmc;
	unsigned int i;

	kvm_for_each_pmc(pmu, pmc, i, pmu->all_valid_pmc_idx)
		if (pmc_has_mode_filter(pmc))
			kvm_pmu_request_counter_reprogram(pmc);
}

void kvm_pmu_handle_event(struct kvm_vcpu *vcpu)
""",
)

# ------------------------------------------------------------------- pmu.h
edit(
    "arch/x86/kvm/pmu.h",
    "void kvm_pmu_handle_event(struct kvm_vcpu *vcpu);\n",
    "void kvm_pmu_handle_event(struct kvm_vcpu *vcpu);\n"
    "void kvm_pmu_nested_transition(struct kvm_vcpu *vcpu);\n",
)

# ------------------------------------------------------------- svm/nested.c
edit(
    "arch/x86/kvm/svm/nested.c",
    '#include "hyperv.h"\n',
    '#include "hyperv.h"\n#include "pmu.h"\n',
)

edit(
    "arch/x86/kvm/svm/nested.c",
    "\t/* Enter Guest-Mode */\n\tenter_guest_mode(vcpu);\n",
    "\t/* Enter Guest-Mode */\n\tenter_guest_mode(vcpu);\n"
    "\tkvm_pmu_nested_transition(vcpu);\n",
)

edit(
    "arch/x86/kvm/svm/nested.c",
    "\t/* Exit Guest-Mode */\n\tleave_guest_mode(vcpu);\n",
    "\t/* Exit Guest-Mode */\n\tleave_guest_mode(vcpu);\n"
    "\tkvm_pmu_nested_transition(vcpu);\n",
)

edit(
    "arch/x86/kvm/svm/nested.c",
    "\t\tleave_guest_mode(vcpu);\n\n\t\tsvm_switch_vmcb(svm, &svm->vmcb01);\n",
    "\t\tleave_guest_mode(vcpu);\n\t\tkvm_pmu_nested_transition(vcpu);\n\n"
    "\t\tsvm_switch_vmcb(svm, &svm->vmcb01);\n",
)


def main():
    failures = []
    for relpath, old, new in EDITS:
        path = ROOT / relpath
        text = path.read_text()
        n = text.count(old)
        if n != 1:
            failures.append(f"{relpath}: anchor appears {n} times, expected 1:\n{old!r}")
            continue
        path.write_text(text.replace(old, new))
        print(f"ok  {relpath}")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    return 0


sys.exit(main())
