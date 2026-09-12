# Harmony supervisor

`harmony-supervisor` is the platform-owned child-process supervisor installed
inside the OCI rootfs at `/usr/lib/harmony/supervisor`. It reads the one
canonical execution document from `/run/harmony/execution.json`. The launcher
does not accept command-line policy: a missing bundle runs the document's
command once, while a bundle path selects the same binary's structured node,
hook, readiness, and standing-window path.

The supervisor owns its child process groups and reaps descendants in the PID
namespace. Every child receives the resolved uid, gid, and supplemental groups
from the execution document. The supervisor retains its own privilege for
platform device operations. `/dev/harmony` is accessed through the shared
`hypercall-doorbell::linux::DeviceTransport`; the supervisor has no raw MMIO
transport. Parking uses the required `/dev/harmony-park` interface. Device errors fail
execution rather than silently omitting a requested process action.

`bundle`, `directive`, `reconcile`, and `supervise` are portable library
modules. Linux device and process wiring is isolated to the binary. The
standalone crate can be checked on a development host with:

```sh
cargo test --manifest-path consonance/harmony-linux/supervisor/Cargo.toml
cargo clippy --manifest-path consonance/harmony-linux/supervisor/Cargo.toml --all-targets -- -D warnings
```

Miri covers the portable parsing and reconciliation modules. Process integration
tests run natively: Miri cannot execute the credential, spawn, and wait syscalls.
The native suite checks process-group isolation, descendant cleanup, and readiness
probes alongside running nodes; the OCI fixture checks guest credentials.
