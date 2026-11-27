# Fault policies

`fault-policy` defines the fault catalog, seeded selection policies, and recorded
fault reproducer format. It includes network, block, process, and named fault
points plus host-side perturbation descriptions. These are workload semantics;
the package using a policy supplies the applicable enforcement mechanism.

Process-class faults cover a node's whole lifecycle: pause, kill, restart, a
workload-defined hook run (`RunHook`), and execution callbacks
(`ProcEventKill` and `ProcEventPark`).
Event-kill rarities use the shared 0 through 63 width. Event parks carry an
edge count from 1 through `1 << 24`, a positive hold, an optional target
range of module offsets, and a flag that makes the runtime count one per edge
from the arm instead of each edge's share of its site's visits. The runtime also
counts one per edge under a target one offset wide. `process_target` and `decode_process_target` give those faults
the one target encoding a host package and the platform supervisor both read.

A **standing fault** is a class, an opaque target, and a half-open V-time window.
The `standing` module carries both wire forms: the answer to a guest poll (the
current moment plus the windows containing it or closed since the previous poll
moment the guest sends) and the window list a package hands its service handler
as configuration. The poll travels over the generic
SDK opaque service request under `STANDING_NAMESPACE`.

A **choice** is a guest driver's draw that the host may answer
(`DecisionPoint::Choice`, ours). The point carries the site, a 32-bit FNV-1a hash of
the site name (`choice_site`), and the option count. `ChoiceQuestion` is its wire
form under `CHOICE_NAMESPACE`: the option count, the guest's own draw (the
default), the guest's monotonic clock and the site name. `Answer::Nominal` takes
the default and `Answer::Label(l)` takes option `l`, which the point admits only
when `l` is below the option count. A recorded deviation is a guest override,
one `Answer` per `Moment`; `RecordedEnv::recorded` returns the override at the
current moment so a caller can tell it from the base answer. The seeded
environment answers every choice `Nominal`. The harness kit asks through this
point from `sdk.Below`, and the [`faults` package](../faults/README.md) answers it.

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

For guest-local enforcement of these faults, the platform supervisor consumes
the generic process and standing-fault contracts; for the search that proposes
them, see the [`faults` package](../faults/README.md).
