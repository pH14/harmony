# The AMD half of the determinism kernel series is incomplete

The patched 6.18.35 tree on the box carries six commits on top of `v6.18.35 pristine`:

    5ec58e3c9  KVM: x86: add KVM_EXIT_DETERMINISM userspace exit ABI
    3ebd9a883  KVM: x86: emulate intercepted RDTSC/RDTSCP/RDRAND/RDSEED to userspace
    129d00282  KVM: VMX: enable RDTSC/RDRAND/RDSEED exiting for the deterministic backend
    6f32578da  KVM: x86: add KVM_EXIT_PREEMPT in-kernel force-exit preemption
    deb8388e2  KVM: VMX: MTF-based deterministic single-step (patch 0005, 6.18 port)
    52248ae11  AMD SVM KVM_EXIT_PREEMPT analogue and cap advertisement (AE-3)

The last is the AMD one. It is 23 lines in two hunks of `arch/x86/kvm/svm/svm.c`: the
force-exit in `nmi_interception()` and `kvm_caps.has_deterministic_intercepts = true`
in `svm_hardware_setup()`. Nothing else in the series touches SVM.

The consequence is that only one of the two determinism mechanisms exists on AMD.

**Present on AMD.** The in-kernel force exit. Measured working: the harness arms the
one-shot, the perf overflow arrives as an NMI, `nmi_interception()` returns to
userspace with `KVM_EXIT_PREEMPT`, and every landing this program measured attested
that exit reason.

**Absent on AMD.** The must-trap instruction intercepts. The enable lives only in
`arch/x86/kvm/vmx/vmx.c` (`SECONDARY_EXEC_RDRAND_EXITING`, `SECONDARY_EXEC_RDSEED_EXITING`
at lines 4645 and 4647, under the deterministic-intercepts gate). `svm.c` has no
matching `svm_set_intercept(svm, INTERCEPT_RDTSC)` / `INTERCEPT_RDRAND` under that
gate; its only `INTERCEPT_RDTSCP` writes are the stock TSC-scaling logic at lines
984-986. `KVM_EXIT_DETERMINISM` appears in `vmx.c` and `x86.c` and never in `svm.c`,
so the common emulate-to-userspace path added by `3ebd9a883` has nothing to trigger it
on an AMD host.

This matters for the classification sweep the suite's stage 2 specifies, which
requires must-trap entries to be "demonstrated on silicon (`RDTSC`, `RDRAND` and kin
exit and are serviced)". On this kernel that demonstration is not possible on AMD.
It is a gap in issue #174's patch series, not a property of the chip. The pack
records nothing about it because the pack has no field for it; the two fields stage 2
would fill, `count_offsets` and `single_step.work_per_step`, remain absent for the
separate reason that the suite's stage 2 is not built.

## Two thirds of this is now closed

Patches `0006` through `0008` add the SVM half: the force-exit analogue with its
capability advertisement, a per-class opt-in mask so a host can state the classes it
covers, and `INTERCEPT_RDTSC` / `INTERCEPT_RDTSCP` for a VM that asked for the
time-stamp class. Demonstrated on silicon in
`rdtsc/the-time-stamp-intercepts-on-svm.md`: a guest reads the sentinels userspace
supplies, and the control VM reads the host counter.

The randomness third stays open and always will on this part, because SVM has no
`RDRAND` or `RDSEED` intercept control to wire —
`amd-rdrand-not-interceptable.md`. The per-class mask is how a caller finds that out:
this host advertises `0x5`, and a request for the randomness class is refused with
`EINVAL` rather than quietly narrowed.

`count_offsets` and `single_step.work_per_step` remain absent, for the separate reason
that the suite's stage 2 is not built.

