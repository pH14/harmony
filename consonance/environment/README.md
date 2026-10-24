# Deterministic input channel

`environment` owns Consonance's workload-neutral deterministic inputs: seeded
entropy and scheduler supplies, an ordered payload tape, opaque service
requests and responses, and the versioned input reproducer. Workload packages
supply service handlers and interpret their request and response bytes.

`channel::RecordedEnv` records overrides by virtual moment, service namespace,
and request identity. Guest-facing SDK requests use recorded, payload-bound
answers, deterministic core supplies, or the built-in nominal handler. A custom
handler's unrecorded answer is `External` unless its installed canonical service
policy declares that generic namespace local. An external question stops before
delivering guest bytes, and `Run { resolve }` records the host's answer before
resuming. A custom
handler cannot claim the nominal fast path by declaring the nominal identity.
The controller may use `RecordedEnv::decide` on a staged environment for a
local question; this alone does not certify a guest-facing continuation. The channel commits a
candidate handler only after a valid answer. Before and after each handler response, it
captures the live handler's identity, configuration, dispatch policy and state. If a shallow
clone changes that live state during an answer, external request, or error,
the channel and its transaction clones become poisoned. They reject later
requests and cannot produce a snapshot or state hash. Public environment clones
and internal handler clones compare both copies with the live state before use;
guest-facing SDK and VMM transactions reject a poisoned clone before accepting
a decision or answer. Handler implementations
must expose all mutable replay state through `snapshot_state` and keep it from
changing outside the channel. Capturing and restoring handler state is fallible, so failed
capture cannot produce a usable snapshot or state hash. A handler's identity,
configuration and dispatch policy are immutable contract selectors. An answered request that
changes a selector only in the candidate is rejected before that
candidate commits, leaving the live handler usable.
Reseeding follows the same candidate-commit rule: a shallow clone that changes
the original handler poisons every channel clone. Rejecting a separate candidate
leaves the entropy stream and live handler unchanged.
Integrations that probe a handler outside the channel can call
`verify_handler_unchanged` with a prior handler snapshot to poison the channel
if that probe changed its live handler.
`handler_fingerprint` and `verify_handler_fingerprint_unchanged` make the same
check against the handler alone without the recorded environment's snapshot
size limits. This lets a caller retire or replay an oversized live
recording while still detecting a shared handler changed during preparation.
Restoring a captured handler checks all selectors on the factory result and
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

The VMM clones a `RecordedEnv` for each service transaction and captures its
state after each answer, so these copies must stay cheap as the recording
grows. Clones and captures share the recorded answers. The answers live in a
shared sorted map plus a small map of recent answers; the recent map merges
into a fresh shared copy once it holds more than the square root of the shared
size. A clone or capture copies only the recent map. `RecordedEnv` and
`RecordedState` also keep the encoded size and unbound count of their answers,
and `InputSpec` keeps its full encoded length. Snapshot and reproducer size
limits are checked from those totals, and
`InputSpec::projected_reproducer_len_with_binding_reserve` sizes a reproducer
with pending effects, reseeds and one more bound answer without encoding it.

`ServiceHandler::respond` receives the virtual moment the question surfaced at
alongside the question. A client may use it to propose an explicit recorded
resolution, but guest execution does not call it for an unrecorded custom
answer.

`input_spec::InputSpec` combines the seed, service configuration, ordered
payloads, recorded answers, an optional piecewise coverage-quantum schedule,
reseed points, and mechanical effects. Its versioned
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
replies. The KVM and HVF VMM and the User-mode Linux bridge both call it for
recorded and nominal answers. Both execution paths pause before an unrecorded
custom answer is sent to the guest and resume after the client supplies a
payload-bound resolution.

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
exchange. The nominal answer is 1024. A branch may record a versioned schedule
with a positive default and ordered half-open windows. The SDK opcode-2 path
selects the exact recorded answer first, then the schedule at the request's
virtual moment. The schedule is part of reproducer, snapshot, and state hash,
so repeated callbacks do not grow the answer tape. A custom contract without
either answer stops as an explicit decision before the scheduler selection,
threshold, or guest response commits. Generic opcode-3 service questions with
numeric service ID 4 do not consult the coverage schedule.
`decide_coverage` takes the callbacks until the guest's next timer deadline
from the VM. With a runnable width of 1, `coverage_span` extends the next
threshold to the last whole quantum before that deadline, capped at
`COVERAGE_SKIP_LIMIT` callbacks.

`ServiceHandler::local_services` declares at most 32 strictly increasing, unique
namespace IDs above the core and coverage supplies (IDs 1–4). Empty means
external-only. The declaration is permission: a backend may still surface an
external decision instead of invoking local handling. `ServiceConfig` carries the same declaration; materialization,
reseed, checked clone and snapshot restore require exact equality. The policy is
part of recorded channel format 4, input reproducer format 8 and handler
fingerprint domain 2. Older records are refused. Factory and handler state
cannot silently change dispatch policy.

Local answering retains the actual guest request and all doorbell guards.
Recorded overrides win before handler execution. A handler returning `External`
leaves the actual question pending and discards its candidate state. An answered
candidate passes the same request binding, reproducer and SDK capacity checks
as explicit resolution. Guarded response publication precedes installation of
handler state, answer and cleared pending stop. Handler failures remain host
errors; they are not empty or nominal guest replies. The policy removes an
exported decision/resolution exchange, not a physical doorbell or guard check.

Local-answer capability is deliberately stateless: an answered candidate must
leave its complete captured handler fingerprint (identity, configuration,
dispatch declaration and state) unchanged. A stateful answer is a typed host
failure before response or tape publication, not an external fallback. This
allows a fresh reproducer to use its recorded override without skipping a
handler state transition. External handlers retain their existing stateful
contract. Supporting stateful computed local answers would require separate
computed-answer provenance and replay semantics and is outside this format.
