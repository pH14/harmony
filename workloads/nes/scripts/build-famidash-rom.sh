#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch the pinned Famidash release ROMs with the license and music credits.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)

# shellcheck disable=SC1091
. "$repo_root/workloads/nes/famidash-versions.env"

output_dir=${1:-"$repo_root/workloads/nes/build/famidash"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)

for tool in curl sha256sum cut; do
    command -v "$tool" >/dev/null || {
        echo "missing required tool: $tool" >&2
        exit 1
    }
done

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/harmony-famidash.XXXXXX")
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

fetch() {
    curl --fail --location --retry 3 --output "$work_dir/$3" "$1"
    [ "$(sha256sum <"$work_dir/$3" | cut -d ' ' -f 1)" = "$2" ] || {
        echo "Famidash $3 checksum mismatch" >&2
        exit 1
    }
}

fetch "$FAMIDASH_ROM_URL" "$FAMIDASH_ROM_SHA256" famidash.nes
fetch "$FAMIDASH_B_SIDES_ROM_URL" "$FAMIDASH_B_SIDES_ROM_SHA256" famidash-b-sides.nes
fetch "$FAMIDASH_C_SIDES_ROM_URL" "$FAMIDASH_C_SIDES_ROM_SHA256" famidash-c-sides.nes
fetch "$FAMIDASH_D_SIDES_ROM_URL" "$FAMIDASH_D_SIDES_ROM_SHA256" famidash-d-sides.nes
fetch "$FAMIDASH_LICENSE_URL" "$FAMIDASH_LICENSE_SHA256" LICENSE
fetch "$FAMIDASH_CREDITS_URL" "$FAMIDASH_CREDITS_SHA256" CREDITS.md
fetch "$FAMIDASH_MUSIC_URL/lvlset_A_metadata.json5" "$FAMIDASH_MUSIC_A_SHA256" music-main.json5
fetch "$FAMIDASH_MUSIC_URL/lvlset_B_metadata.json5" "$FAMIDASH_MUSIC_B_SHA256" music-b-sides.json5
fetch "$FAMIDASH_MUSIC_URL/lvlset_C_metadata.json5" "$FAMIDASH_MUSIC_C_SHA256" music-c-sides.json5
fetch "$FAMIDASH_MUSIC_URL/lvlset_D_metadata.json5" "$FAMIDASH_MUSIC_D_SHA256" music-d-sides.json5

for rom in famidash.nes famidash-b-sides.nes famidash-c-sides.nes famidash-d-sides.nes; do
    install -m 0644 "$work_dir/$rom" "$output_dir/$rom"
done
install -m 0644 "$work_dir/LICENSE" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$work_dir/CREDITS.md" "$output_dir/UPSTREAM-CREDITS.md"
for music in music-main music-b-sides music-c-sides music-d-sides; do
    install -m 0644 "$work_dir/$music.json5" "$output_dir/UPSTREAM-$music.json5"
done
printf '%s\n' "$FAMIDASH_COMMIT" >"$output_dir/famidash.commit"
(cd "$output_dir" && sha256sum famidash.nes famidash-b-sides.nes famidash-c-sides.nes famidash-d-sides.nes >SHA256SUMS)
echo "Famidash ROMs ready: $output_dir"
