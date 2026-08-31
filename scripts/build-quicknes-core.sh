#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later

set -eu

revision=26bb785c9deddb66a17717b21bb4e328f03ade32
repository=https://github.com/libretro/QuickNES_Core.git
output=${1:-quicknes_libretro.so}
build_root=$(mktemp -d "${TMPDIR:-/tmp}/harmony-quicknes.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

echo "QuickNES distribution gate: upstream carries mixed GPL-2.0-or-later and LGPL-2.1-or-later source plus a top-level GPL-2.0 license."
echo "This script builds an external benchmark core; do not bundle or distribute it with Harmony until licensing compatibility is reviewed."

git clone --quiet "$repository" "$build_root/core"
git -C "$build_root/core" checkout --quiet --detach "$revision"
test "$(git -C "$build_root/core" rev-parse HEAD)" = "$revision"

make -C "$build_root/core" -j "${HARMONY_QUICKNES_BUILD_JOBS:-4}" \
    DEBUG=0 OPTIMIZE=-O2 GIT_VERSION="$revision"

case $(uname -s) in
    Darwin) artifact=$build_root/core/quicknes_libretro.dylib ;;
    Linux) artifact=$build_root/core/quicknes_libretro.so ;;
    *) echo "unsupported QuickNES build host" >&2; exit 1 ;;
esac

cp "$artifact" "$output"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$output"
else
    shasum -a 256 "$output"
fi
