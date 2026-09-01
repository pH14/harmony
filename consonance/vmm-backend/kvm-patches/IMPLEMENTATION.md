# Deterministic-intercept patch implementation

The three-patch Linux KVM series provides opt-in userspace exits for x86
instructions whose host-derived values would violate the deterministic guest
contract.

`0001` introduces a capability, enable ioctl, exit record, and completion
record. `0002` validates completions, writes the modeled destination registers,
updates flags where required, and advances RIP. `0003` enables the VMX controls
for RDTSC/RDTSCP, RDRAND, and RDSEED only after userspace opts in.

The default remains stock KVM behavior. An opted-in VM fails closed if the
required CPU controls or completion state are unavailable. The VMM supplies
clock values from its exit-count virtual clock and entropy from its seeded
stream. No patch in this directory implements timing, preemption, performance
counter access, or instruction stepping.
