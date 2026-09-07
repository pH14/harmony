# Consonance client

`Client<T>` negotiates the control contract over a synchronous `Transport` and
preserves protocol failures as errors distinct from workload stops. The optional
`in-process` feature implements transport for `ControlServer`.

The SDK catalog decoder resolves named state registers and validates declarations
and values. The current SDK event stream carries no publisher identity; this
reader accepts one catalog per evidence stream and rejects multiple publishers
rather than merging ambiguous register coordinates. Read a fresh catalog/state
view at the stopped snapshot evidence cut when constructing a workload driver.

`session::Session` is the optional in-process composition layer used by
workload adapters that boot a guest directly. `SessionConfig` makes RAM,
seed, run budget, command line, and identity domain explicit. The session
owns setup, branch, replay, run, read, and SDK-event operations, while
workloads retain only their observation and action codecs. `PortableSnapshot`
keeps the original full-copy archive format used by the fault workload.
Sessions release completed host trace segments after successful branch/replay
operations, keeping their evidence storage bounded by the active segment.
Callers that archive normalized exit traces use the control server's trace API.

A package that answers its own opaque service requests installs a resolver with
`Session::set_service_factory` and branches with `branch_with_service`, which
carries the package's `ServiceConfig` into the branch so the control server
builds that handler. The handler is built before the live VM changes, so an
uninstalled configuration fails the branch and leaves the session untouched.

`Session::run_until` runs to an absolute virtual-time deadline or an earlier
stop. `Session::seal` snapshots the current stopped state, running the guest a
further settle step whenever the control server cannot seal that point yet, and
gives up once the caller's total settle allowance is spent. A guest that has
crashed or gone quiescent advances no further, so its endpoint is offered one
last seal and then reported rather than settled again.

`SessionConfig::wall_limit` bounds one run in host time. A guest spinning on a
frozen virtual clock takes no exit, so it never reaches its virtual-time
deadline and only the host clock notices it; past the bound the run is
abandoned through the backend's cancellation latch and reported as
`SessionError::Hung`. The limit is a host resource bound, so it is deliberately
outside the session identity and the image identity. The `watchdog` module owns
the mechanism — it reserves SIGUSR1 process-wide, so every composition that
arms a host bound shares this one guard.

`SparseSnapshot` is the explicit `consonance-whole-vm-v2` archive shape used
by adapters that need page and sidecar sharing across related checkpoints.
Its serde fields remain `base`, `image_identity`, `pages`, and `sidecar`; the
sharing metadata is host-local and is never written to the wire. An export
base must have the same setup and identity, and unchanged pages/chunks are
retained by reference until a snapshot is serialized.

The embedded complete and sparse portable snapshots use format version 3,
which captures service state through the generic SDK channel. Import rejects
versions 1 and 2 explicitly. Execution identities record sidecar version 3;
the outer sparse archive field layout remains version 2.

```sh
cargo test -p consonance-client
cargo clippy -p consonance-client --all-features --all-targets -- -D warnings
```
