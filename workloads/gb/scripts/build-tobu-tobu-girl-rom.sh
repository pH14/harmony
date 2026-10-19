#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/../../.." && pwd)

TOBU_COMMIT=4985d4d282aa43229f250ffa3b0d86998ebd55d3
TOBU_URL=https://github.com/SimonLarsen/tobutobugirl-dx/archive/$TOBU_COMMIT.tar.gz
TOBU_SHA256=14cf034383aff32beb2f0278f0cda07d2e0d1ef148a89b95f8cb1c2ddb291d3b

GBDK_VERSION=2.96a
GBDK_URL=https://downloads.sourceforge.net/project/gbdk/gbdk/2.96/gbdk-2.96a-i586-pc-linux2.2.tar.gz
GBDK_SHA256=37adb30a0b91e1118deeb2d5b01a80aed2d0b66feb10925b66e936ab1f138db4

SDCC_VERSION=3.8.0
SDCC_URL=https://downloads.sourceforge.net/project/sdcc/sdcc-linux-amd64/3.8.0/sdcc-3.8.0-amd64-unknown-linux2.5.tar.bz2
SDCC_SHA256=eb52f4043d72b954e9c7c9af2d114e64ddf971ad14782f9e30bb64be31d92133

GBDK_N_COMMIT=4e68b98acee9e5c88f31314d5fd1cb5ebf97f79f
GBDK_N_URL=https://github.com/andreasjhkarlsson/gbdk-n/archive/$GBDK_N_COMMIT.tar.gz
GBDK_N_SHA256=743cb791215c3261666d130ed809d9ae17f0391d01ff5cac837198e19e3fec39

JRE_VERSION=8u472-b08
JRE_URL=https://github.com/adoptium/temurin8-binaries/releases/download/jdk8u472-b08/OpenJDK8U-jre_x64_linux_hotspot_8u472b08.tar.gz
JRE_SHA256=6f7fb5fd640a0fd00837344b0920cbc4b9b9284b50e66f33789e3b250446a16e

DEBIAN_SNAPSHOT=https://snapshot.debian.org/archive/debian
LIBC6_URL=$DEBIAN_SNAPSHOT/20250805T202250Z/pool/main/g/glibc/libc6_2.41-12_amd64.deb
LIBC6_SHA256=6cff801097cd0eb2126b60e533f37afdc78456efacdf2bfae62877f00c795d0b
LIBC6_I386_URL=$DEBIAN_SNAPSHOT/20250805T202250Z/pool/main/g/glibc/libc6-i386_2.41-12_amd64.deb
LIBC6_I386_SHA256=d3b984867db9cac65d89b75c75f64347964e2837eb53e7782b4349a6c2a5eaaa
LIBSTDCXX6_URL=$DEBIAN_SNAPSHOT/20250315T204837Z/pool/main/g/gcc-14/libstdc++6_14.2.0-19_amd64.deb
LIBSTDCXX6_SHA256=ab1fa05837aa7a92aae748fd07a18a35f7d18bb4a71c4724fe2bbf0e32089de0
LIBGCC_S1_URL=$DEBIAN_SNAPSHOT/20250315T204837Z/pool/main/g/gcc-14/libgcc-s1_14.2.0-19_amd64.deb
LIBGCC_S1_SHA256=3c71917b490d1a17aed43196a2787a256ecf060526cdb20216a74bedc061b150

TOBU_ROM_SHA256=839a2a3df9785be71757bed4229474ed8aafe37576efd1d394fb61ff7d314f08

BUILD_ROOT_LENGTH=160

output_dir=${1:-"$repo_root/workloads/gb/build/tobu-tobu-girl"}
mkdir -p "$output_dir"
output_dir=$(CDPATH='' cd -- "$output_dir" && pwd)

[ "$(uname -sm)" = "Linux x86_64" ] || {
    echo "the build host must be Linux x86_64" >&2
    exit 1
}
for tool in curl sha256sum cut tar xz bzip2 make perl python3; do
    command -v "$tool" >/dev/null || {
        echo "missing required tool: $tool" >&2
        exit 1
    }
done

check_sha256() {
    [ "$(sha256sum <"$2" | cut -d ' ' -f 1)" = "$1" ]
}

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/harmony-tobu-tobu-girl.XXXXXX")
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT

work_dir=$(CDPATH='' cd -- "$work_dir" && pwd -P)
pad_length=$((BUILD_ROOT_LENGTH - ${#work_dir} - 1))
[ "$pad_length" -ge 1 ] || {
    echo "temporary directory path is longer than $((BUILD_ROOT_LENGTH - 2)) bytes: $work_dir" >&2
    exit 1
}
build_root="$work_dir/$(printf '%*s' "$pad_length" '' | tr ' ' p)"
mkdir "$build_root"

fetch() {
    local name=$1 url=$2 sha256=$3 dest="$work_dir/$4"
    curl --fail --silent --show-error --location --retry 3 --output "$dest.part" "$url"
    check_sha256 "$sha256" "$dest.part" || {
        echo "$name archive checksum mismatch" >&2
        exit 1
    }
    mv "$dest.part" "$dest"
}

fetch "Tobu Tobu Girl Deluxe" "$TOBU_URL" "$TOBU_SHA256" tobu.tar.gz
fetch "GBDK $GBDK_VERSION" "$GBDK_URL" "$GBDK_SHA256" gbdk.tar.gz
fetch "SDCC $SDCC_VERSION" "$SDCC_URL" "$SDCC_SHA256" sdcc.tar.bz2
fetch gbdk-n "$GBDK_N_URL" "$GBDK_N_SHA256" gbdk-n.tar.gz
fetch "Temurin JRE $JRE_VERSION" "$JRE_URL" "$JRE_SHA256" jre.tar.gz
fetch libc6 "$LIBC6_URL" "$LIBC6_SHA256" libc6.deb
fetch libc6-i386 "$LIBC6_I386_URL" "$LIBC6_I386_SHA256" libc6-i386.deb
fetch libstdc++6 "$LIBSTDCXX6_URL" "$LIBSTDCXX6_SHA256" libstdcxx6.deb
fetch libgcc-s1 "$LIBGCC_S1_URL" "$LIBGCC_S1_SHA256" libgcc-s1.deb

tools="$build_root/tools"
runtime="$tools/runtime"
mkdir -p "$tools/shim" "$runtime"
tar -xzf "$work_dir/gbdk.tar.gz" -C "$tools"
tar -xjf "$work_dir/sdcc.tar.bz2" -C "$tools"
tar -xzf "$work_dir/gbdk-n.tar.gz" -C "$tools"
tar -xzf "$work_dir/jre.tar.gz" -C "$tools"
tar -xzf "$work_dir/tobu.tar.gz" -C "$build_root"
source_dir="$build_root/tobutobugirl-dx-$TOBU_COMMIT"
gbdk_n="$tools/gbdk-n-$GBDK_N_COMMIT"

for deb in libc6 libc6-i386 libstdcxx6 libgcc-s1; do
    python3 -I -c '
import sys
data = open(sys.argv[1], "rb").read()
assert data[:8] == b"!<arch>\n"
pos = 8
while pos < len(data):
    name = data[pos:pos + 16].decode().strip().rstrip("/")
    size = int(data[pos + 48:pos + 58])
    if name == "data.tar.xz":
        open(sys.argv[2], "wb").write(data[pos + 60:pos + 60 + size])
        sys.exit(0)
    pos += 60 + size + (size & 1)
sys.exit("no data.tar.xz in " + sys.argv[1])
' "$work_dir/$deb.deb" "$work_dir/$deb.tar.xz"
    tar -xJf "$work_dir/$deb.tar.xz" -C "$runtime"
done

wrap() {
    local loader=$1 libraries=$2 program=$3 elf_dir=$4
    mkdir -p "$elf_dir"
    mv "$program" "$elf_dir/"
    printf '#!/bin/sh\nexec %q --library-path %q %q "$@"\n' \
        "$loader" "$libraries" "$elf_dir/$(basename -- "$program")" >"$program"
    chmod +x "$program"
}
for program in "$tools/gbdk/bin/"*; do
    wrap "$runtime/usr/lib32/ld-linux.so.2" "$runtime/usr/lib32" "$program" "$tools/gbdk/elf"
done
sdcc_dir="$tools/sdcc-$SDCC_VERSION"
for program in sdcc sdcpp; do
    wrap "$runtime/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2" "$runtime/usr/lib/x86_64-linux-gnu" \
        "$sdcc_dir/bin/$program" "$sdcc_dir/elf-$program"
done
ln -s ../bin/sdasgb ../bin/sdcpp "$sdcc_dir/elf-sdcc/"

printf '#!/bin/sh\nexec python3 -W ignore "$@"\n' >"$tools/shim/python"
chmod +x "$tools/shim/python"

export LC_ALL=C
export PATH="$tools/shim:$tools/jdk$JRE_VERSION-jre/bin:$PATH"
export GBDKDIR="$tools/gbdk/"
sdccn="$sdcc_dir/bin/sdcc -mgbz80 --no-std-crt0 -I $gbdk_n/include -I $gbdk_n/include/asm"
sed -i 's/ data_ptrs\.o$//' "$source_dir/Makefile"

make -C "$source_dir/mmlgb/driver" ASMFILES SDCCN="$sdccn" >"$work_dir/build.log" 2>&1 || {
    cat "$work_dir/build.log" >&2
    exit 1
}
make -C "$source_dir" -j "$(nproc)" tobudx.gb \
    CC="$tools/gbdk/bin/lcc" SDCCN="$sdccn" \
    IMGTOGB=false IMGTOSGB=false GETPALETTE=false >>"$work_dir/build.log" 2>&1 || {
    cat "$work_dir/build.log" >&2
    exit 1
}

rom="$source_dir/tobudx.gb"
check_sha256 "$TOBU_ROM_SHA256" "$rom" || {
    echo "Tobu Tobu Girl Deluxe ROM checksum mismatch: $(sha256sum <"$rom" | cut -d ' ' -f 1)" >&2
    exit 1
}

install -m 0644 "$rom" "$output_dir/tobudx.gb"
install -m 0644 "$source_dir/tobudx.sym" "$output_dir/tobudx.sym"
install -m 0644 "$source_dir/LICENSE" "$output_dir/UPSTREAM-LICENSE.txt"
install -m 0644 "$source_dir/README.md" "$output_dir/UPSTREAM-README.md"
printf '%s\n' "$TOBU_COMMIT" >"$output_dir/tobudx.commit"
(cd "$output_dir" && sha256sum tobudx.gb >SHA256SUMS)
echo "Tobu Tobu Girl Deluxe ROM ready: $output_dir/tobudx.gb"
