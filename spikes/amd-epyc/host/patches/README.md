<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AE-3 kernel patches — the `svm.c` force-exit + the single-step 0005-analogue

`docs/AMD-EPYC.md` §3 (the 0004-analogue) and §1 (the single-step primitive without MTF).
**Untested-on-silicon drafts** until AE-3 builds and measures them on the box.

## `0004-KVM-SVM-KVM_EXIT_PREEMPT-force-exit-analogue.patch`

The AMD/SVM analogue of the Intel/VMX in-kernel force-exit
(`consonance/vmm-backend/kvm-patches/patches/0004-KVM-x86-add-KVM_EXIT_PREEMPT-...`).

**It is only the `svm.c` hunk, and that is the point.** Intel and AMD are the *same*
x86-64 `Arch` and share the whole `arch/x86` tree (`docs/ARCH-BOUNDARY.md`). Everything
0004 adds outside `vmx.c` is vendor-neutral and shared verbatim with SVM:

| shared plumbing (from x86 patches 0001/0004) | where |
|---|---|
| `KVM_EXIT_PREEMPT` = 42 (UAPI exit reason) | `include/uapi/linux/kvm.h` |
| `KVM_ARM_PREEMPT_EXIT` = `_IO(KVMIO, 0xe4)` (arm ioctl) | `include/uapi/linux/kvm.h` |
| `vcpu->arch.preempt_armed` one-shot | `arch/x86/include/asm/kvm_host.h` |
| `kvm->arch.deterministic_intercepts` opt-in | `arch/x86/include/asm/kvm_host.h` |
| `KVM_ARM_PREEMPT_EXIT` ioctl case (arms the one-shot) | `arch/x86/kvm/x86.c` |

So the AMD backend applies patches **0001, 0002, 0004** (the vendor-neutral UAPI +
intercept-emulation + force-exit plumbing) **plus this one `svm.c` hunk**. It does **not**
need 0003 (that enables *VMX* RDTSC/RDRAND/RDSEED exiting — the SVM equivalent is the
VMCB intercept bitmap, wired in AE-4, not a force-exit concern). The `svm.c` hook is the
line-for-line twin of 0004's `vmx.c handle_exception_nmi()` arm: the perf-overflow PMI is
an NMI on both vendors, `#VMEXIT`s with `SVM_EXIT_NMI`, is serviced by
`svm_handle_exit_irqoff()`, and `nmi_interception()` (the SVM twin of VMX's `is_nmi()`
branch) returns `KVM_EXIT_PREEMPT` under the arm instead of re-entering.

**AVIC:** disabled for the deterministic backend (`kvm_amd avic=0`, recorded in the
baseline). AVIC accelerates *IRQ* delivery, not NMIs, so the perf-overflow NMI reaches
this path regardless; AVIC-off is a recorded standing condition (the interrupt fabric is
kept in userspace for determinism, as on Intel), attested per AE-3 run so a silent AVIC-on
run cannot masquerade as the mechanism (evidence integrity #4).

## The single-step 0005-analogue (AE-2 decides the primitive, then it is drafted)

SVM has **no Monitor Trap Flag** (`docs/AMD-EPYC.md` §1), so patch 0005's MTF mechanism
(`KVM_EXIT_DET_STEP` via `CPU_BASED_MONITOR_TRAP_FLAG`) has no SVM equivalent. The
single-step primitive must be built from AMD debug facilities — `DebugCtl.BTF` (branch
single-step, the natural fit: its granularity **is** the retired-taken-branch V-time
event) or `RFLAGS.TF` (instruction single-step), with `#DB` reaching the VMCB `#DB`
intercept. **AE-2 characterizes both against the analytical oracle under SVM and writes
the ranked ruling**; only then is the `0005-svm-single-step` patch drafted around the
ruled primitive (via `KVM_SET_GUEST_DEBUG` / a VMCB `#DB`-intercept hook). "TF and hope"
is not a ruling — the draft waits on AE-2's on-silicon data (`harness/singlestep-driver.c`).

## Build + apply

See `host/build-6.18-kernel.sh`: fetch the pinned `linux-6.18.35`, `git am` the canonical
determinism series 0001-0005 + this `svm.c` hunk (both apply clean to 6.18.35), and build a
BOOTABLE patched kernel `.deb` (the 6.18 `KVM_EXIT_PREEMPT`/`preempt_armed` infrastructure is
absent from stock 6.8, so the module must match a booted 6.18.35 host — `host/stage-6.18-boot.sh`
installs it behind a self-recovering GRUB one-shot). The stock kernel + module hashes are
recorded first (record-then-modify) so the box restores to baseline. (`host/build-kvm-amd.sh` is
the SUPERSEDED out-of-tree-against-stock-6.8 recipe — kept only for provenance.)
