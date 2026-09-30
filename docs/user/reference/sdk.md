# Guest SDK

`harmony-sdk` is a Rust guest library for reporting properties, progress, and lifecycle events and requesting seeded entropy. It uses no allocator and is generic over a transport. Linux OCI applications normally use `hypercall_doorbell::linux::DeviceTransport` with the `linux-device` feature.

See [add SDK assertions](../how-to/sdk.md) for a complete dependency and application example.

## Catalog

Create points with `Point::always`, `Point::sometimes`, `Point::reachable`, `Point::unreachable`, `Point::state`, or `Point::buggify`. Each takes `(id: u32, name: &'static str)`. Initialize with `Sdk::init(transport, &catalog)`.

Names must be unique within the catalog. IDs must fit in 24 bits (`0` through `16_777_215`) and be unique within their namespace. All assertion kinds share a namespace; state and buggify have their own. The entire catalog must fit in one 4,068-byte event payload, including its encoding overhead. Initialization returns an error for duplicate coordinates, duplicate names, oversized IDs, an oversized catalog, or a transport failure.

Keep IDs stable between runs of the same workload. The SDK does not validate each call against the declared point's kind, so use the matching declaration and method consistently.

## Assertions and state

Every method below returns `Result<_, SdkError<T::Error>>`.

| Method | Effect |
| --- | --- |
| `assert_always(cond, point)` | Emits a violation when `cond` is false |
| `assert_sometimes(cond, point)` | Emits a hit when `cond` is true |
| `assert_reachable(point)` | Emits a reached-point hit |
| `assert_unreachable(point)` | Emits a violation whenever called |
| `state_set(reg, value)` | Sends a `u64` value for a state register |
| `state_max(reg, value)` | Sends a maximum-update event; the host interprets it |
| `entropy_fill(&mut bytes)` | Fills the supplied slice through Harmony's seeded entropy service |

Assertion reporting is not a Rust panic and does not automatically exit the process. A false `sometimes` call emits nothing; absence of a hit does not by itself prove a bug in a finite campaign.

`Point::buggify` declares a point kind. The current `Sdk` wrapper does not expose a matching high-level `buggify()` decision method; it must not be assumed to inject faults simply because the point was declared.

## Lifecycle and advanced calls

| Method | Intended use |
| --- | --- |
| `setup_complete()` | Signal a setup boundary for a guest integration that owns its lifecycle |
| `frame_complete(frame_count)` | Signal a frame boundary |
| `coverage_yield(thread, observed, ready)` | Coverage-yield handshake; returns the host's `(u64, u32)` response |
| `client_mut()` | Access the underlying protocol client |

For a fault bundle, the supervisor owns setup. Ordinary application code should report properties and progress without creating another setup boundary.

## Other languages

For a language-independent integration, emit [checker directives](bundles.md#checker-directives) from hooks and continuous checks. Those directives are read by the supervisor; arbitrary node log lines are not automatically SDK events.

The repository also contains `libvoidstar.so`, a compatibility library for the public SDK ABI used by some instrumented workloads. It is not a set of documented, packaged language SDKs for Harmony. Device errors do not fall back to host randomness, and linking the shim alone does not enable event-directed fault actions. Prefer the explicit checker interface unless your image already supplies a compatible instrumented runtime.
