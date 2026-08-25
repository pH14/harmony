# The enforcement demo against the AMD draft column

`docs/cpu-msr-contract-amd-draft.toml` marks every enforcement cell
`verified = "on-silicon-pending-AE4"` and names the mechanism it presumes: the SVM VMCB
CPUID intercept and the MSR-permission bitmap. This program demonstrates that mechanism
on the silicon and on the kernel the column names, `kernel-tag = "v6.18.35"`. It does not
ratify the column's individual dispositions; that is a per-row job and is not one of this
program's items.

## What the mechanism does enforce

**The CPUID intercept enforces a frozen model, including below host capability.** Leaf 1
EDX bit 4 is set on this host, `0x078bfbff`; cleared in the model handed to the vCPU the
guest reads `0x078bfbef`, and the vendor string stays AuthenticAMD.
`box/stage2/ae4-cpuid-freeze.json`.

**The MSR-permission bitmap routes an unlisted MSR to the VMM.** With
`KVM_CAP_X86_USER_SPACE_MSR` enabled and a filter installed, a guest `rdmsr` of
`0xc0010015` leaves the guest and is delivered to the VMM rather than serviced by the
kernel. `box/stage2/ae4-msr-deny.json`.

## Where the enforcement abstraction ends

A CPUID row controls what the guest is told, not what the guest can do. SVM's intercept
vector has no RDRAND or RDSEED control, so for those two instructions the column's
CPUID row is advisory: clearing the feature bit changes the reported model and the guest
still executes the instruction and still receives hardware entropy. Measured, five runs,
`amd-rdrand-not-interceptable.md`. A row disposition that reads as "this guest does not
have RDRAND" is true of the model and false of the machine.

RDTSC is the opposite case: `INTERCEPT_RDTSC` and `INTERCEPT_RDTSCP` exist in SVM's
vector, so the chip can enforce it, and the determinism patch series does not wire them
on this vendor. `amd-determinism-kernel-gap.md`.

## The baseline placeholder

The column carries `cpuid-baseline = "det-zenN-v1"`, a placeholder the file says the
generation-discovery stage replaces with the pinned name. That generation is now pinned
by measurement: family 0x19, model 0x01, stepping 0x01, AuthenticAMD, microcode
0xa0011a9, and the pack this program sealed is `det-zen3-v1`. Replacing the literal is a
version bump and a `contract_hash` re-derivation, which the file states must never be a
silent edit, so it is named here as a follow-up rather than made here.
