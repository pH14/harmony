# Platform scripts

The instruction scanners reject unsupported instructions in shipped guest
executables. Their planted negative controls must continue to fail.

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
