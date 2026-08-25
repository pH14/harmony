# HV_X64_MSR_TIME_REF_COUNT: read-only synthetic state, not a restore failure

The stage-1 fixpoint writes a vCPU's saved state back and saves again. A displacement
probe — write a value 2^32 ticks past the one the register holds, read it back — found
that `HV_X64_MSR_TIME_REF_COUNT` (`0x4000_0020`) accepts the write and discards it:

    written 0x100002b51, which is 4294967296 past the 0x2b51 it held, read back 0x2b67

The question that decides whether this is a defect is whether the kernel intends the
write to take effect.

## The kernel this was measured against

The box runs `6.12.95+deb13-amd64`. `arch/x86/kvm/hyperv.c` at tag `v6.12.95` is kept
beside this file as `hyperv-6.12.95.c`, fetched from
`git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git`.

`kvm_hv_set_msr_pw` (line 1375), case at line 1482:

    case HV_X64_MSR_TIME_REF_COUNT:
        /* read-only, but still ignore it if host-initiated */
        if (!host)
            return 1;
        break;

A guest write returns 1, which the caller turns into a fault. A host-initiated write —
which is what `KVM_SET_MSRS` from userspace is — falls through and does nothing. The
read path, line 1647, is `data = get_time_ref_counter(kvm);`: a computed value with no
stored field behind it, so there is nothing a write could set.

The comment states the intent in as many words. The register is read-only, and the
write is discarded on purpose so that a host restoring vCPU state does not fail on it.

## The register that behaved differently, and why

`HV_X64_MSR_VP_RUNTIME` (`0x4000_0010`) took the displaced write in the same probe.
`kvm_hv_set_msr` (line 1518), case at line 1586:

    case HV_X64_MSR_VP_RUNTIME:
        if (!host)
            return 1;
        hv_vcpu->runtime_offset = data - current_task_runtime_100ns();
        break;

A host write there does take effect, by setting an offset, and the read at line 1708
returns `current_task_runtime_100ns() + hv_vcpu->runtime_offset`. The measurement
matches both paths exactly: the register with a stored offset restores, the computed
one does not.

## Classification

`HV_X64_MSR_TIME_REF_COUNT` is read-only synthetic hypervisor state. It is excluded
from the fixpoint's must-restore set, named in every fixpoint record with this reason,
and it is not a finding about the silicon or about KVM. It gets the same treatment as
`MSR_KVM_ASYNC_PF_INT` (`0x4b56_4d06`), which this vCPU does not own at all because
KVM gates it on an in-kernel local APIC the measurement window does not create.

The exclusion is declared, not inferred. `consonance/cpu-qualification/src/guest.rs`
carries `READ_ONLY_MSRS`, one entry per register with the citation above, and the probe
checks the declaration both ways: a register named there that *does* take a write fails
the run because the declaration is wrong, and a register not named there that ignores
one fails the run because a restore failed. Neither direction is decided by the run's
own convenience.

## Where the classification changed the verdict

Campaign D, run before this classification, passed 35 of 36 checks and failed only
`fixpoint[0]`, on this register. Campaign E is the same measurement with the register
classified from the kernel source.
