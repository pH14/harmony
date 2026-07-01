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

- `0001-KVM-x86-add-KVM_EXIT_DETERMINISM-userspace-exit-ABI.patch`
- `0002-KVM-x86-emulate-intercepted-RDTSC-RDTSCP-RDRAND-RDSE.patch`
- `0003-KVM-VMX-enable-RDTSC-RDRAND-RDSEED-exiting-for-the-d.patch`
- `0004-KVM-x86-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-pr.patch`
- `0005-KVM-VMX-MTF-based-deterministic-single-step.patch`

Verified: the five-patch series is `git am`-clean on a fresh `linux-6.18.35`
checkout, reproduces the built tree byte-for-byte, and the out-of-tree modules
build cleanly (vermagic `6.18.35-…`). Per-file sha256 are pinned in
`guest/linux/versions.lock` (`KVM_PATCH_000x_SHA256`). `scripts/apply_patch.py`
reproduces the 0001-0003 edits by string anchor; `scripts/apply_patch_612.py`
ports them to the Debian 6.12.90 source for the loadable proxy build
(`../BUILD.md` Part 2).
