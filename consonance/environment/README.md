# Deterministic input channel

`environment` owns Consonance's workload-neutral deterministic inputs: seeded
entropy and scheduler supplies, an ordered payload tape, opaque service
requests and responses, and the versioned input reproducer. Workload packages
supply service handlers and interpret their request and response bytes.

`channel::RecordedEnv` records overrides by virtual moment, service namespace,
and request identity. A handler can answer immediately or return `External` to
stop the guest for a host-selected answer. The channel commits a candidate
handler only after a valid answer. Before and after each handler response, it
captures the live handler's identity, configuration, and state. If a shallow
clone changes that live state during an answer, external request, or error,
the channel and its transaction clones become poisoned. They reject later
requests and cannot produce a snapshot or state hash. Handler implementations
must expose all mutable replay state through `snapshot_state` and keep it from
changing outside the channel. Capturing and restoring handler state is fallible, so failed
capture cannot produce a usable snapshot or state hash. A handler's identity
and configuration are immutable contract selectors. An answered request that
changes either selector only in the candidate is rejected before that
candidate commits, leaving the live handler usable.
Reseeding follows the same candidate-commit rule: a shallow clone that changes
the original handler poisons every channel clone. Rejecting a separate candidate
leaves the entropy stream and live handler unchanged.
Restoring a captured handler checks both selectors on the factory result and
again after loading its state. It also checks that a new capture exactly
reproduces the recorded state bytes. Restoring a recorded environment makes the
same checks on a cloned handler before replacing the live one. If that clone
changes the live handler while restoring, the environment is poisoned instead
of accepting the candidate.

The first use of a recorded answer binds its request identity to the request
payload digest. Reusing that identity at the same virtual moment with different
payload bytes fails the SDK request instead of silently reusing an answer meant
for another question. The binding is part of the versioned channel snapshot
and input reproducer, so restore, branch, and portable replay enforce the same
check.

`RecordedState::encode_into` appends the same canonical snapshot encoding as
`encode` to an existing byte vector, preserving any prefix. Framing owners can
write nested records directly into their destination instead of allocating and
copying an intermediate buffer. Neither method changes the captured state.

`ServiceHandler::respond` receives the virtual moment the question surfaced at
alongside the question, so a handler whose answer depends on the guest's
position in virtual time reads it from the environment rather than keeping a
count of its own.

`input_spec::InputSpec` combines the seed, service configuration, ordered
payloads, recorded answers, reseed points, and mechanical effects. Its versioned
codec bounds lengths and validates ordering. Mechanical effects describe memory
writes and memory XOR; packages choose when and why to use them. The VMM validates their machine addresses before applying them.

`InputSpec::materialize` verifies the handler's identity and configuration both
before and after the initial reseed. A reseed that changes either value rejects
the input instead of returning an environment under a different service contract.

`InputSpec` writes service configuration and effects directly into its output,
filling each nested length after encoding. Standalone configuration and effect
encoders share those writers and retain the same bytes. This avoids temporary
per-effect buffers while preserving field order, tags, and length framing.

`InputSpec::decode` accepts one transport message and caps the complete blob at
`MAX_CHANNEL_BYTES`. `InputSpec::decode_snapshot` runs the same strict parser
for a caller-bounded captured section, so a long input history may contain more
than one transport message while every nested value remains bounded and ordered.
`InputSpec::decode_reproducer` and `try_encode_reproducer` admit a complete
reproducer up to seven MiB for the control plane. The one MiB headroom under
the SDK snapshot's eight MiB limit covers the maximum service handler state
when a reproducer restores the full payload tape at its branch origin.
Branch admission, scheduled effects, and host answers reserve the digest bytes
for every answer that has not yet been used, so first use cannot strand an
accepted branch beyond the export or snapshot limits.
Individual channel values remain capped at `MAX_CHANNEL_BYTES`.

`sdk` holds the SDK request rules every backend shares: the event classes,
service questions and answers, payload pulls, and coverage requests and
replies. The KVM and HVF VMM and the User-mode Linux bridge both call it, so a
guest gets the same answers on either backend.

The fault catalog and fault-selection policies live in
[`workloads/fault-policy`](../../workloads/fault-policy/README.md). An ordinary
Consonance execution uses the nominal handler and builds independently of that
package. Its dependency graph contains no workload package.

```sh
cargo test -p environment
cargo clippy -p environment --all-features --all-targets -- -D warnings
```

The coverage-quantum service (ID 4) takes a thread ID as the request ID and
the current threshold as eight little-endian bytes. It answers with a positive
eight-byte quantum: the number of coverage callbacks before that thread's next
exchange. The nominal answer is 1024. A workload handler answers from its own
recorded configuration, so quantum answers add nothing to the recorded state.
