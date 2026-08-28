# Kernel patch series

The series applies to Linux `v6.18.35` and is opt-in per VM through
`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`.

- `0001` defines the `KVM_EXIT_DETERMINISM` userspace ABI.
- `0002` adds completion for modeled RDTSC, RDTSCP, RDRAND, and RDSEED exits.
- `0003` enables the corresponding VMX exit controls for opted-in VMs.

These patches are guest-visible-value tripwires. They do not measure or advance
virtual time. See `../BUILD.md` for a clean apply and build recipe.
