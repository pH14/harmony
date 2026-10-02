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

# NES games spend much of each frame spinning in a wait loop until the next
# interrupt. The patch skips whole iterations of a loop that provably repeats
# with no effect, so emulated state and timing match the unpatched core while
# search runs faster. Both the shared and static builds carry it. The patch is
# fail-closed: a source move at the pinned revision stops the build.
patch -d "$build_root/core" -f -p1 <<'PATCH'
--- a/nes_emu/Nes_Cpu.cpp
+++ b/nes_emu/Nes_Cpu.cpp
@@ -113,6 +113,45 @@
 	2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6,// E
 	3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7 // F
 };
+
+// Inside run(), every memory write, I/O-range read and interrupt-flag change
+// counts as an effect. A backward jump that reaches the same target with the
+// same registers and no effect since the previous visit has entered a loop
+// whose iterations repeat exactly until the next event, so whole iterations
+// are skipped up to clock_limit.
+
+#undef READ_LIKELY_PPU
+#undef READ
+#undef WRITE
+#undef WRITE_LOW
+
+#define NOTE_READ( addr )       (effects += (unsigned) ((addr) - 0x2000) < 0x6000)
+#define READ_LIKELY_PPU( addr ) (NOTE_READ( addr ), NES_CPU_READ_PPU( this, (addr), (clock_count) ))
+#define READ( addr )            (NOTE_READ( addr ), NES_CPU_READ( this, (addr), (clock_count) ))
+#define WRITE( addr, data )     {effects++; NES_CPU_WRITE( this, (addr), (data), (clock_count) );}
+#define WRITE_LOW( addr, data ) (void) (effects++, READ_LOW( addr ) = (data))
+
+#define IDLE_CHECK()                                                    \
+{                                                                       \
+	if ( pc == idle_pc && effects == idle_effects && a == idle_a &&     \
+			x == idle_x && y == idle_y && sp == idle_sp &&              \
+			status == idle_status && c == idle_c && nz == idle_nz )     \
+	{                                                                   \
+		nes_time_t period = clock_count - idle_clock;                   \
+		if ( period > 0 && clock_count < clock_limit )                  \
+			clock_count += (clock_limit - 1 - clock_count) / period * period; \
+	}                                                                   \
+	idle_pc = pc;                                                       \
+	idle_clock = clock_count;                                           \
+	idle_effects = effects;                                             \
+	idle_a = a;                                                         \
+	idle_x = x;                                                         \
+	idle_y = y;                                                         \
+	idle_sp = sp;                                                       \
+	idle_status = status;                                               \
+	idle_c = c;                                                         \
+	idle_nz = nz;                                                       \
+}
 
 Nes_Cpu::result_t Nes_Cpu::run( nes_time_t end )
 {
@@ -168,6 +207,13 @@
 		int temp = r.status;
 		SET_STATUS( temp );
 	}
+	
+	unsigned long effects = 0;
+	unsigned long idle_effects = 0;
+	unsigned idle_pc = ~0u;
+	nes_time_t idle_clock = 0;
+	int idle_a = 0, idle_x = 0, idle_y = 0, idle_sp = 0;
+	int idle_status = 0, idle_c = 0, idle_nz = 0;
 	
 	goto loop;
 dec_clock_loop:
@@ -280,6 +326,8 @@
 	pc += offset;       \
 	pc = uint16_t( pc ); \
 	clock_count += (extra_clock >> 8) & 1;  \
+	if ( offset < 0 )   \
+		IDLE_CHECK()    \
 	goto loop;          \
 }
 
@@ -304,9 +352,13 @@
 		goto loop;
 	}
 	
-	case 0x4C: // JMP abs
+	case 0x4C: { // JMP abs
+		unsigned from = pc;
 		pc = GET_OPERAND16( pc );
+		if ( pc < from )
+			IDLE_CHECK()
 		goto loop;
+	}
 	
 	case 0xE8: INC_DEC_XY( x, 1 )  // INX
 	
@@ -813,6 +865,7 @@
 		if ( !((data ^ status) & st_i) )
 			goto loop; // I flag didn't change
 	i_flag_changed:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		// update clock_limit based on modified I flag
 		clock_limit = end_time_;
@@ -888,6 +941,7 @@
 			goto loop;
 		status &= ~st_i;
 	handle_cli:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		if ( clock_count < end_time_ )
 		{
@@ -907,6 +961,7 @@
 			goto loop;
 		status |= st_i;
 	handle_sei:
+		effects++;
 		this->r.status = status; // update externally-visible I flag
 		clock_limit = end_time_;
 		if ( clock_count < irq_time_ )
@@ -1164,6 +1219,7 @@
 			len = 3;
 		pc += len - 1;
 		error_count_++;
+		effects++;
 		goto loop;
 		
 		//result = result_badop; // intentionally unreachable; see comment above
PATCH

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
