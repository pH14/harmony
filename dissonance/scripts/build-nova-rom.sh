#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch, verify, and source-build the pinned Nova ROM.
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../.." && pwd)

# shellcheck disable=SC1091
. "$repo_root/dissonance/nova-versions.env"

output_dir=${1:-"$repo_root/dissonance/nova-build"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)

for tool in curl sha256sum tar ca65 ld65 python3; do
    command -v "$tool" >/dev/null || {
        echo "missing required tool: $tool" >&2
        exit 1
    }
done

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/dissonance-nova.XXXXXX")
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

archive="$work_dir/nova.tar.gz"
curl --fail --location --retry 3 --output "$archive.part" "$NOVA_URL"
printf '%s  %s\n' "$NOVA_SHA256" "$archive.part" | sha256sum --check --status || {
    echo "Nova archive checksum mismatch" >&2
    exit 1
}
mv "$archive.part" "$archive"
tar -xzf "$archive" -C "$work_dir"
source_dir="$work_dir/NovaTheSquirrel-$NOVA_COMMIT"

# Nova displays a project-day value computed from ca65's host-clock `.TIME`.
# Replace that single expression before assembly so the pinned source produces
# the same ROM on every calendar day and still fails closed if the source drifts.
python3 - "$source_dir/src/options.s" "$NOVA_BUILD_EPOCH" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
source = path.read_text()
needle = "(.TIME / 86400) - 16611"
replacement = f"({sys.argv[2]} / 86400) - 16611"
if source.count(needle) != 1:
    raise SystemExit("Nova project-day source expression drifted")
path.write_text(source.replace(needle, replacement))
PY

(
    cd "$source_dir"
    ca65 src/nova.s -o src/nova.o -l nova.lst -g
    ld65 -C src/nova.x src/nova.o -o nova.nes -m map.txt --dbgfile debug.dbg
)
printf '%s  %s\n' "$NOVA_ROM_SHA256" "$source_dir/nova.nes" \
    | sha256sum --check --status || {
        echo "source-built Nova ROM checksum mismatch" >&2
        exit 1
    }

verify_symbol() {
    local name=$1
    local value=$2
    grep -Eq "name=\"${name}\".*val=${value}(,|$)" "$source_dir/debug.dbg" || {
        echo "Nova symbol drift: $name is not $value" >&2
        exit 1
    }
}

verify_symbol PlayerPXL 0x25
verify_symbol PlayerPXH 0x26
verify_symbol PlayerPYH 0x27
verify_symbol PlayerPYL 0x28
verify_symbol PlayerHealth 0x4B
verify_symbol LevelNumber 0xA7
verify_symbol StartedLevelNumber 0xA8
verify_symbol NeedLevelReload 0xA9
verify_symbol ChipCount 0x508
verify_symbol ChipsNeeded 0x509
verify_symbol PlayerAbility 0x7200
verify_symbol LevelCleared 0x7F1F
verify_symbol LevelAvailable 0x7F27
verify_symbol CollectibleBits 0x7F2F

install -m 0644 "$source_dir/nova.nes" "$output_dir/nova.nes"
install -m 0644 "$source_dir/debug.dbg" "$output_dir/nova.debug.dbg"
printf '%s\n' "$NOVA_COMMIT" >"$output_dir/nova.commit"
sha256sum "$output_dir/nova.nes" >"$output_dir/SHA256SUMS"
echo "Nova ROM ready: $output_dir/nova.nes"
