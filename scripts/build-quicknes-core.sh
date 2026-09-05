#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later

set -eu

script_dir=$(cd -P -- "$(dirname "$0")" && pwd)
revision=26bb785c9deddb66a17717b21bb4e328f03ade32
repository=https://github.com/libretro/QuickNES_Core.git
output=${1:-quicknes_libretro.so}
static_output=${HARMONY_QUICKNES_STATIC_OUTPUT:-}
build_root=$(mktemp -d "${TMPDIR:-/tmp}/harmony-quicknes.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

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

if [ -n "$static_output" ]; then
    # QuickNES uses only C library calls plus C++ allocation/vtable machinery.
    # Disabling exceptions and RTTI keeps the host C++ runtime out of the
    # archive. Explicit target tools prevent a native arm64 build from mixing
    # an archive from one target with a runtime from another.
    static_cxx=${HARMONY_QUICKNES_CXX:-${CXX:-c++}}
    static_cc=${HARMONY_QUICKNES_CC:-${CC:-cc}}
    static_ar=${HARMONY_QUICKNES_AR:-${AR:-ar}}
    static_target_flags=${HARMONY_QUICKNES_CXXFLAGS:-${CXXFLAGS:-}}
    static_cxxflags="$static_target_flags -fno-exceptions -fno-rtti -fno-use-cxa-atexit"
    for static_tool in "$static_cc" "$static_cxx" "$static_ar"; do
        command -v "$static_tool" >/dev/null 2>&1 || {
            echo "FAIL: QuickNES static tool is not executable: $static_tool" >&2
            exit 1
        }
    done
    make -C "$build_root/core" clean >/dev/null

    # The pinned core has three C++-only header uses. The patched-musl wrapper
    # intentionally has no libstdc++ headers, so rewrite these exact sites to
    # their libc equivalents. This patch is fail-closed: a source move at the
    # pinned revision stops the build instead of silently changing the core.
    patch -d "$build_root/core" -f -p1 <<'PATCH'
diff --git a/nes_emu/Nes_Mapper.cpp b/nes_emu/Nes_Mapper.cpp
--- a/nes_emu/Nes_Mapper.cpp
+++ b/nes_emu/Nes_Mapper.cpp
@@ -6 +6 @@
-#include <cstdio>
+#include <stdio.h>
diff --git a/nes_emu/mappers/mapper009.hpp b/nes_emu/mappers/mapper009.hpp
--- a/nes_emu/mappers/mapper009.hpp
+++ b/nes_emu/mappers/mapper009.hpp
@@ -3 +3 @@
-#include <cstring>
+#include <string.h>
@@ -30 +30 @@
-		std::memset(regs, 0, sizeof(regs));
+		memset(regs, 0, sizeof(regs));
diff --git a/nes_emu/mappers/mapper010.hpp b/nes_emu/mappers/mapper010.hpp
--- a/nes_emu/mappers/mapper010.hpp
+++ b/nes_emu/mappers/mapper010.hpp
@@ -2 +2 @@
-#include <cstring>
+#include <string.h>
@@ -29 +29 @@
-		std::memset(regs, 0, sizeof(regs));
+		memset(regs, 0, sizeof(regs));
PATCH
    CXXFLAGS="$static_cxxflags" CC="$static_cc" CXX="$static_cxx" AR="$static_ar" \
        make -C "$build_root/core" -j "${HARMONY_QUICKNES_BUILD_JOBS:-4}" \
        DEBUG=0 OPTIMIZE=-O2 GIT_VERSION="$revision" \
        STATIC_LINKING=1 TARGET=libquicknes_libretro.a

    # The pinned core needs only allocation and pure-virtual failure hooks from
    # the C++ ABI. The shim uses the target libc and carries no host libstdc++.
    static_runtime_source=$script_dir/quicknes-static-runtime.cpp
    static_runtime_object=$build_root/quicknes-static-runtime.o
    [ -f "$static_runtime_source" ] || {
        echo "FAIL: QuickNES static runtime source is missing: $static_runtime_source" >&2
        exit 1
    }
    # Build variables intentionally contain a trusted whitespace-separated
    # compiler flag list, matching make's CXXFLAGS convention.
    # shellcheck disable=SC2086
    "$static_cxx" -x c++ $static_target_flags -O2 -fno-stack-protector \
        -fno-exceptions -fno-rtti -fno-use-cxa-atexit \
        -c "$static_runtime_source" -o "$static_runtime_object"
    "$static_ar" rcs "$build_root/core/libquicknes_libretro.a" "$static_runtime_object"
    cp "$build_root/core/libquicknes_libretro.a" "$static_output"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$static_output"
    else
        shasum -a 256 "$static_output"
    fi
fi
