#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later

set -eu

revision=d9d6cd06382d1ced30de34d56d3609452323dab1
repository=https://github.com/libretro/gambatte-libretro.git
output=${1:-gambatte_libretro.so}
build_root=$(mktemp -d "${TMPDIR:-/tmp}/harmony-gambatte.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

git clone --quiet "$repository" "$build_root/core"
git -C "$build_root/core" checkout --quiet --detach "$revision"
test "$(git -C "$build_root/core" rev-parse HEAD)" = "$revision"

# HAVE_NETWORK=0 removes the serial-link listener, which would otherwise let a
# socket reach the emulated machine. The library version string the adapter
# checks depends on this setting.
make -C "$build_root/core" -j "${HARMONY_GAMBATTE_BUILD_JOBS:-4}" \
    DEBUG=0 HAVE_NETWORK=0 GIT_VERSION="$revision"

case $(uname -s) in
    Darwin) artifact=$build_root/core/gambatte_libretro.dylib ;;
    Linux)
        case $(uname -m) in
            x86_64|aarch64) artifact=$build_root/core/gambatte_libretro.so ;;
            *) echo "unsupported Gambatte build architecture: $(uname -m)" >&2; exit 1 ;;
        esac
        ;;
    *) echo "unsupported Gambatte build host" >&2; exit 1 ;;
esac

cp "$artifact" "$output"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$output"
else
    shasum -a 256 "$output"
fi
