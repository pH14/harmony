# Control protocol

The control protocol is the semantic boundary between an explorer and a live
consonance machine. It can be driven in process or carried over a byte stream.
The operations and their obligations are the same in either form.

`control-proto` owns the versioned request and reply vocabulary. `vmm-core` owns
the server that applies it to a VM. dissonance's machine interface mirrors the
state operations without depending on either crate.

A session is one client and server transport lifetime. It is independent of a
search campaign.

## Session plane

`Hello` exchanges protocol version, accepted reproducer versions, coverage-map
geometry, and capability flags.

Compatibility must be established before use. `Hello` is the first operation
in a session. Before it, other operations are unsupported. The server rejects a
mismatched protocol version there rather than opening the session, and a client
checks the returned capabilities before issuing another operation.

## State plane

The state operations are:

| Operation | Meaning |
|---|---|
| `Snapshot` | Seal the current quiescent state and return its handle and evidence cut. |
| `Branch` | Restore a snapshot and install a new reproducer for exploration. |
| `Replay` | Restore a snapshot verbatim for reproduction. |
| `Run` | Advance to a requested stop condition. |
| `Drop` | Release a snapshot handle. |

The obligation of this plane is replay identity. Restoring a snapshot and
applying the same inputs must produce the same complete state.

A snapshot reply binds values taken from one stopped state: its session-local
handle, seal `Moment`, timeline taint, and the prefix length of SDK events
included in the seal. The prefix length is authoritative because several events
may share a `Moment`. Console bytes are a separate observation stream and are
not part of that cut.

Handles name resources in the snapshot pool. Unknown or dropped handles are
errors. A `Run` returns a guest outcome such as a deadline, quiescence, crash,
surfaced decision, snapshot point, or assertion. Protocol and machine failures
are reported separately from these outcomes.

## Observation plane

The observation operations are:

| Operation | Meaning |
|---|---|
| `Hash` | Return a canonical digest for a requested scope. |
| `Read` | Read an exact range of guest-physical memory. |
| `Regs` | Return a versioned register view. |
| `Console` | Read a page of captured serial bytes. |
| `SdkEvents` | Read a page of `Moment`-stamped guest SDK events. |

Observation is state-neutral. Inserting observations into a run does not alter
its state hash, V-time, environment, or later behavior.

Paging state belongs to the request rather than the server. Console and SDK
event cursors are explicit offsets. A memory read returns the requested bytes
exactly or an error. The register reply is an observation schema rather than
the save and restore representation, and can evolve additively.

The vocabulary defines whole-state, disk, and memory-region hash scopes. The
current server implements the whole-state digest. An unavailable scope returns
`Unsupported` rather than a substitute value.

## Intervention plane

`Perturb` stages a host-plane fault at a `Moment`. `Exec` injects an interactive
serial command and runs to a completion marker or deadline. Each operation has
an explicit recording policy.

A perturbation is a recorded input. When applied, it joins the active recorded
environment at the requested point. Invalid, past, or unschedulable
perturbations fail without changing the recorded timeline.

`Exec` is off the record. Its first use taints the current timeline. Later
snapshots preserve the taint, and the server refuses to mint a reproducer from
that timeline. The serial command has no replay guarantee. The taint ensures it
cannot be mistaken for reproducible evidence.

## Provenance plane

`RecordedEnv` returns the genesis-complete reproducer for the current point.

Persisted reproducers have stable, versioned meaning. The reproducer is opaque
to the control codec and generic search code. Its owning machine validates the
blob version and contents. A tainted timeline returns an error instead of a
reproducer.

Round-trip codec tests do not by themselves preserve persisted evidence because
an encoder and decoder can change together. Golden bytes and version rejection
make representation changes visible.

## Stops and decisions

`Run` takes an optional deadline, a mask of decision classes to surface, and an
optional answer to the immediately preceding decision. Terminal conditions
always surface. Other guest decisions can be answered by the installed
environment or returned to the explorer when their class is armed.

At most one decision is outstanding on a timeline. An answer without a pending
decision is an error. Silently discarding it would desynchronize the client and
machine.

## Hash vocabulary

Three related names have separate roles:

| Name | Role |
|---|---|
| `state_hash` | Digest of modeled state that can affect future execution, including latent state. |
| `observable_digest` | Digest of output deliberately emitted by the guest. |
| `Hash { scope }` | Protocol operation used to request a digest. |

`state_hash` establishes replay identity. `observable_digest` describes visible
workload behavior. `Hash` is the request vocabulary, and its scope determines
the digest requested.

## Representation ownership

Frame tags, integer widths, size limits, discriminants, and byte order are
owned by `consonance/control-proto`. Guest-service frame details are owned by
`consonance/hypercall-proto`, and their transport is owned by
`consonance/hypercall-doorbell`. These representations are contracts. Their
tables live beside the implementations that encode and decode them.
