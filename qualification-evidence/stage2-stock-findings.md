# Stage-2 measurements on the stock kernel

All four ran on the qualification box on the stock Debian 6.12.95 kernel, pinned to
core 3, with the determinism posture applied. Raw records are in
`qualification-evidence/box/stage2/`.

## Single-step by RFLAGS.TF (`singlestep-tf.json`)

Instruction granularity, `KVM_GUESTDBG_SINGLESTEP`. Exact on four of the five
boundary classes: `nop_sled` 16 debug exits against 16 instructions, `loop` 33
against 33, `jmp_chain` 16 against 16, `sti_shadow` 3 against 3.

`movss_shadow` delivered 2 debug exits where the payload retires 3 instructions.
The shortfall is exactly one, and it is x86 architecture rather than a property of
this chip: a `MOV SS` (and `POP SS`) suppresses the debug exception that would
otherwise be raised after the following instruction, so a stepper counting `#DB`
exits skips one instruction per `MOV SS` shadow. Any landing built on TF stepping
must therefore either avoid `MOV SS` in the payload or account for the shadow. The
AE-3 loop payload contains none, so its landing contract is unaffected.

## Single-step by DebugCtl.BTF (`singlestep-btf.json`)

Taken-branch granularity, the granularity that would match the work clock directly.
It delivered zero debug exits on every one of the five payloads, including
`jmp_chain` whose 16 taken branches should each have trapped. `guest_tf_kept=1`
attests only that the guest's `RFLAGS.TF` survived the round trip; the harness does
not read `MSR_DEBUGCTL` back, so it cannot separate "the silicon did not raise the
branch trap" from "KVM never loaded `DebugCtl.BTF` into the VMCB". Measured result:
branch-trap stepping is unavailable in this configuration. Which layer drops it is a
named gap, not a claim.

## CPUID freeze below the host (`ae4-cpuid-freeze.json`)

Passed. The guest vendor string is frozen to `AuthenticAMD`, and leaf 1 EDX bit 4,
which the host has set (`0x078bfbff`), is cleared in the guest's view
(`0x078bfbef`). Presenting a feature set strictly below the host's is enforced.

## MSR default-deny (`ae4-msr-deny.json`)

Passed. `KVM_CAP_X86_USER_SPACE_MSR` is available, a filter installed, and a guest
`RDMSR` of `0xc0010015` trapped out to the VMM rather than being serviced by KVM.
The `#GP`-then-shutdown variant was not the path taken and its field is zero.

## AE-3 on the stock kernel (`stock-half.log`)

Refused, as it must: `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` is absent from the stock
module, `ENABLE_CAP` returned `EINVAL`, and the harness exited 3 without running a
single arm. The harness is structurally unable to report a pass on the stock
mechanism.
