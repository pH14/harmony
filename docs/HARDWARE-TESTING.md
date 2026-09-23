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

Hosted runners have no arm64 hypervisor, so the arm64 ignored tests run in the
pre-push hook on a machine that has one. `HOST_TESTS` in
`scripts/ci_contract.py` lists them per machine, and this runs the current
machine's entry:

```sh
cargo nextest run --all-features --run-ignored only -E "$(python3 scripts/ci_contract.py host-filter)"
```

On Apple silicon the cargo runner in `.cargo/config.toml` signs each test binary
with the hypervisor entitlement.
