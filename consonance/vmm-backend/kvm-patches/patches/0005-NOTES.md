# Patch 0005 — MTF-based deterministic single-step (provenance)

Captured on the determinism box (task 56, STEP 0) from the live deb612 build tree:

- Built/loaded module source: `/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm`
  (built `kvm.ko` / `kvm-intel.ko`, loaded by `/root/run-patched-ht49.sh`).
- Headers (uapi + asm): `.../linux-headers-6.12.90+deb13.1-common/`.
- Diffed against the in-place-patched stock tree `/root/kvm-spike/deb612/linux-6.12.90`,
  which already has patches 0001-0004 applied. So `0005-...patch` is the **0005-only delta**.

## What it does
Adds a one-shot VMX Monitor-Trap-Flag single-step that fires THROUGH guest
syscall/exception/interrupt delivery (which TF/IA32_FMASK could not — root cause of issue #34
Phase-2 overshoot):
- `kvm_vcpu_arch.mtf_step_armed` (one-shot flag).
- `KVM_ARM_MTF_STEP` vcpu ioctl `_IO(KVMIO, 0xe5)` — arms MTF when `deterministic_intercepts`.
- On VM-entry, if armed, set `CPU_BASED_MONITOR_TRAP_FLAG`.
- `handle_monitor_trap`: if armed, clear the flag, return `KVM_EXIT_DET_STEP` (43) to userspace.
Userspace (`consonance/vmm-backend/src/kvm_sys.rs`) arms it each step in `single_step_once`.

## Dependencies / caveats
- Depends on patches 0001-0004 (esp. 0004: `deterministic_intercepts`, `preempt_armed`,
  `KVM_EXIT_PREEMPT`) already present in the deb612 tree. 0004 itself is parked (PR #33 / issue #34)
  and is NOT yet a tracked patch file here.
- Productionizing (clean series + canonical linux-6.18 port + gates) is a separate later task
  (task-55-class); this file only preserves the box-proven 0005 delta.
