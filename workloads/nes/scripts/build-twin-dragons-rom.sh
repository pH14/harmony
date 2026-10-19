#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch the pinned Twin Dragons release ROM and silence its sound effects.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)

# shellcheck disable=SC1091
. "$repo_root/workloads/nes/twin-dragons-versions.env"

output_dir=${1:-"$repo_root/workloads/nes/build/twin-dragons"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)

for tool in curl sha256sum cut tar dd; do
    command -v "$tool" >/dev/null || {
        echo "missing required tool: $tool" >&2
        exit 1
    }
done

check_sha256() {
    [ "$(sha256sum <"$2" | cut -d ' ' -f 1)" = "$1" ]
}

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/harmony-twin-dragons.XXXXXX")
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

archive="$work_dir/twin-dragons.tar.gz"
curl --fail --location --retry 3 --output "$archive.part" "$TWIN_DRAGONS_URL"
check_sha256 "$TWIN_DRAGONS_SHA256" "$archive.part" || {
    echo "Twin Dragons archive checksum mismatch" >&2
    exit 1
}
mv "$archive.part" "$archive"
tar -xzf "$archive" -C "$work_dir"
source_dir="$work_dir/twin-dragons-nes-$TWIN_DRAGONS_COMMIT"
rom="$source_dir/bin/twin_dragons.nes"
check_sha256 "$TWIN_DRAGONS_UPSTREAM_ROM_SHA256" "$rom" || {
    echo "Twin Dragons upstream ROM checksum mismatch" >&2
    exit 1
}

# The upstream README reserves all rights to the sound effects, so the effect
# streams are zeroed. FamiTone ends an effect at its first zero byte, which
# keeps the effect table and every other ROM byte unchanged. The streams occupy
# CPU $ECF0-$F9F9, which is file offset 16 + $6CF0.
dd if=/dev/zero of="$rom" bs=1 seek=27904 count=3338 conv=notrunc status=none
check_sha256 "$TWIN_DRAGONS_ROM_SHA256" "$rom" || {
    echo "Twin Dragons silent-effects ROM checksum mismatch" >&2
    exit 1
}

install -m 0644 "$rom" "$output_dir/twin-dragons.nes"
install -m 0644 "$source_dir/LICENSE" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$source_dir/README.md" "$output_dir/UPSTREAM-README.md"
printf '%s\n' "$TWIN_DRAGONS_COMMIT" >"$output_dir/twin-dragons.commit"
(cd "$output_dir" && sha256sum twin-dragons.nes >SHA256SUMS)
echo "Twin Dragons ROM ready: $output_dir/twin-dragons.nes"
