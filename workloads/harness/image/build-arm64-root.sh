#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eux

: "${MUSL_URL:?}" "${MUSL_SHA256:?}" "${BUSYBOX_URL:?}" "${BUSYBOX_SHA256:?}"
: "${IPROUTE2_URL:?}" "${IPROUTE2_SHA256:?}" "${IPTABLES_URL:?}" "${IPTABLES_SHA256:?}"

src=/build/harness-src
musl=/opt/harmony-musl
out=/out/harness
root=$out/root
flags='-march=armv8.1-a+lse -mno-outline-atomics'
link='-Wl,-z,separate-code'
jobs=$(nproc)

fetch() {
    name=$1
    url=$2
    sha256=$3
    curl --fail --silent --show-error --location --retry 3 --output "$src/$name.archive" "$url"
    echo "$sha256  $src/$name.archive" | sha256sum --check --strict -
    mkdir "$src/$name"
    tar -xf "$src/$name.archive" -C "$src/$name" --strip-components=1
}

rm -rf "$src" "$musl" "$out"
mkdir -p "$src" "$root"
fetch musl "$MUSL_URL" "$MUSL_SHA256"
fetch busybox "$BUSYBOX_URL" "$BUSYBOX_SHA256"
fetch iproute2 "$IPROUTE2_URL" "$IPROUTE2_SHA256"
fetch iptables "$IPTABLES_URL" "$IPTABLES_SHA256"

patch -d "$src/musl" --batch -p1 <"/build/harness-image/musl/0001-aarch64-harmony-lse-only-atomics.patch"
(
    cd "$src/musl"
    CC=cc CFLAGS="-O2 $flags" LDFLAGS="$link" ./configure --prefix="$musl" --syslibdir=/lib
    make -j"$jobs"
    make install
)
dpkg-query -L linux-libc-dev | sed -n 's#^/usr/include/\([^/]*\)$#\1#p' | while read -r headers; do
    [ "$headers" = aarch64-linux-gnu ] || cp -a "/usr/include/$headers" "$musl/include/"
done
cp -a /usr/include/aarch64-linux-gnu/asm "$musl/include/"
cc_musl=$musl/bin/harmony-cc
printf '#!/bin/sh\nexec %s %s %s "$@"\n' "$musl/bin/musl-gcc" "$flags" "$link" >"$cc_musl"
chmod 0755 "$cc_musl"

(
    cd "$src/busybox"
    make defconfig
    sed -i 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' .config
    grep -qxF '# CONFIG_TC is not set' .config
    make CC="$cc_musl" -j"$jobs" busybox
    make CC="$cc_musl" CONFIG_PREFIX="$root" install
)

(
    cd "$src/iproute2"
    PKG_CONFIG=false CC="$cc_musl" ./configure
    make CC="$cc_musl" CCOPTS='-O2 -pipe -include limits.h' -j"$jobs" SUBDIRS='lib ip tc'
)

(
    cd "$src/iptables"
    PKG_CONFIG=false CC="$cc_musl" ./configure \
        --prefix=/usr --sbindir=/sbin --disable-nftables --disable-ipv6 \
        --enable-static --disable-shared
    make -j"$jobs"
    install -m 0755 iptables/xtables-legacy-multi "$root/sbin/xtables-legacy-multi"
)

for tool in ip tc iptables iptables-save iptables-restore; do
    rm -f "$root/bin/$tool" "$root/sbin/$tool" "$root/usr/bin/$tool" "$root/usr/sbin/$tool"
done
install -m 0755 "$src/iproute2/ip/ip" "$root/sbin/ip"
install -m 0755 "$src/iproute2/tc/tc" "$root/sbin/tc"
for tool in iptables iptables-save iptables-restore; do
    ln -s xtables-legacy-multi "$root/sbin/$tool"
done

make -C /build/fault-runtime CC="$cc_musl" \
    CORE_DIR=/build/libvoidstar BUILD_DIR="$src/libvoidstar" all
/build/libvoidstar/check-symbols.sh "$src/libvoidstar/libvoidstar.so"
mkdir -p "$root/usr/lib" "$root/lib"
install -m 0755 "$src/libvoidstar/libvoidstar.so" "$root/usr/lib/libvoidstar.so"
install -m 0755 "$musl/lib/libc.so" "$root/lib/ld-musl-aarch64.so.1"

mkdir -p "$root/dev" "$root/etc" "$root/proc" "$root/root" "$root/run" "$root/sys" \
    "$root/tmp" "$root/var/tmp"
chmod 1777 "$root/tmp" "$root/var/tmp"
ln -s ../run "$root/var/run"
printf 'root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/bin/false\n' >"$root/etc/passwd"
printf 'root:x:0:\nnogroup:x:65534:\n' >"$root/etc/group"
printf '127.0.0.1 localhost\n::1 localhost\n' >"$root/etc/hosts"
for license in musl:COPYRIGHT busybox:LICENSE iproute2:COPYING iptables:COPYING; do
    mkdir -p "$root/licenses"
    cp "$src/${license%%:*}/${license#*:}" "$root/licenses/${license%%:*}.txt"
done

"$root/sbin/ip" -V
"$root/sbin/tc" -V
"$root/sbin/iptables" -V
"$root/bin/busybox" true
rm -rf "$src"
