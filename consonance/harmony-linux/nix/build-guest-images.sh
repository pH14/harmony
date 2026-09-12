#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build Harmony's native guest images from a locked Nix closure. Linux/aarch64
# produces the minimal and PostgreSQL guests; Linux/x86_64 produces the
# minimal guest used by the x86 virtual-time reference and the fault-library
# kernel profile that runs stock userspace binaries. Nix supplies every
# tool and source tarball. The application performs assembly in a fresh
# external workspace.
set -euo pipefail

usage() {
    echo "usage: harmony-build-guest-images --output DIR [--minimal-only] [--mutate-cache-line] [--serialization-gate] [--n6]" >&2
    exit 2
}

output=
minimal_only=0
mutate_cache_line=0
serialization_gate=0
n6=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            [ "$#" -ge 2 ] || usage
            output=$2
            shift 2
            ;;
        --minimal-only)
            minimal_only=1
            shift
            ;;
        --mutate-cache-line)
            mutate_cache_line=1
            shift
            ;;
        --serialization-gate)
            serialization_gate=1
            shift
            ;;
        --n6)
            n6=1
            shift
            ;;
        *) usage ;;
    esac
done

[ -n "$output" ] || usage
[ "$(uname -s)" = Linux ] || {
    echo "FAIL: the lock build requires native Linux" >&2
    exit 1
}
host_arch=$(uname -m)
case "$host_arch" in
    aarch64)
        [ "$serialization_gate" -eq 0 ] || {
            echo "FAIL: --serialization-gate is x86_64-only" >&2
            exit 1
        }
        [ "$(id -u)" -eq 0 ] || {
            echo "FAIL: the PostgreSQL snapshot build requires root" >&2
            exit 1
        }
        ;;
    x86_64)
        [ "$minimal_only" -eq 0 ] || {
            echo "FAIL: --minimal-only is implicit on Linux/x86_64" >&2
            exit 1
        }
        ;;
    *)
        echo "FAIL: unsupported native architecture: $host_arch" >&2
        exit 1
        ;;
esac

if [ -e "$output" ] && [ -n "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]; then
    echo "FAIL: output directory is not empty: $output" >&2
    exit 1
fi
mkdir -p "$output"
output=$(cd "$output" && pwd)

if [ "$host_arch" = aarch64 ]; then
    # The compiler embeds compilation identity in its output. A randomized
    # absolute source root changes that identity even when diagnostics are
    # remapped, so native ARM builds use one stable, freshly-created path.
    # A stale or concurrent workspace fails closed instead of being reused.
    work=/build/harmony-nix-guest
    [ ! -e "$work" ] || {
        echo "FAIL: stable ARM build workspace already exists: $work" >&2
        exit 1
    }
    mkdir -p "$work"
else
    work=$(mktemp -d "${TMPDIR:-/tmp}/harmony-nix-guest.XXXXXXXX")
fi
cleanup() {
    if [ "${HARMONY_NIX_KEEP_WORK:-0}" -eq 1 ]; then
        echo "N5 diagnostic workspace preserved: $work" >&2
    else
        rm -rf "$work"
    fi
}
trap cleanup EXIT HUP INT TERM

repo=$work/repo
downloads=$work/downloads
artifacts=$work/artifacts
build_root=$work/build
mkdir -p "$repo" "$downloads" "$artifacts" "$build_root"
cp -a "$HARMONY_NIX_SOURCE/." "$repo/"
chmod -R u+w "$repo"

install -m 0644 "$HARMONY_NIX_LINUX_SOURCE" \
    "$downloads/linux-6.18.35.tar.xz"
install -m 0644 "$HARMONY_NIX_BUSYBOX_SOURCE" \
    "$downloads/busybox-1.38.0.tar.bz2"
if [ "$host_arch" = aarch64 ]; then
    install -m 0644 "$HARMONY_NIX_MUSL_SOURCE" \
        "$downloads/musl-1.2.6.tar.gz"
    install -m 0644 "$HARMONY_NIX_POSTGRES_SOURCE" \
        "$downloads/postgresql-17.10.tar.bz2"
fi

export MAKEFLAGS=-j4
export SOURCE_DATE_EPOCH=0
export TZ=UTC
export LC_ALL=C
export HARMONY_DOWNLOAD_DIR=$downloads
export HARMONY_ARTIFACT_DIR=$artifacts
export GUEST_BUILD_ROOT=$build_root
export HARMONY_BUILD_PATH_PREFIX=$work

guest=$repo/consonance/harmony-linux
linux_dir=$guest/linux

if [ "$mutate_cache_line" -eq 1 ]; then
    if [ "$host_arch" = aarch64 ]; then
        mutant=$linux_dir/patches/arm64/0009-arm64-harmony-fixed-cache-topology.patch
    else
        mutant=$linux_dir/patches/x86/0001-x86-harmony-pvclock-exit-count-clocksource.patch
    fi
    python3 - "$mutant" "$host_arch" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
if sys.argv[2] == "aarch64":
    before = b"+\treturn 64;"
    after = b"+\treturn 65;"
else:
    before = b'+#define pr_fmt(fmt) "harmony_pvclock: " fmt'
    after = b'+#define pr_fmt(fmt) "harmony_pvclock; " fmt'
data = path.read_bytes()
if data.count(before) != 1:
    raise SystemExit("FAIL: patch mutant anchor is not unique")
path.write_bytes(data.replace(before, after))
PY
    echo "== N5 negative control: changed one patch byte"
fi

stage=$work/stage
if [ "$host_arch" = aarch64 ]; then
    echo "== N5: build minimal ARM kernel and initramfs"
    (cd "$linux_dir" && ./build-arm64-kernel.sh && ./build-arm64-initramfs.sh)
    if [ "$n6" -eq 1 ]; then
        echo "== N6: build owned musl for the generated sweep"
        (
            cd "$linux_dir"
            # shellcheck source=../linux/lib-build.sh disable=SC1091
            . ./lib-build.sh
            build_arm64_game_musl
        )
        echo "== N6: build generated sweep and traps-off ARM kernel"
        (cd "$linux_dir" && \
            HARMONY_N6_CC="$build_root/musl-arm64-game-prefix/bin/musl-gcc" \
                ./build-n6-instruction-images.sh && \
            ARM64_KERNEL_PROFILE=n6-traps-off ./build-arm64-kernel.sh)
    fi

    if [ "$minimal_only" -eq 0 ]; then
        echo "== N5: build PostgreSQL kernel, initramfs, and payloads"
        (cd "$linux_dir" && ARM64_KERNEL_PROFILE=postgres ./build-arm64-kernel.sh) && \
            "$repo/workloads/guest-images/build-arm64-postgres-image.sh"
    fi

    mkdir -p "$stage/arm64"
    if [ "$minimal_only" -eq 1 ]; then
        names=(Image initramfs.cpio.gz)
    else
        names=(
            Image
            initramfs.cpio.gz
            Image-postgres
            initramfs-postgres.cpio.gz
            postgres
            psql
            pg_ctl
        )
    fi
    for name in "${names[@]}"; do
        [ -f "$artifacts/arm64/$name" ] || {
            echo "FAIL: lock build did not produce arm64/$name" >&2
            exit 1
        }
        cp -p "$artifacts/arm64/$name" "$stage/arm64/$name"
    done
    if [ "$n6" -eq 1 ]; then
        for name in Image-n6-traps-off initramfs-n6.cpio.gz initramfs-n6-traps-off.cpio.gz; do
            [ -f "$artifacts/arm64/$name" ] || {
                echo "FAIL: N6 lock build did not produce arm64/$name" >&2
                exit 1
            }
            cp -p "$artifacts/arm64/$name" "$stage/arm64/$name"
        done
    fi
else
    echo "== N5: build minimal x86 kernel and initramfs"
    (cd "$linux_dir" && ./build-kernel.sh && ./build-initramfs.sh && ./build-go-runtime-image.sh)
    if [ "$n6" -eq 1 ]; then
        echo "== N6: build generated sweep and traps-off x86 kernel"
        (cd "$linux_dir" && \
            ./build-n6-instruction-images.sh && \
            N6_TRAPS_OFF=1 ./build-kernel.sh)
    fi
    if [ "$serialization_gate" -eq 1 ]; then
        echo "== N5: run /dev/harmony serialization positive and negative control"
        (cd "$linux_dir" && ./test-harmony-serialization.sh)
    fi
    # The fault-library profile: the same series and pinned source, built
    # with the task park enabled. Both production profiles emulate ring-3
    # counter reads from the same virtual clock. It carries its own reviewed
    # counter-opcode baseline because its call sites sit at different offsets. Built after
    # everything above: the other profiles keep the object-directory sequence
    # they were reproduced under, and the serialization test seeds its KUnit
    # kernels from that shared object directory's configuration, which a
    # concurrency test needs left multiprocessor.
    echo "== N5: build the fault-library x86 kernel profile"
    (cd "$linux_dir" && FAULTLAB=1 ./build-kernel.sh)
    mkdir -p "$stage/x86_64"
    for name in bzImage bzImage-faultlab initramfs.cpio.gz initramfs-go-runtime.cpio.gz; do
        [ -f "$artifacts/$name" ] || {
            echo "FAIL: lock build did not produce x86_64/$name" >&2
            exit 1
        }
        cp -p "$artifacts/$name" "$stage/x86_64/$name"
    done
    if [ "$n6" -eq 1 ]; then
        for name in bzImage-n6-traps-off initramfs-n6.cpio.gz initramfs-n6-traps-off.cpio.gz; do
            [ -f "$artifacts/$name" ] || {
                echo "FAIL: N6 lock build did not produce x86_64/$name" >&2
                exit 1
            }
            cp -p "$artifacts/$name" "$stage/x86_64/$name"
        done
    fi
fi

# Compiler diagnostics and panic locations are part of the shipped byte stream.
# Reject an artifact if the external workspace escaped the compiler prefix maps,
# rather than silently blessing a host-specific manifest.
while IFS= read -r -d '' artifact; do
    if grep -aFq "$work" "$artifact"; then
        echo "FAIL: artifact embeds external build path: ${artifact#"$stage/"}" >&2
        exit 1
    fi
done < <(find "$stage" -mindepth 2 -type f -print0)

(
    cd "$stage"
    find . -mindepth 2 -type f -print0 | LC_ALL=C sort -z \
        | sed -z 's#^\./##' | xargs -0 sha256sum \
        >MANIFEST.sha256
)
cp -a "$stage/." "$output/"
echo "PASS: Nix-locked guest images built offline"
cat "$output/MANIFEST.sha256"
