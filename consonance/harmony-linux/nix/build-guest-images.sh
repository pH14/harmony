#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build Harmony's native guest images from a locked Nix closure. Linux/aarch64
# produces the minimal, NES, and PostgreSQL guests; Linux/x86_64 produces the
# minimal guest used by the x86 virtual-time reference. Nix supplies every
# tool, source tarball, and Cargo registry crate. The application performs
# assembly in a fresh external workspace with Cargo networking disabled.
set -euo pipefail

usage() {
    echo "usage: harmony-build-guest-images --output DIR [--rom FILE] [--minimal-only] [--mutate-cache-line] [--serialization-gate]" >&2
    exit 2
}

output=
rom=
minimal_only=0
mutate_cache_line=0
serialization_gate=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            [ "$#" -ge 2 ] || usage
            output=$2
            shift 2
            ;;
        --rom)
            [ "$#" -ge 2 ] || usage
            rom=$2
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
        [ -n "$rom" ] || usage
        [ "$(id -u)" -eq 0 ] || {
            echo "FAIL: the PostgreSQL snapshot build requires root" >&2
            exit 1
        }
        [ -f "$rom" ] || { echo "FAIL: ROM does not exist: $rom" >&2; exit 1; }
        rom_sha=$(sha256sum "$rom" | awk '{print $1}')
        [ "$rom_sha" = "$HARMONY_NIX_SMB_SHA256" ] || {
            echo "FAIL: ROM sha256 mismatch (want $HARMONY_NIX_SMB_SHA256, got $rom_sha)" >&2
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
    # rustc includes compilation identity in symbol hashes.  A randomized
    # absolute source root changes those hashes even when diagnostics are
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
cargo_home=$work/cargo-home
mkdir -p "$repo" "$downloads" "$artifacts" "$build_root" "$cargo_home"
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

if [ "$host_arch" = aarch64 ]; then
    cat >"$cargo_home/config.toml" <<EOF
[net]
offline = true

[source.crates-io]
replace-with = "nix-vendor"

[source.nix-vendor]
directory = "$HARMONY_NIX_CARGO_VENDOR"
EOF
fi

export CARGO_HOME=$cargo_home
export CARGO_NET_OFFLINE=true
export CARGO_BUILD_JOBS=4
export MAKEFLAGS=-j4
export SOURCE_DATE_EPOCH=0
export TZ=UTC
export LC_ALL=C
export HARMONY_DOWNLOAD_DIR=$downloads
export HARMONY_ARTIFACT_DIR=$artifacts
export GUEST_BUILD_ROOT=$build_root
export HARMONY_BUILD_PATH_PREFIX=$work
if [ "$host_arch" = aarch64 ]; then
    export HARMONY_SMB_ROM=$rom
    export HARMONY_RUST_SOURCE_ROOT=$HARMONY_NIX_RUST_SOURCE_ROOT
    export HARMONY_RUST_LIBUNWIND=$HARMONY_NIX_RUST_LIBUNWIND
fi

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
    echo "== N5: build owned musl and the Cargo-lock-derived NES agent offline"
    (
        cd "$linux_dir"
        # shellcheck source=../linux/lib-build.sh disable=SC1091
        . ./lib-build.sh
        build_arm64_game_musl
    )
    if ! agent_output=$(HARMONY_MUSL_PREFIX="$build_root/musl-arm64-game-prefix" \
        bash "$guest/tetanes-agent/build.sh"); then
        printf '%s\n' "$agent_output" >&2
        echo "FAIL: offline NES agent build or image audit failed" >&2
        exit 1
    fi
    printf '%s\n' "$agent_output"
    agent=$(printf '%s\n' "$agent_output" | tail -1)
    [ -x "$agent" ] || { echo "FAIL: offline NES agent missing: $agent" >&2; exit 1; }
    export HARMONY_TETANES_AGENT=$agent

    echo "== N5: build minimal ARM kernel and initramfs"
    (cd "$linux_dir" && ./build-arm64-kernel.sh && ./build-arm64-initramfs.sh)

    if [ "$minimal_only" -eq 0 ]; then
        echo "== N5: build NES kernel, initramfs, and payload"
        (cd "$linux_dir" && ./build-arm64-game-kernel.sh && ./build-arm64-game-image.sh)
        echo "== N5: build PostgreSQL kernel, initramfs, and payloads"
        (cd "$linux_dir" && ./build-arm64-postgres-kernel.sh && ./build-arm64-postgres-image.sh)
    fi

    mkdir -p "$stage/arm64"
    if [ "$minimal_only" -eq 1 ]; then
        names=(Image initramfs.cpio.gz)
    else
        names=(
            Image
            initramfs.cpio.gz
            Image-game
            initramfs-game.cpio.gz
            harmony-tetanes-agent
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
else
    echo "== N5: build minimal x86 kernel and initramfs"
    (cd "$linux_dir" && ./build-kernel.sh && ./build-initramfs.sh)
    if [ "$serialization_gate" -eq 1 ]; then
        echo "== N5: run /dev/harmony serialization positive and planted negative"
        (cd "$linux_dir" && ./test-harmony-serialization.sh)
    fi
    mkdir -p "$stage/x86_64"
    for name in bzImage initramfs.cpio.gz; do
        [ -f "$artifacts/$name" ] || {
            echo "FAIL: lock build did not produce x86_64/$name" >&2
            exit 1
        }
        cp -p "$artifacts/$name" "$stage/x86_64/$name"
    done
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
