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

### Packaging the Nix platform without rebuilding its kernel

`build-platform-runtime.sh --from-nix DIR` consumes a completed native Nix
`--oci-runtime` output. The destination architecture directory must not exist.
It verifies the copied-source digest and every recorded output digest before
building only `runtime-fixture`, preserves the exact kernel, OCI archive and direct Linux `initramfs.cpio.gz`,
and emits the canonical `runtime-manifest.json` used by PR consumers. The
original Nix build manifest, source/output provenance and OCI runtime manifest
are retained under `build-provenance/` and included in the canonical seal.

The Nix builder records `nix-build-provenance.json` from its actual copied source
and rejects source changes during the build. `nix-runtime-artifacts.py` provides
the internal record/verify/package operations; record is a trusted builder
operation, not a way to qualify an old artifact. Outputs predating this record
are rejected. A source digest is build provenance, not reviewed instruction
admission; controlled workloads still require their exact admission baselines.
Normal kernel builds and the existing ARM build path remain available.

Run `python3 consonance/harmony-linux/scripts/test_nix_runtime_artifacts.py`
and `test_runtime_artifacts.py` to check successful packaging, stale source,
changed bytes, missing provenance and symlink rejection.

The `--from-nix` path requires the direct Linux fixture; missing bytes make the
input unavailable. It never substitutes the OCI archive or reconstructs an old
fixture. The canonical runtime manifest binds the direct fixture when present,
so consumers can verify and use this producer for snapshot consistency tests.

The Nix OCI assembler also requires source-bound runtime payloads. Build these
once using the canonical pinned Rust/remap settings, then pass all three paths:

```sh
consonance/harmony-linux/scripts/build-platform-runtime.sh --runtime-payloads "$PWD/runtime-payloads"
export HARMONY_NIX_RUNTIME_INIT="$PWD/runtime-payloads/init.sh"
export HARMONY_NIX_RUNTIME_SUPERVISOR="$PWD/runtime-payloads/harmony-supervisor"
export HARMONY_NIX_RUNTIME_MANIFEST="$PWD/runtime-payloads/runtime-payloads.json"
nix run .#guest-images -- --oci-runtime --output "$PWD/nix-guest-output"
consonance/harmony-linux/scripts/build-platform-runtime.sh --from-nix "$PWD/nix-guest-output"
```

`--runtime-payloads OUTPUT` requires a new output directory and builds only the
supervisor, copying `runtime/init.sh` from source. Its portable manifest binds
architecture, pinned toolchain/target, source digest and both payload hashes.
Nix checks the manifest against its actual copied source and supplied bytes
before and after assembly, and retains it in the output. Packaging retains this
manifest in `build-provenance/`. Arbitrary environment-supplied payloads without
matching provenance are rejected. These records rely on the trusted builder;
they are not signatures or automatic instruction-admission approvals.
