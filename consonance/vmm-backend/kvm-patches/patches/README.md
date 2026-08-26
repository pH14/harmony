# Kernel patch series — `git format-patch` against the `linux-6.18.35` tag

Apply to a fresh checkout of the pinned tag with `git am`:

```sh
git clone --depth 1 --branch v6.18.35 \
  https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git linux-6.18.35
cd linux-6.18.35
git am /path/to/consonance/vmm-backend/kvm-patches/patches/0001-*.patch ...
```

See `../BUILD.md` for the full apply → build → load → revert recipe.

The series is two layers, all opt-in per VM via
`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` (default-off → stock behavior):

**Phase-0, the value intercepts (0001-0003).** Enable the three VMX exiting
controls (RDTSC/RDTSCP via PROCBASED bit 12; RDRAND via PROCBASED2 bit 11; RDSEED
via PROCBASED2 bit 16) and route each VM-exit to userspace as
`KVM_EXIT_DETERMINISM` (41), with a completion path that writes the destination
register(s) and advances RIP.

**Phase-1 + Phase-2, deterministic preemption + single-step (0004-0005).** The
full-determinism timing control that the Postgres-on-k3s frontier (tasks 49/56)
was proven on:

- `0004` — in-kernel force-exit preemption. A retired-branch perf overflow PMI
  (NMI) VM-exits; if the one-shot `KVM_ARM_PREEMPT_EXIT` (`_IO(KVMIO, 0xe4)`) is
  armed, `handle_exception_nmi()` returns to userspace with `KVM_EXIT_PREEMPT`
  (42) instead of re-entering, so the V-time deadline is hit with only the
  bounded hardware-PMI skid. Per-vCPU one-shot `vcpu->arch.preempt_armed`.
  **Disarm asymmetry with 0005 (note for the userspace backend).** 0004's arm is
  cleared **only** when the NMI fires it — there is no clear-on-own-exit and no
  disarm ioctl (contrast 0005 below, which `vmx_handle_exit` disarms on any
  non-MTF exit). So an arm set for a `run_until` free-run can outlive an early
  guest exit (a PIO/MMIO exit before the overflow) and later surface as a stale
  `KVM_EXIT_PREEMPT` on a plain `run()` that takes any host NMI. The kernel has
  already cleared the flag by then and neither guest state nor the work counter is
  touched, so the userspace backend swallows such a stale exit as a transparent
  re-entry (`decode_exit`, `src/kvm.rs`) rather than treating it as unhandled.
- `0005` — MTF (Monitor-Trap-Flag) deterministic single-step. `KVM_ARM_MTF_STEP`
  (`_IO(KVMIO, 0xe5)`) arms a one-shot MTF in `vmx_vcpu_pre_run`; the resulting
  monitor-trap VM-exit returns `KVM_EXIT_DET_STEP` (43). Unlike a TF/IA32_FMASK
  single-step it fires *through* guest syscall/exception/interrupt delivery (the
  issue #34 Phase-2 overshoot root cause). Per-vCPU one-shot
  `vcpu->arch.mtf_step_armed`. The arm is a **strict one-shot**: if the stepped
  instruction itself exits to userspace (MMIO/PIO/MSR/HLT/`KVM_EXIT_DETERMINISM`)
  instead of taking the MTF exit, `vmx_handle_exit` disarms it (clears the bool +
  the exec-control) on that non-MTF exit, so no stale `KVM_EXIT_DET_STEP` can reach
  the next entry and no hidden MTF state survives a snapshot boundary. In-kernel-
  handled exits re-enter with the MTF still armed, so stepping through a demand-
  paged fault still lands its `DET_STEP`.

**AMD (0006-0008).** The vendor-neutral patches (0001, 0002, 0004) are shared; the
VMX halves above have SVM counterparts. Apply these after 0001-0005.

- `0006` — the SVM analogue of 0004's force-exit, plus the capability
  advertisement in `svm_hardware_setup()` without which `KVM_ENABLE_CAP` is
  refused on an AMD host and the force-exit can never arm.
- `0007` — the opt-in becomes a per-class mask. It was a single bit covering the
  time-stamp instructions, the randomness instructions and the preemption exit
  together, so an AMD host advertising it promised entropy coverage the hardware
  cannot provide. `KVM_CHECK_EXTENSION` returns the classes the vendor supports
  and `KVM_ENABLE_CAP` refuses a request for anything outside that set. This changes what
  an existing caller gets: `args[0] = 1` used to mean "on" and now names the time-stamp
  class alone, so a caller written against the single-bit form enables less than it asks
  for and finds out at the first ioctl the missing class gates, not at the enable.
- `0008` — SVM sets `INTERCEPT_RDTSC` and `INTERCEPT_RDTSCP` for a VM that asked
  for the time-stamp class and routes both exits to the shared helpers from 0002.
  RDTSCP keeps its stock meaning when the VM has not opted in, where the trap
  exists to inject `#UD` into a guest that was not given the feature.
- `0009` — a guest's own host/guest counting filter is applied to the guest it
  runs. Bits 40 and 41 of the event select (GuestOnly, HostOnly) were stripped as
  reserved, so a guest asking to count only its own guest counted its whole self
  instead. Hardware cannot make the distinction, because a guest and any guest it
  runs in turn are inside the same `VMRUN`; KVM makes it by stopping and starting
  the backing event at the nested transitions. Needed to run this backend inside a
  virtual machine, where the work clock is exactly that measurement.

RDRAND and RDSEED have no SVM counterpart. The intercept vector in
`arch/x86/include/asm/svm.h` carries no control for either instruction, so a guest
executes them against the hardware whatever its CPUID model says. Guest randomness
has to be denied above the hypervisor on this vendor.

- `0010` — the single-step gets its own class, `KVM_DETERMINISTIC_INTERCEPT_STEP`.
  `0007` had gated it on the preemption class, which SVM advertises, so
  `KVM_ARM_MTF_STEP` succeeded on an AMD host and set a flag only `vmx.c` reads:
  the step never happened and `KVM_RUN` resumed the guest instead of advancing it
  one instruction. VMX advertises the new class where the hardware has a monitor
  trap flag; SVM never does.

There is no SVM counterpart to 0005. SVM has no monitor trap flag, so single-stepping
there goes through stock `KVM_GUESTDBG_SINGLESTEP` on `RFLAGS.TF`. TF is guest state,
which costs three things a hypervisor stepping a whole guest cares about: any interrupt
or exception delivered through a gate clears it in the new flags and `SYSCALL` masks it
through `IA32_FMASK`, so stepping stops at the guest's own kernel entry; the resulting
`#DB` shares a channel with the guest's own debugging; and the `MOV SS` / `POP SS`
shadow defers it, so one step covers two instructions. MTF has none of these — it lives
in the VMCS, produces its own exit, and the guest cannot see or clear it.

- `0001-KVM-x86-add-KVM_EXIT_DETERMINISM-userspace-exit-ABI.patch`
- `0002-KVM-x86-emulate-intercepted-RDTSC-RDTSCP-RDRAND-RDSE.patch`
- `0003-KVM-VMX-enable-RDTSC-RDRAND-RDSEED-exiting-for-the-d.patch`
- `0004-KVM-x86-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-pr.patch`
- `0005-KVM-VMX-MTF-based-deterministic-single-step.patch`
- `0006-AMD-SVM-KVM_EXIT_PREEMPT-analogue-and-cap-advertisem.patch`
- `0007-KVM-x86-make-the-deterministic-intercepts-opt-in-a-p.patch`
- `0008-KVM-SVM-trap-RDTSC-and-RDTSCP-for-the-deterministic-.patch`
- `0009-KVM-SVM-apply-a-guest-s-host-guest-counting-filter-t.patch`
- `0010-KVM-x86-give-the-deterministic-single-step-its-own-c.patch`

Verified: the 0001-0005 series is `git am`-clean on a fresh `linux-6.18.35`
checkout, reproduces the built tree byte-for-byte, and the out-of-tree modules
build cleanly (vermagic `6.18.35-…`). Per-file sha256 are pinned in
`harmony-linux/linux/versions.lock` (`KVM_PATCH_000x_SHA256`). `scripts/apply_patch.py`
reproduces the 0001-0003 edits by string anchor; `scripts/apply_patch_612.py`
ports them to the Debian 6.12.90 source for the loadable proxy build
(`../BUILD.md` Part 2).

The 0006-0009 half was applied on top of that series, built, and booted on an EPYC
7313P; every measurement under `qualification-evidence/nested/` and
`qualification-evidence/rdtsc/` was taken against those modules. It has not been
through the byte-for-byte reproduction check the first five have, and there is no
`apply_patch.py` anchor form for it.

`0010` is `git am`-clean on top of 0001-0009 on a fresh `linux-6.18.35` checkout and
nothing further. It was written after the qualification box was released, so it has
not been compiled or booted, and no measurement here was taken against it.

