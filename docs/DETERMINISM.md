# Determinism

Harmony defines exact replay as follows: given the same machine definition and
recorded inputs, an execution produces the same ordered events, guest-visible
output, and complete machine state.

The identity of a run includes the workload bytes, architecture and machine
contract, Harmony version, initial state, and ordered environment. A numeric
seed alone does not identify a run. Artifact digests and versioned encodings
make mismatched inputs detectable.

The claim is bit-level within one architecture. Harmony does not claim that an
x86-64 execution and an arm64 execution have identical state encodings.

## Transition model

For a current single-vCPU composition, execution can be modeled as a sequence
of deterministic transitions:

```text
state[n+1] = transition(state[n], normalized_event[n], environment[n])
```

At the start, the workload bytes, machine policy, initial state, seed, and
environment are fixed. Given equal state before an event, execution through the
admitted instruction set reaches an equivalent guest-visible event. The vendor
layer normalizes the event. The engine handles it using prior state and recorded
inputs. Time advances, entropy is consumed, devices change, and interrupts
become eligible under integer rules. The resulting state is equal.

The argument requires every value that can influence a later transition to be
inside the model. Adding a device, input channel, CPU feature, or asynchronous
source extends the claim only after its state, ordering, and replay behavior are
modeled and tested.

## Virtual time

consonance does not derive guest time from wall time, host CPU frequency, or a
performance counter. Its V-time clock is an integer accumulator advanced at
normalized machine events.

Each normalized exit class has an advancement rule. A guest doorbell can carry
a modeled duration. Device and architectural exits use configured class
durations. Terminal events do not advance time. Raw substrate-only exits can be
excluded from the portable event sequence, so a backend implementation detail
does not become guest-visible time.

Timers use absolute V-time deadlines. They become eligible at the first event
boundary whose post-advance time reaches the deadline. Equal deadlines are
ordered by their scheduling sequence. When the guest is idle and a deliverable
timer is pending, the clock advances directly to the next deadline.

A cooperative guest reads this clock through a paravirtual page. Publication is
canonical. Snapshot state includes the clock and its registration. Restore
publishes the restored value before execution resumes.

## Entropy and environment

The environment owns values the workload cannot determine internally. These
include seeded entropy, supplied payloads, selected scheduling decisions,
guest-visible fault answers, and host-plane perturbations.

A seeded environment produces a repeatable default stream. A recorded
environment fixes selected answers and host actions at their `Moment`s.
Branching restores captured state and installs a new environment for
exploration. Replaying restores state without replacing its recorded
continuation.

External data is deterministic only after it crosses this boundary. Guest
services use controlled request and response paths. Host faults are staged on
the deterministic axis and added to the active recorded environment when
applied. Operator telemetry remains outside transition state and does not feed
back into the guest.

## Complete state

Replay state includes latent state as well as visible output:

- guest RAM and CPU state;
- interrupt-controller, timer, serial, and other modeled device state;
- V-time and pending deadline state;
- deterministic entropy state and guest-service progress;
- outstanding delivery or completion state that can affect the next entry;
- the identity of the machine contract used to interpret the state.

Guest memory is stored in copy-on-write layers. Remaining state is encoded in a
versioned canonical blob. A snapshot is sealed only at a boundary the VMM can
restore exactly. Requests at intermediate boundaries that cannot be represented
fail rather than creating a partial checkpoint.

The whole-state hash covers state that can affect future execution. An
observable digest is narrower and covers output deliberately emitted by the
guest. The distinction is used by the acceptance oracles described in
[Testing](TESTING.md).

## CPU and architecture boundary

consonance installs an architecture-specific CPU identity and handles or
excludes channels that expose host-specific time, entropy, performance
counters, save-image residue, or implementation identity.

Depending on the operation and substrate, closure can use one or more of these
mechanisms:

- expose a fixed architectural identity;
- provide a consonance-owned replacement for time or entropy;
- trap an operation and handle it deterministically;
- hide the associated feature and audit owned executable images;
- canonicalize architecturally irrelevant state before hashing or saving.

Not every substrate can trap every instruction. The current model therefore
has a cooperative guest boundary. Owned kernels and payloads follow the
advertised feature set and are audited where masking alone cannot prevent an
instruction from executing. A binary that deliberately executes a hidden,
untrappable entropy or timing instruction is outside the claim.

## Trust and portability

The virtualization substrate is trusted to save, restore, and report the
architectural state it exposes correctly. The argument also trusts consonance's
engine and device models, the selected CPU contract, and the controlled guest
environment.

Raw backend logs and substrate-private exit counts do not need to match. Only
normalized guest-visible transitions have portable meaning.

Cross-host replay within one ISA adds the assumption that two qualified
implementations agree on the admitted architectural subset after Harmony's
normalization and canonicalization. Hardware qualification tests specific CPU
and backend compositions. It does not establish behavior for every instruction
encoding or future processor.

## Scope

The current determinism claim has these limits:

- machine compositions are single-vCPU;
- only controlled guest images and admitted instruction surfaces are covered;
- network, storage, clock, entropy, or device effects outside modeled services
  are not replayable;
- a modeled device is incomplete until its full state and event ordering
  participate in snapshots and hashes;
- operator wall-time limits may stop a search, but wall time does not select or
  rank deterministic machine states;
- same-ISA portability applies only to qualified machine compositions;
- no cross-ISA state identity is claimed.

An unavailable capability or incompatible artifact is reported explicitly. It
does not receive a best-effort deterministic interpretation.
