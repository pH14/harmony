#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build platform Linux artifacts from a locked Nix closure. This entrypoint
# owns only the kernels, direct platform fixtures, and optional OCI runtime;
# application packages have their own builders under workloads/.
set -euo pipefail

usage() {
    echo "usage: harmony-build-guest-images --output DIR [--minimal-only] [--mutate-cache-line] [--serialization-gate] [--n6] [--oci-runtime]" >&2
    exit 2
}

output=
minimal_only=0
mutate_cache_line=0
serialization_gate=0
n6=0
oci_runtime=0
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
        --oci-runtime)
            oci_runtime=1
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
        echo "platform diagnostic workspace preserved: $work" >&2
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

install -m 0644 "$HARMONY_NIX_LINUX_SOURCE" "$downloads/linux-6.18.35.tar.xz"
install -m 0644 "$HARMONY_NIX_BUSYBOX_SOURCE" "$downloads/busybox-1.38.0.tar.bz2"
install -m 0644 "$HARMONY_NIX_MUSL_SOURCE" "$downloads/musl-1.2.6.tar.gz"
if [ "$oci_runtime" -eq 1 ]; then
    : "${HARMONY_NIX_RUNC_SOURCE:?--oci-runtime requires HARMONY_NIX_RUNC_SOURCE}"
    install -m 0644 "$HARMONY_NIX_RUNC_SOURCE" "$downloads/v1.5.0.tar.gz"
    if [ "$host_arch" = aarch64 ]; then
        : "${HARMONY_NIX_GO_ARM_BOOTSTRAP:?--oci-runtime requires HARMONY_NIX_GO_ARM_BOOTSTRAP}"
        install -m 0644 "$HARMONY_NIX_GO_ARM_BOOTSTRAP" "$downloads/go1.25.0.linux-arm64.tar.gz"
    else
        : "${HARMONY_NIX_GO_X86_BOOTSTRAP:?--oci-runtime requires HARMONY_NIX_GO_X86_BOOTSTRAP}"
        install -m 0644 "$HARMONY_NIX_GO_X86_BOOTSTRAP" "$downloads/go1.25.0.linux-amd64.tar.gz"
    fi
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
    echo "== platform negative control: changed one patch byte"
fi

if [ "$oci_runtime" -eq 1 ]; then
    : "${HARMONY_NIX_RUNTIME_MANIFEST:?--oci-runtime requires HARMONY_NIX_RUNTIME_MANIFEST}"
    python3 "$guest/scripts/nix-runtime-artifacts.py" payload-verify --repo "$repo" \
        --input "$HARMONY_NIX_RUNTIME_MANIFEST" --architecture "$host_arch" \
        --init "$HARMONY_NIX_RUNTIME_INIT" --supervisor "$HARMONY_NIX_RUNTIME_SUPERVISOR"
fi

build_source_digest=$(python3 "$guest/scripts/runtime-artifacts.py" source-key --repo "$repo" --architecture "$host_arch")

if [ "$host_arch" = aarch64 ]; then
    echo "== platform: build standard ARM kernel and fixture initramfs"
    (cd "$linux_dir" && ./build-arm64-kernel.sh && ./build-arm64-initramfs.sh)
    if [ "$oci_runtime" -eq 1 ]; then
        : "${HARMONY_NIX_RUNTIME_INIT:?--oci-runtime requires HARMONY_NIX_RUNTIME_INIT}"
        : "${HARMONY_NIX_RUNTIME_SUPERVISOR:?--oci-runtime requires HARMONY_NIX_RUNTIME_SUPERVISOR}"
        (cd "$linux_dir" && HARMONY_RUNTIME_INIT="$HARMONY_NIX_RUNTIME_INIT" \
            HARMONY_RUNTIME_SUPERVISOR="$HARMONY_NIX_RUNTIME_SUPERVISOR" \
            ./build-oci-runtime-initramfs.sh aarch64)
    fi
    if [ "$n6" -eq 1 ]; then
        echo "== platform: build generated sweep and traps-off ARM kernel"
        (
            cd "$linux_dir"
            . ./lib-build.sh
            build_arm64_musl
            HARMONY_N6_CC="$ARM64_MUSL_PREFIX/bin/musl-gcc" \
                ./build-n6-instruction-images.sh
            ARM64_KERNEL_PROFILE=n6-traps-off ./build-arm64-kernel.sh
        )
    fi
    stage=$work/stage
    mkdir -p "$stage/aarch64"
    for name in Image initramfs.cpio.gz; do
        [ -f "$artifacts/aarch64/$name" ] || {
            echo "FAIL: lock build did not produce aarch64/$name" >&2
            exit 1
        }
        cp -p "$artifacts/aarch64/$name" "$stage/aarch64/$name"
    done
    if [ "$oci_runtime" -eq 1 ]; then
        for name in initramfs-oci.cpio.gz initramfs-oci.cpio.gz.sha256 oci-runtime.manifest; do
            [ -f "$artifacts/aarch64/$name" ] || {
                echo "FAIL: OCI build did not produce aarch64/$name" >&2
                exit 1
            }
            cp -p "$artifacts/aarch64/$name" "$stage/aarch64/$name"
        done
    fi
    if [ "$n6" -eq 1 ]; then
        for name in Image-n6-traps-off initramfs-n6.cpio.gz initramfs-n6-traps-off.cpio.gz; do
            [ -f "$artifacts/aarch64/$name" ] || {
                echo "FAIL: platform negative build did not produce aarch64/$name" >&2
                exit 1
            }
            cp -p "$artifacts/aarch64/$name" "$stage/aarch64/$name"
        done
    fi
else
    echo "== platform: build standard x86 kernel and fixture images"
    (cd "$linux_dir" && ./build-kernel.sh && ./build-initramfs.sh && ./build-go-runtime-image.sh)
    if [ "$oci_runtime" -eq 1 ]; then
        : "${HARMONY_NIX_RUNTIME_INIT:?--oci-runtime requires HARMONY_NIX_RUNTIME_INIT}"
        : "${HARMONY_NIX_RUNTIME_SUPERVISOR:?--oci-runtime requires HARMONY_NIX_RUNTIME_SUPERVISOR}"
        (cd "$linux_dir" && HARMONY_RUNTIME_INIT="$HARMONY_NIX_RUNTIME_INIT" \
            HARMONY_RUNTIME_SUPERVISOR="$HARMONY_NIX_RUNTIME_SUPERVISOR" \
            ./build-oci-runtime-initramfs.sh x86_64)
    fi
    if [ "$n6" -eq 1 ]; then
        echo "== platform: build generated sweep and traps-off x86 kernel"
        (cd "$linux_dir" && ./build-n6-instruction-images.sh && N6_TRAPS_OFF=1 ./build-kernel.sh)
    fi
    if [ "$serialization_gate" -eq 1 ]; then
        echo "== platform: run /dev/harmony serialization checks"
        (cd "$linux_dir" && ./test-harmony-serialization.sh)
    fi
    echo "== platform: build the park-enabled x86 test profile"
    (cd "$linux_dir" && TASK_PARK_PROFILE=1 ./build-kernel.sh)
    stage=$work/stage
    mkdir -p "$stage/x86_64"
    for name in bzImage bzImage.vmlinux bzImage-task-park initramfs.cpio.gz initramfs-go-runtime.cpio.gz; do
        [ -f "$artifacts/x86_64/$name" ] || [ -f "$artifacts/$name" ] || {
            echo "FAIL: lock build did not produce x86_64/$name" >&2
            exit 1
        }
        if [ -f "$artifacts/x86_64/$name" ]; then
            cp -p "$artifacts/x86_64/$name" "$stage/x86_64/$name"
        else
            cp -p "$artifacts/$name" "$stage/x86_64/$name"
        fi
    done
    if [ "$oci_runtime" -eq 1 ]; then
        for name in initramfs-oci.cpio.gz initramfs-oci.cpio.gz.sha256 oci-runtime.manifest; do
            [ -f "$artifacts/x86_64/$name" ] || {
                echo "FAIL: OCI build did not produce x86_64/$name" >&2
                exit 1
            }
            cp -p "$artifacts/x86_64/$name" "$stage/x86_64/$name"
        done
    fi
    if [ "$n6" -eq 1 ]; then
        for name in bzImage-n6-traps-off initramfs-n6.cpio.gz initramfs-n6-traps-off.cpio.gz; do
            [ -f "$artifacts/x86_64/$name" ] || [ -f "$artifacts/$name" ] || {
                echo "FAIL: platform negative build did not produce x86_64/$name" >&2
                exit 1
            }
            if [ -f "$artifacts/x86_64/$name" ]; then
                cp -p "$artifacts/x86_64/$name" "$stage/x86_64/$name"
            else
                cp -p "$artifacts/$name" "$stage/x86_64/$name"
            fi
        done
    fi
fi

while IFS= read -r -d '' artifact; do
    if grep -aFq "$work" "$artifact"; then
        echo "FAIL: artifact embeds external build path: ${artifact#"$stage/"}" >&2
        exit 1
    fi
done < <(find "$stage" -mindepth 2 -type f -print0)

(
    cd "$stage"
    find . -mindepth 2 -type f -print0 | LC_ALL=C sort -z \
        | sed -z 's#^\./##' | xargs -0 sha256sum >MANIFEST.sha256
)
if [ "$oci_runtime" -eq 1 ]; then
    [ "$build_source_digest" = "$(python3 "$guest/scripts/runtime-artifacts.py" source-key --repo "$repo" --architecture "$host_arch")" ] || {
        echo "FAIL: copied platform source changed during build" >&2
        exit 1
    }
    python3 "$guest/scripts/nix-runtime-artifacts.py" payload-verify --repo "$repo" \
        --input "$HARMONY_NIX_RUNTIME_MANIFEST" --architecture "$host_arch" \
        --init "$HARMONY_NIX_RUNTIME_INIT" --supervisor "$HARMONY_NIX_RUNTIME_SUPERVISOR"
    cp "$HARMONY_NIX_RUNTIME_MANIFEST" "$stage/runtime-payloads.json"
    python3 "$guest/scripts/nix-runtime-artifacts.py" record --repo "$repo" \
        --input "$stage" --architecture "$host_arch"
fi
cp -a "$stage/." "$output/"
echo "PASS: Nix-locked platform artifacts built offline"
cat "$output/MANIFEST.sha256"
