# Add SDK assertions

Use assertions to tell Harmony what correctness means for your application. For a fault bundle, a [checker script](fault-workload.md#make-the-checker-meaningful) is the smallest integration and works with any language. Use the Rust guest SDK when the property must be observed inside your application.

## Add the Rust dependencies

The SDK is currently consumed from source. Keep a Harmony checkout next to your application and pin its commit. For this layout:

```text
workspace/
  harmony/
  my-app/
    Cargo.toml
    src/main.rs
```

Add these dependencies to `my-app/Cargo.toml`:

```toml
[dependencies]
harmony-sdk = { path = "../harmony/consonance/harmony-linux/sdk" }
hypercall-doorbell = { path = "../harmony/consonance/hypercall-doorbell", features = ["linux-device"] }
```

Use the same Harmony revision as your execution environment. These are source dependencies, not instructions to install a published crates.io SDK.

## Declare and report properties

This complete Linux guest program declares an invariant, a reached point, and a progress register:

```rust
use harmony_sdk::{Point, Sdk};
use hypercall_doorbell::linux::DeviceTransport;

fn main() -> Result<(), String> {
    let transport = DeviceTransport::open().map_err(|e| e.to_string())?;
    let mut sdk = Sdk::init(
        transport,
        &[
            Point::always(1, "acknowledged-record-is-readable"),
            Point::reachable(2, "verification-completed"),
            Point::state(1, "records-checked"),
        ],
    )
    .map_err(|e| e.to_string())?;

    let expected = b"record-1";
    std::fs::write("/tmp/record", expected).map_err(|e| e.to_string())?;
    let observed = std::fs::read("/tmp/record").map_err(|e| e.to_string())?;
    sdk.assert_always(observed == expected, 1)
        .map_err(|e| e.to_string())?;
    sdk.state_set(1, 1).map_err(|e| e.to_string())?;
    sdk.assert_reachable(2).map_err(|e| e.to_string())?;
    Ok(())
}
```

Replace the small file round-trip with an observation of your application. IDs are stable identifiers, not source line numbers. The state register can reuse numeric ID 1 because it occupies a different namespace from assertions.

A failed SDK assertion reports an event; it does not panic, stop the process, or change the process exit code automatically. Decide whether your application should continue after reporting it. Propagate transport errors: an assertion that never reached Harmony is not a passing assertion.

## Build and run inside the guest

Compile for Linux and the host CPU architecture, then include the executable in your OCI image. For example, on native x86-64 Linux with rustup and a linker installed:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

The static target is useful for a small Rust application, but static linking alone does not qualify arbitrary dependencies. Arm64 applications need an Arm64 build satisfying the guest's instruction restrictions; do not copy this x86 target for an Arm64 host.

For this example, a Dockerfile in `my-app` can package the binary (replace `my-app` if your Cargo package has another name):

```dockerfile
FROM alpine:3
COPY target/x86_64-unknown-linux-musl/release/my-app /app/sdk-demo
ENTRYPOINT ["/app/sdk-demo"]
```

Build the image and run it using [the OCI guide](oci.md). `/dev/harmony` is supplied inside the Harmony guest. Running this program directly on an ordinary host or in a normal Docker container fails to open that device.

`oci run` records process results and console output; it is not an assertion dashboard. To have a fault campaign collect property violations, run your instrumented program as a node or hook in a [fault bundle](fault-workload.md), then inspect the campaign report. Direct SDK events from a node do not replace the supervisor's completed-check evidence from a `check` command.

The bundle supervisor owns setup completion. Do not add a `setup_complete()` call to a normal supervised application. See [SDK reference](../reference/sdk.md) for specialized lifecycle interfaces.
