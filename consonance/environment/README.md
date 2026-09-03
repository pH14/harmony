# environment

`environment` is the deterministic fault and input model used by Dissonance.
It owns the guest decision catalog, host perturbation catalog, seeded and
recorded backings, and the byte-exact reproducer format. It contains pure logic
only; VM, guest, and host-device enforcement belongs to the owning services.

## Two control planes

The guest plane handles services a guest explicitly requests through
`Environment::decide`: entropy, payload bytes, scheduler choices, network flow
policies, block I/O, process lifecycle, and named buggify points. A
`DecisionPoint` identifies the class and service context. Supply classes return
`Answer::Supply`; fault classes return `Answer::Nominal` or a same-class
`Answer::Fault`. `DecisionPoint::admits` is the single admissibility check for
answers, including length, scheduler-range, and block-torn bounds.

The host plane contains workload-independent perturbations that the guest does
not request: virtual-time skew, clock-rate changes, memory corruption, and
interrupt injection. A `HostFault` is applied by the VMM at a `Moment`; it does
not pass through `Environment::decide`. `Action` joins host faults and guest
answers in the reproducer without making the decision engine understand either
enforcement mechanism.

## Reproducer and backings

`EnvSpec::Seeded` stores a seed and `FaultPolicy` for fully local decisions.
`EnvSpec::Recorded` adds a canonical `BTreeMap<Moment, Action>`, standing
faults, reseed markers, and an optional ordered payload tape. `Moment` is the
single ordered timeline for both planes. Repeated records at one moment replace
the prior action; standing faults describe correlated half-open windows.

`EnvSpec::encode` and `decode` implement the versioned reproducer blob. Maps and
standing faults are emitted in canonical order, and decoding rejects bad
versions, malformed tags, non-canonical ordering, truncation, and trailing
bytes. `materialize` exposes only guest overrides through `RecordedEnv`; the
frontier reads host overrides and standing faults separately. Snapshot callers
can save and restore the dynamic PRNG state and the remaining payload tape.

`SeededEnv` uses independent deterministic streams for supply values and fault
sampling. `RecordedEnv` first uses an admissible override at its current moment,
then falls back to the seeded streams. An inadmissible override is ignored and
does not consume a fallback stream value.

## Policies and proposals

`FaultPolicy` uses integer probabilities (`num/den`) and canonical eligible
fault lists. It supports per-class fault selection and per-point buggify bias;
the policy is part of every reproducer because a seed alone is insufficient to
reconstruct the answer sequence.

`EnvCodec` is the proposal seam for the searcher. `seeded` creates a base
environment, `mutate` deterministically changes host overrides while preserving
guest overrides and timeline facts, and `compose` rebases compatible recorded
overrides and reseed markers onto one `Moment` axis. Unsupported composition
shapes fail explicitly rather than silently producing a non-replaying artifact.

All public codecs are strict and panic-free on arbitrary bytes. The crate is
portable, has no I/O or hypervisor dependency, and is validated by catalog,
codec, replay, policy, mutation, property, and formal-bound tests.
