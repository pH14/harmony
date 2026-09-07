#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned offline UNROM image; the GCC release requires Linux x86-64.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)
# shellcheck disable=SC1091
. "$repo_root/workloads/nes/stb-versions.env"

if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
    echo "STB's pinned compiler requires Linux x86-64 (or a linux/amd64 container)" >&2
    exit 1
fi
for tool in git curl sha256sum unzip tar make gcc python3; do
    command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done
python3 -c 'from PIL import Image'

output_dir=${1:-"$repo_root/workloads/nes/build/stb"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)
work_dir=$(mktemp -d "${TMPDIR:-/tmp}/dissonance-stb.XXXXXX")
cleanup() {
    if [[ -f "$work_dir/source/build.log" ]]; then
        cp "$work_dir/source/build.log" "$output_dir/build.log"
    fi
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

# Upstream tools use /usr/bin/env python. Bind that spelling to the checked
# Python 3 interpreter without depending on a distribution's optional alias.
mkdir "$work_dir/bin"
ln -s "$(command -v python3)" "$work_dir/bin/python"
export PATH="$work_dir/bin:$PATH"

checkout() {
    local repository=$1 revision=$2 destination=$3
    git init --quiet "$destination"
    git -C "$destination" fetch --quiet --depth 1 "$repository" "$revision"
    git -C "$destination" checkout --quiet --detach FETCH_HEAD
    test "$(git -C "$destination" rev-parse HEAD)" = "$revision"
}
# Upstream compile-mod.py has a broken special case for a checkout named
# exactly "game". A neutral directory name avoids it without patching source.
checkout https://github.com/sgadrat/super-tilt-bro.git "$STB_COMMIT" "$work_dir/source"
checkout https://github.com/sgadrat/xa65-stb.git "$STB_XA_COMMIT" "$work_dir/xa"

curl --fail --location --retry 3 --output "$work_dir/gcc.zip" "$STB_GCC_URL"
printf '%s  %s\n' "$STB_GCC_SHA256" "$work_dir/gcc.zip" | sha256sum --check --status
mkdir "$work_dir/compiler"
unzip -q "$work_dir/gcc.zip" -d "$work_dir/compiler"
tar -xf "$work_dir/compiler/prefix.tar" -C "$work_dir/compiler"
make -C "$work_dir/xa/xa" -j "${HARMONY_STB_BUILD_JOBS:-2}"

(
    cd "$work_dir/source"
    # Upstream's supported flag skips only the Rainbow rescue image compression.
    # The selected UNROM image is assembled before that phase; no game patch,
    # prebuilt ROM, Huffmunch, or network-enabled mapper is required.
    XA_BIN="$work_dir/xa/xa/xa" \
    CC_BIN="$work_dir/compiler/prefix/bin/6502-gcc" \
    SKIP_RESCUE_IMG=2 ./build.sh
)
rom="$work_dir/source/tilt_no_network_unrom_(E).nes"
printf '%s  %s\n' "$STB_ROM_SHA256" "$rom" | sha256sum --check --status || {
    echo "source-built STB ROM checksum mismatch" >&2
    exit 1
}
install -m 0644 "$rom" "$output_dir/stb.nes"
install -m 0644 "$work_dir/source/LICENSE" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$work_dir/source/game/data/menu_credits/credits.asm" "$output_dir/UPSTREAM-CREDITS.asm"
install -m 0644 "$repo_root/workloads/nes/stb-versions.env" "$output_dir/stb-versions.env"
(cd "$output_dir" && sha256sum stb.nes > SHA256SUMS)
echo "Super Tilt Bro ROM ready: $output_dir/stb.nes"
