#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
output=${1:?usage: build-image.sh OUTPUT}
binary=${NESTED_DRIVER_BIN:-$root/target/x86_64-unknown-linux-musl/release/nested-driver}
if [ -z "${NESTED_DRIVER_BIN:-}" ]; then
    cargo build --manifest-path "$root/Cargo.toml" --locked --release \
        --target x86_64-unknown-linux-musl -p nested-driver --bin nested-driver
fi
if readelf -l "$binary" | grep -q ' INTERP '; then
    echo 'FAIL: nested-driver must be static' >&2
    exit 1
fi
if objdump -d "$binary" | grep -E '[[:space:]](rdtsc|rdtscp|rdrand|rdseed)([[:space:]]|$)' > /dev/null; then
    echo 'FAIL: nested-driver contains a raw hardware counter or RNG instruction' >&2
    exit 1
fi
python3 - "$binary" "$output" <<'PY'
import hashlib
import io
import json
import pathlib
import sys
import tarfile

binary, output = map(pathlib.Path, sys.argv[1:])
output.mkdir(parents=True, exist_ok=False)
blobs = output / 'blobs' / 'sha256'
blobs.mkdir(parents=True)

def blob(data, media):
    digest = hashlib.sha256(data).hexdigest()
    (blobs / digest).write_bytes(data)
    return {'mediaType': media, 'digest': 'sha256:' + digest, 'size': len(data)}

def encode(data):
    return (json.dumps(data, sort_keys=True, separators=(',', ':')) + '\n').encode()

layer = io.BytesIO()
with tarfile.open(fileobj=layer, mode='w', format=tarfile.USTAR_FORMAT) as archive:
    directory = tarfile.TarInfo('app')
    directory.type = tarfile.DIRTYPE
    directory.mode = 0o755
    archive.addfile(directory)
    data = binary.read_bytes()
    entry = tarfile.TarInfo('app/nested-driver')
    entry.size = len(data)
    entry.mode = 0o755
    archive.addfile(entry, io.BytesIO(data))
layer_descriptor = blob(layer.getvalue(), 'application/vnd.oci.image.layer.v1.tar')
config = blob(encode({
    'architecture': 'amd64', 'os': 'linux',
    'config': {'Entrypoint': ['/app/nested-driver'], 'Cmd': ['--sdk'], 'User': '0:0', 'WorkingDir': '/'},
    'rootfs': {'type': 'layers', 'diff_ids': [layer_descriptor['digest']]},
}), 'application/vnd.oci.image.config.v1+json')
manifest = blob(encode({'schemaVersion': 2, 'config': config, 'layers': [layer_descriptor]}),
                'application/vnd.oci.image.manifest.v1+json')
manifest['platform'] = {'architecture': 'amd64', 'os': 'linux'}
(output / 'index.json').write_bytes(encode({'schemaVersion': 2, 'manifests': [manifest]}))
(output / 'oci-layout').write_bytes(encode({'imageLayoutVersion': '1.0.0'}))
print(output)
PY
