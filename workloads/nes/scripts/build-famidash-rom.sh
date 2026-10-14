#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch the pinned Famidash release ROM with its license and music credits.
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
fetch "$FAMIDASH_LICENSE_URL" "$FAMIDASH_LICENSE_SHA256" LICENSE
fetch "$FAMIDASH_CREDITS_URL" "$FAMIDASH_CREDITS_SHA256" CREDITS.md

install -m 0644 "$work_dir/famidash.nes" "$output_dir/famidash.nes"
install -m 0644 "$work_dir/LICENSE" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$work_dir/CREDITS.md" "$output_dir/UPSTREAM-CREDITS.md"
printf '%s\n' "$FAMIDASH_COMMIT" >"$output_dir/famidash.commit"
(cd "$output_dir" && sha256sum famidash.nes >SHA256SUMS)
echo "Famidash ROM ready: $output_dir/famidash.nes"
