# Architecture

Harmony separates execution from exploration. consonance supplies a
deterministic machine that can be captured, branched, and replayed. dissonance
treats that machine as a search target and decides which executions to try.

Workload packages connect these cores with typed execution adapters, evaluation
policies, and prepared programs. The boundary consists of operations such as `snapshot`, `branch`, `replay`,
`run`, and `read`, plus opaque recorded environments. dissonance does not need
to know how the machine virtualizes a CPU or stores a snapshot. consonance does
not need to know how the searcher evaluates a state. The
[control protocol](PROTOCOL.md) defines the operations shared across this
boundary.

## Execution flow

1. A machine is created from fixed workload bytes and deterministic
   configuration.
2. consonance runs it until a requested stop, such as a deadline, decision,
   assertion, crash, or quiescent point.
3. At a quiescent point, consonance can seal the complete state behind a
   snapshot handle.
4. dissonance selects a retained state, derives a new input or environmental
   mutation, and asks the machine to branch from that state.
5. The rollout returns observations. The workload evaluator derives progress
   keys and outcomes; Dissonance applies the configured archive retention policy.
6. The machine's recorded environment reproduces an individual timeline. The
   campaign stream separately records the choices needed to reproduce the
   search process.

A snapshot is a live resource owned by one machine or snapshot pool. A
reproducer is an input artifact that can reconstruct an execution. Snapshots
make branching inexpensive; reproducers make results portable.

## consonance

consonance owns the state and inputs that can affect future guest-visible
behavior.

### VMM engine

`vmm-core` is the architecture-neutral coordinator. It owns guest memory, the
run loop, deterministic time, entropy, snapshots, state hashing, guest-service
dispatch, and the control server. Common exits are handled by the engine.
Architecture-specific exits and devices are delegated through the `Vendor`
trait.

Current machine compositions use one virtual CPU. A run is therefore one
ordered sequence of serviced exits, decisions, timer deliveries, and stops.

### Architecture and virtualization backends

`vmm-backend` is the lower virtualization boundary. A backend enters the guest,
reports typed exits, saves and restores typed vCPU state, maps memory, and
delivers modeled interrupts. Linux KVM and macOS Hypervisor.framework are
substrates behind this boundary. Mock backends exercise the portable logic.

Architecture-specific policy lives under `vmm-core::vendor`:

- x86-64 owns its CPUID and MSR policy, boot protocols, xAPIC and legacy
  platform models, exit handling, and snapshot records;
- arm64 owns its CPU identity policy, board and image format, GICv3 and timer
  model, exit handling, and snapshot records.

The shared engine operates on guest-physical addresses, moments, bytes, hashes,
and common exits. It does not name ISA registers or architecture-specific
devices.

### Time and interrupt devices

`vtime` implements the deterministic clock and deadline queue. It consumes
explicit integer advances and does not read a host clock. `lapic` and `gicv3`
model the interrupt controllers and their timers as state machines driven by
that time. The VMM joins the clock, device deadlines, and backend delivery.

A cooperative guest can read the clock through a paravirtual page. The page is
guest memory, so its published value participates in snapshots and state
hashes.

### State and snapshots

A consonance snapshot has two main parts:

- `snapshot-store` retains guest memory as copy-on-write page layers;
- `vm-state` encodes the non-memory state needed to continue execution,
  including CPU, device, clock, timer, entropy, and contract identity state.

`vmm-core` coordinates the two at a synchronized, quiescent boundary. It also
owns portable snapshot import and export, which materialize the state behind a
session-local handle into a host-neutral artifact.

### Environment and guest communication

`environment` supplies seeded entropy, ordered payloads, opaque service answers,
and recorded inputs. An optional handler supplies service-specific responses.
Its implementation identity, configuration, and dynamic state participate in
snapshot/restore and state hashes. Capture and restore failures propagate to the
caller. The default handler supplies nominal responses.

Workload tooling owns fault catalogs, probabilities, eligibility, and decoding.
The execution core can schedule bounded memory writes, memory XOR operations,
and interrupt delivery at exact execution moments. An external package maps its
fault meanings onto these mechanical operations. Input format version 5 records
these operations and the selected service configuration; the fault-policy
package explicitly translates supported historical environments.

`hypercall-proto` defines guest service frames and `hypercall-doorbell` transports
them. `control-proto` supplies host machine operations. `consonance-client`
negotiates these operations and exposes raw SDK events, memory reads, session
lifecycle, and portable snapshots to workload drivers.

`harmony-linux` supplies controlled Linux and paravirtual transport. Package-owned
guest agents and image recipes live under `workloads/`. The guest environment
and package payload are separately identified parts of a prepared execution.

### Observation and acceptance

`telemetry` copies already-produced events to operator-facing sinks without
feeding them back into machine state. Canonical hashes and recorded artifacts
remain the execution evidence.

`unison` compares deterministic subjects and localizes divergence.
Stock-KVM virtual-time checks exercise controlled Linux guests. [Testing](TESTING.md) describes these layers.

## dissonance

The `dissonance` workspace is independent of the consonance build graph.

`searcher` contains the campaign coordinator, mutation machinery,
quality-diversity archive, deterministic worker scheduling, and checkpoint and
stream formats. Its generic rollout executor restores origins, replays parent
paths, applies suffixes, observes outcomes, and restores retention probes.
Workloads supply actions, evaluation, archive keys, milestones, and snapshots.
[Exploration](EXPLORATION.md) describes the search model.

## Workload packages

`workloads/nes` owns SMB and Nova adapters. Each supports QuickNES directly
and QuickNES inside Consonance through the same game semantics. The package's
machine driver interprets NES controller actions and publications. Shared
host/guest codec code validates publication versions and region bounds. Native
emulator snapshots and whole-VM snapshots retain distinct execution identities.

`workloads/fault-policy` owns the optional fault decision catalog and its legacy
compatibility adapter. `workloads/fault-runtime` owns deterministic guest fault
schedules and process/network enforcement. The systems package puts its
supervisor, logical nodes, message paths, and files inside one VM on one virtual
CPU, so whole-VM snapshots preserve the entire experiment.

The CLI selects a package and backend at campaign startup. Packages prepare the
input and record workload semantics, execution artifacts, and search settings as
separate identities. The default NES backend is native; the systems faults
package uses Consonance.

## Enforcing ownership

The [Cargo dependency policy](dependency-boundaries.toml) classifies every
first-party crate. A required CI check resolves declared dependencies across
workspaces, including optional, target-specific, build, and development edges.
Integration crates compose packages and cores without giving core crates reverse
dependencies. The semantic ownership lens in `REVIEWING.md` complements the graph
check by reviewing the behavior implemented inside each crate.

## Documentation ownership

Project-level concepts live in this directory. Concrete formats, register
models, command lines, and component limitations live in the README nearest
their implementation. Executable contracts and fixtures live with the code
that consumes them. Git preserves design history; maintained documentation
describes the current repository.
