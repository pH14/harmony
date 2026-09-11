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

The same call carries the host-plane effects the run after the branch applies,
each against the virtual moment it lands at, so a package stages a machine-level
perturbation without reaching past the session boundary. The control server
checks every effect against the branched snapshot before the live VM changes: a
moment behind the snapshot, a moment already occupied, an out-of-range address,
an interrupt identity the machine reserves, or a backend that cannot arm the
exact-count arrival all fail the branch with the session untouched. One moment
carries one effect, so a duplicate is reported rather than overwritten.

`Session::run_until` runs to an absolute virtual-time deadline or an earlier
stop. `Session::snapshot` captures that exact stopped state in one control
exchange and returns the server's synchronized V-time. It never advances the
guest or retries a refusal, so a capture failure is returned to the caller with
the control diagnostic.

`SessionConfig::defer_virtual_time_checkpoint_hashes` moves sparse
virtual-time checkpoint hashing out of the run that reaches a checkpoint. Each
due checkpoint otherwise hashes all of guest RAM inside that run, which a
gigabyte-class guest cannot afford during boot. The session applies the setting
to every VM it boots, including the ones a restore boots from its factory, and
before the guest runs, so the boot is covered. The setting is off by default,
changes neither guest state nor the normalized event sequence, and stays
outside the session identity; a composition root that wants the hashes installs
them afterwards with `Vmm::checkpoint_virtual_time_trace_at`.

`SessionConfig::wall_limit` bounds one run in host time. A guest spinning on a
frozen virtual clock takes no exit, so it never reaches its virtual-time
deadline and only the host clock notices it; past the bound the run is
abandoned through the backend's cancellation latch and reported as
`SessionError::Hung`. A canceled VM cannot be entered again, so every later
request on that session reports `SessionError::Abandoned`. A backend with no
cancellation latch can honor no such bound and reports
`SessionError::Unboundable` on the first run rather than running unbounded. The
limit is a host resource bound, so it is deliberately outside the session
identity and the image identity. The `watchdog` module owns the mechanism — it
reserves SIGUSR1 process-wide, so every composition that arms a host bound
shares this one guard. A request that returns just before the bound expires
claims the run and keeps its reply.

`SparseSnapshot` is the explicit `consonance-whole-vm-v2` archive shape used
by adapters that need page and sidecar sharing across related checkpoints.
Its serde fields remain `base`, `image_identity`, `pages`, and `sidecar`; the
sharing metadata is host-local and is never written to the wire. An export
base must have the same setup and identity, and unchanged pages/chunks are
retained by reference until a snapshot is serialized.

The embedded complete and sparse portable snapshots use format version 4,
which also carries undelivered SDK stops and unanswered service requests.
Readers still accept version 3 (no pending stop); versions 1 and 2 remain
explicitly unsupported. The execution identity retains its version-3
compatibility token so existing artifacts remain importable; the artifact's
own header selects the codec. The outer sparse archive layout remains version 2.

```sh
cargo test -p consonance-client
cargo clippy -p consonance-client --all-features --all-targets -- -D warnings
```
