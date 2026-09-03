# hypercall-proto

`hypercall-proto` is the `no_std` wire protocol and client/dispatcher support
for guest-to-host hypercalls. The optional `guest` feature provides the client;
the optional `host` feature provides the dispatcher and reference services.

## Frames

Frames are at most 4 KiB including a 24-byte header and use the little-endian
`HCP1` format. A header identifies request or response kind, service, opcode,
status, sequence number, payload length, and a reserved field. Payloads are
bounded by `MAX_PAYLOAD`; malformed magic, kind, lengths, reserved bits, and
truncated fields produce `ProtoError` rather than a panic.

The service identifiers are:

| Service | Purpose |
| --- | --- |
| `Console` | serial output |
| `Entropy` | seeded entropy |
| `Block` | capacity and sector reads |
| `Event` | one fire-and-forget event |
| `Net` | per-flow network decision |
| `Sdk` | buggify and coverage-yield decisions |
| `Pvclock` | virtual-time clock-page registration |
| `Payload` | exact-length ordered payload consumption |

Unknown services and opcodes receive explicit status responses. Service payload
formats are fixed and bounded; block reads use whole sectors, events are not
fragmented, and payload fetches consume exactly one matching tape entry.

## Host and guest sides

`Client<T: Transport>` encodes calls into a caller-provided frame buffer and
validates response identity, lengths, and status before exposing data. The
host-side `Dispatcher` routes decoded requests to registered `Service`
implementations and can save/restore service state. Registration and state
serialization use ordered structures so equal service state has equal bytes;
failed restores leave the dispatcher unchanged.

`SeededEntropy` supplies the reference xorshift64* stream and serializes its
non-zero state for snapshots. The network service carries environment answer
bytes opaquely; this crate does not depend on the environment catalog.

Golden, protocol, stateful, and public-API tests cover frame canonicalization,
service routing, state restoration, and client-side bounds. The crate is
portable and contains no hypervisor-specific device code.
