# Fault policies

`fault-policy` defines the fault catalog, seeded selection policies, and recorded
fault reproducer format. It includes network, block, process, and named fault
points plus host-side perturbation descriptions. These are workload semantics;
the package using a policy supplies the applicable enforcement mechanism.

Process-class faults cover a node's whole lifecycle: pause, kill, restart, a
workload-defined hook run (`RunHook`), and a hold at an execution place
(`ProcPark`). `process_target` and `decode_process_target` give those faults the
one target encoding a host package and an in-guest agent both read.

A **standing fault** is a class, an opaque target, and a half-open V-time window.
The `standing` module carries both wire forms: the answer to a guest poll (the
current moment plus the windows containing it) and the window list a package
hands its service handler as configuration. The poll travels over the generic
SDK opaque service request under `STANDING_NAMESPACE`.

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

For guest-local enforcement of these faults, see the
[`fault-agent`](../fault-agent/README.md); for the search that proposes them, the
[`faults` package](../faults/README.md).
