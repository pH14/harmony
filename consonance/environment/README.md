# Deterministic input channel

`environment` owns Consonance's workload-neutral deterministic inputs: seeded
entropy and scheduler supplies, an ordered payload tape, opaque service
requests and responses, and the versioned input reproducer. Workload packages
supply service handlers and interpret their request and response bytes.

`channel::RecordedEnv` records overrides by virtual moment, service namespace,
and request identity. A handler can answer immediately or return `External` to
stop the guest for a host-selected answer. Handler changes commit only after a
valid answer; an external request preserves the handler state. Capturing and
restoring handler state is fallible, so failed capture cannot produce a usable
snapshot or state hash.

`input_spec::InputSpec` combines the seed, service configuration, ordered
payloads, recorded answers, reseed points, and mechanical effects. Its versioned
codec bounds lengths and validates ordering. Mechanical effects describe memory
writes, memory XOR, and interrupt delivery; packages choose when and why to use
them. The VMM validates their machine addresses before applying them.

The fault catalog and fault-selection policies live in
[`workloads/fault-policy`](../../workloads/fault-policy/README.md). An ordinary
Consonance execution uses the nominal handler and builds independently of that
package. Its dependency graph contains no workload package.

```sh
cargo test -p environment
cargo clippy -p environment --all-features --all-targets -- -D warnings
```
