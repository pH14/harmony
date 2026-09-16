# Platform scripts

The instruction scanners reject unsupported instructions in shipped guest
executables. Their planted negative controls must continue to fail.

`build-platform-runtime.sh` builds the native architecture's kernel, static
supervisor, OCI runtime, and OCI smoke fixture, then seals their manifest. Run
`make -C consonance/harmony-linux fetch` first. Install the pinned
`nightly-2026-06-16` Rust toolchain with `rust-src` and the architecture's Linux
musl target. ARM builds rebuild the Rust standard library against LSE musl and
scan the complete shipped binaries for unsupported instructions.

`runtime-artifacts.py` records and verifies the canonical kernel, OCI runtime,
and platform fixture. `source-key` hashes platform build sources and pinned
contracts; generated build/download directories are excluded. `seal` records
that source digest and every required artifact digest in `runtime-manifest.json`.
Run it only after building the artifacts from those sources.

`verify` reports one of four qualification scopes:

- `exact-input`: all artifact digests match, and their source digest matches the checkout.
- `host-only`: the artifacts match their manifest, but were built from different sources.
- `inconclusive`: the artifact directory has no manifest.
- `unavailable`: required artifacts are missing, corrupt, or incompatible.

Only a successful hardware smoke with `exact-input` artifacts qualifies a guest
platform change. A successful smoke with `host-only` artifacts checks host
compatibility. The other scopes fail prerequisite verification and cannot pass
qualification. CI retains the machine-readable result with the smoke logs.

```sh
python3 consonance/harmony-linux/scripts/runtime-artifacts.py verify \
  --architecture aarch64 --artifacts /path/to/aarch64 --output /path/to/provenance.json
python3 consonance/harmony-linux/scripts/test_runtime_artifacts.py
```

### Packaging the Nix platform

The OCI producer builds its Rust payloads once, builds the Nix kernel/runtime
once, then packages the generic fixture without rebuilding the kernel:

```sh
consonance/harmony-linux/scripts/build-platform-runtime.sh --runtime-payloads "$PWD/runtime-payloads"
export HARMONY_NIX_RUNTIME_INIT="$PWD/runtime-payloads/init.sh"
export HARMONY_NIX_RUNTIME_SUPERVISOR="$PWD/runtime-payloads/harmony-supervisor"
export HARMONY_NIX_RUNTIME_MANIFEST="$PWD/runtime-payloads/runtime-payloads.json"
nix run .#platform-guest-images -- --oci-runtime --output "$PWD/nix-guest-output"
consonance/harmony-linux/scripts/build-platform-runtime.sh --from-nix "$PWD/nix-guest-output"
```

Both output directories must be new. `--runtime-payloads` builds the supervisor
with the pinned Rust/remap settings and copies canonical `runtime/init.sh`.
Its manifest binds source, architecture, toolchain/target and payload hashes.
Nix verifies the external payloads against its actual copied source before and
after assembly, rejects source changes, and records all produced file hashes.

`--from-nix` verifies those source/output bindings, builds only `runtime-fixture`,
and preserves the exact kernel, OCI archive and direct `initramfs.cpio.gz`.
The direct fixture is required; absent bytes are unavailable, not reconstructed.
The canonical `runtime-manifest.json` seals these files and the original OCI
manifest, with the Nix build/payload records under `build-provenance/`.

This bridge is required by the source-correct producer/consumer handoff: the
Nix assembler consumes externally built Rust payloads, and quick consumers
require the canonical artifact layout. Removing the binding could label stale
payloads or kernel bytes with the current source key; using the ordinary builder
instead would rebuild the kernel. Records rely on the trusted builder and do
not provide signatures or instruction admission. The ordinary native and ARM
build paths remain available.

```sh
python3 consonance/harmony-linux/scripts/test_nix_runtime_artifacts.py
python3 consonance/harmony-linux/scripts/test_runtime_artifacts.py
```
