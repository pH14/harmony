# Fault policies

`fault-policy` defines the fault catalog, seeded selection policies, and recorded
fault reproducer format. It includes network, block, process, and named fault
points plus host-side perturbation descriptions. These are workload semantics;
the package using a policy supplies the applicable enforcement mechanism.

The `consonance` adapter maps supported policy questions and answers onto the
opaque deterministic service channel. Unsupported host operations return an
error. Policy identity, configuration, and dynamic state participate in capture
and replay through the channel's handler contract. Ordinary Consonance execution
uses its nominal handler and requires no dependency on this crate.

The catalog codec has unit, property, golden, and Kani checks:

```sh
cargo test --manifest-path workloads/fault-policy/Cargo.toml
cargo kani --manifest-path workloads/fault-policy/Cargo.toml
```
