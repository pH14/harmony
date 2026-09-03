# control-proto

`control-proto` defines the out-of-band control protocol used to drive a VM as a
black box. It provides the wire value types and a strict, canonical codec; the
socket, VM backend, and request execution live in `vmm-core`.

## Protocol model

Every session begins with `Request::Hello`, which negotiates the application
protocol and capability geometry. The remaining requests cover snapshot
management (`Snapshot`, `Drop`, `Branch`, `Replay`), execution (`Run`), state
observation (`Hash`, `Read`, `Regs`), host perturbations (`Perturb`), capture
(`SdkEvents`, `Console`), and explicitly tainted improvisation
(`Exec`, `RecordedEnv`). `Run` returns either a guest-observable `StopReason`
or a `ControlError`; backend and transport failures are not masqueraded as
guest outcomes. `Exec` is deliberately outside the reproducer; a timeline that
uses it is tainted, and `RecordedEnv` reports an error instead of minting a
non-replaying artifact.

`Reproducer`, `Answer`, and `HostFault` are opaque carried values here. Their
schemas belong to `environment`; the backend decodes and validates them at the
service boundary. `Moment`, snapshot and decision identifiers, stop masks, and
hash scopes are plain protocol values. `Branch` explicitly restores and
re-seeds, while `Replay` restores verbatim.

## Wire format

Frames contain `magic("CTL1")`, a framing version, sequence number, body
length, and a tagged body. Header and body integers are little-endian. Variable
length values use `u32` lengths, and the body is capped at `MAX_FRAME_LEN`
(16 MiB). `Read` requests are capped at `READ_CAP` (64 KiB).

Encoding is canonical: fixed field order, one representation per value, and no
unordered data. Decoding is strict and total. A partial frame returns
`Ok(None)` for streaming callers; malformed headers, over-cap lengths, unknown
tags, truncated fields, and trailing body bytes return `ProtocolError` without
allocating an unbounded buffer or panicking.

The framing version (`PROTO_VERSION`), application vocabulary version
(`APP_PROTOCOL_VERSION`), and environment blob version are independent. The
codec validates only framing; application and blob versions are checked by the
negotiation and backend layers.

## Modules and validation

- `types.rs` contains requests, replies, handles, run controls, and outcomes.
- `error.rs` separates wire failures from backend/control failures.
- `codec.rs` implements little-endian framing and the bounds-checked reader.

Golden-byte, round-trip, streaming, negotiation, adversarial, loopback, and
public-API tests exercise the codec. The crate has no hypervisor or socket
dependency and is usable as a portable host-side protocol library.
