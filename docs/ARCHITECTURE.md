# Architecture

Harmony separates execution from exploration. consonance supplies a
deterministic machine that can be captured, branched, and replayed. dissonance
treats that machine as a search target and decides which executions to try.

The boundary consists of operations such as `snapshot`, `branch`, `replay`,
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
5. The rollout returns observations. dissonance converts them into
   workload-specific progress keys and decides whether to retain the endpoint.
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

`environment` is the deterministic answering surface. It models guest decisions
such as entropy, payloads, scheduling, network policy, and injected faults. A
seeded environment generates answers. A recorded environment fixes selected
answers and host actions for replay. Guest-plane answers and host-plane
perturbations share one ordered timeline.

The in-band guest channel is split into `hypercall-proto`, which owns service
frames, and `hypercall-doorbell`, which transports those frames between a guest
and the VMM. The out-of-band `control-proto` is the explorer-facing machine
interface. The guest uses the first channel to request a service. The explorer
uses the second to drive and observe the whole machine.

`harmony-linux` contains the controlled Linux kernels, images, guest agents,
and SDK used by Linux workloads. The guest environment is part of the tested
machine composition.

### Observation and acceptance

`telemetry` copies already-produced events to operator-facing sinks without
feeding them back into machine state. Canonical hashes and recorded artifacts
remain the execution evidence.

`unison` compares deterministic subjects and localizes divergence.
`acceptance-suite` applies the project oracles to registered microprograms and
workloads. [Testing](TESTING.md) describes these layers.

## dissonance

The `dissonance` workspace is independent of the consonance build graph.

`machine` defines a small deterministic-machine vocabulary for search clients.
It mirrors the control operations with local types, so the searcher is not
coupled to consonance crates. Its QuickNES implementation provides an
emulator-backed target.

`searcher` contains the campaign coordinator, mutation policies,
quality-diversity archive, deterministic worker scheduling, checkpoint and
stream formats, and workload adapters. The generic layer sees actions,
observations, archive keys, milestones, and snapshots supplied by a `Game`. It
does not encode game rules.

Current adapters exercise the search machinery against NES workloads. A
consonance-backed Nova adapter runs the emulator inside the controlled Linux
guest while retaining the same search boundary. [Exploration](EXPLORATION.md)
describes the search model.

## Documentation ownership

Project-level concepts live in this directory. Concrete formats, register
models, command lines, and component limitations live in the README nearest
their implementation. Executable contracts and fixtures live with the code
that consumes them. Git preserves design history; maintained documentation
describes the current repository.
