#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch, verify, and source-build the pinned Thwaite ROM.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)

# shellcheck disable=SC1091
. "$repo_root/workloads/nes/thwaite-versions.env"

output_dir=${1:-"$repo_root/workloads/nes/build/thwaite"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)

for tool in git sha256sum make ca65 ld65 python3; do
    command -v "$tool" >/dev/null || {
        echo "missing required tool: $tool" >&2
        exit 1
    }
done
python3 -c 'from PIL import Image' || {
    echo "Thwaite tile conversion needs Pillow for python3" >&2
    exit 1
}

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/dissonance-thwaite.XXXXXX")
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

source_dir="$work_dir/source"
git init --quiet "$source_dir"
git -C "$source_dir" fetch --quiet --depth 1 "$THWAITE_REPOSITORY" "$THWAITE_COMMIT"
git -C "$source_dir" checkout --quiet --detach FETCH_HEAD
test "$(git -C "$source_dir" rev-parse HEAD)" = "$THWAITE_COMMIT"

# Upstream tile conversion calls the interpreter as python3 and needs no
# generated timestamps, so the pinned source builds the same ROM every day.
make -C "$source_dir" -j "${HARMONY_THWAITE_BUILD_JOBS:-2}" thwaite.nes

printf '%s  %s\n' "$THWAITE_ROM_SHA256" "$source_dir/thwaite.nes" \
    | sha256sum --check --status || {
        echo "source-built Thwaite ROM checksum mismatch (pinned cc65 output expected)" >&2
        exit 1
    }

verify_symbol() {
    local name=$1
    local value=$2
    grep -Eq "name=\"${name}\",addrsize=[a-z]+,(size=[0-9]+,)?scope=[0-9]+,def=[^,]*,(ref=[^,]*,)?val=${value}(,|$)" \
        "$source_dir/thwaite.dbg" || {
        echo "Thwaite symbol drift: $name is not $value" >&2
        exit 1
    }
}

verify_symbol gameState 0x38
verify_symbol numPlayers 0x39
verify_symbol isPractice 0x4F
verify_symbol enemyMissilesLeft 0x305
verify_symbol housesStanding 0x3CC
verify_symbol buildingsDestroyedThisLevel 0x3D8
verify_symbol score100s 0x3D9
verify_symbol score1s 0x3DA
verify_symbol gameDay 0x3DD
verify_symbol gameHour 0x3DE
verify_symbol gameMinute 0x3DF
verify_symbol gameSecond 0x3E0
verify_symbol siloMissilesLeft 0x3E6
verify_symbol missileYHi 0x412
verify_symbol crosshairXHi 0x4E0
verify_symbol crosshairYHi 0x4E4

install -m 0644 "$source_dir/thwaite.nes" "$output_dir/thwaite.nes"
install -m 0644 "$source_dir/thwaite.dbg" "$output_dir/thwaite.debug.dbg"
install -m 0644 "$source_dir/LICENSE.txt" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$source_dir/README.md" "$output_dir/UPSTREAM-README.md"
install -m 0644 "$repo_root/workloads/nes/thwaite-versions.env" "$output_dir/thwaite-versions.env"
printf '%s\n' "$THWAITE_COMMIT" >"$output_dir/thwaite.commit"
(cd "$output_dir" && sha256sum thwaite.nes >SHA256SUMS)
echo "Thwaite ROM ready: $output_dir/thwaite.nes"
