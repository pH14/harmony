# Hardware testing

Hardware tests run manually on separately provisioned hosts. This public
repository must not dispatch jobs to home-network self-hosted runners. There
is no repository-managed hardware runner or reserved CPU allocation.

Before running an ignored hardware test, verify its documented CPU, kernel,
virtualization, perf, and guest-image requirements. Reserve an idle CPU
allocation on the chosen host and adapt any example taskset CPU numbers to
that allocation. Follow the requirements of the current test and runtime;
the legacy patched-KVM acceptance stack and hardware-pinned CPU contracts
have been removed.

Arm64 hosts have their own ignored tests. On Apple silicon, run
`cargo test -p vmm-backend --test hvf_smoke -- --ignored` and
`cargo test -p vmm-core --lib vendor::arm64::contract -- --ignored`; the cargo
runner in `.cargo/config.toml` signs each test binary with the hypervisor
entitlement. On an arm64 KVM host, run the second command.
