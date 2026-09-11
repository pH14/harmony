#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned diagnostic kernel in a disposable source tree.
# This script deliberately does not install a kernel or any kernel modules.
set -Eeuo pipefail

readonly KERNEL_URL='https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.17.tar.xz'
readonly KERNEL_VERSION='6.17'
readonly KERNEL_SOURCE_SHA256='9b607166a1c999d8326098121222feb080a20a3253975fcdfa2de96ba7f757a7'
readonly KERNEL_ARCH='x86'
readonly KERNEL_LOCALVERSION='-harmony-sync-shadow-diagnostic'

fail() {
    printf 'nested-kvm-build-kernel: %s\n' "$*" >&2
    exit 1
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "required tool is unavailable: $1"
}

for tool in curl sha256sum tar patch make awk grep cp mkdir mktemp uname; do
    require_tool "$tool"
done

case "$(uname -m)" in
    x86_64|amd64) ;;
    *) fail "an x86_64 builder is required (host is $(uname -m))" ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
patch_file="$script_dir/nested-kvm-sync-shadow.patch"
reports_dir="$repo_root/reports"

[ -f "$patch_file" ] || fail "missing root-owned patch: $patch_file"

build_tmp=''
cleanup() {
    if [ -n "$build_tmp" ] && [ -d "$build_tmp" ]; then
        rm -rf -- "$build_tmp"
    fi
}
trap cleanup EXIT

build_tmp=$(mktemp -d "${TMPDIR:-/tmp}/harmony-nested-kernel.XXXXXX")
archive="$build_tmp/linux-$KERNEL_VERSION.tar.xz"

printf 'nested-kvm-build-kernel: downloading %s\n' "$KERNEL_URL"
curl --fail --location --proto '=https' --tlsv1.2 --silent --show-error \
    --connect-timeout 20 --max-time 300 --output "$archive" "$KERNEL_URL"

archive_sha256=$(sha256sum -- "$archive" | awk '{print $1}')
[ "$archive_sha256" = "$KERNEL_SOURCE_SHA256" ] || \
    fail "Linux source checksum mismatch: $archive_sha256"

printf 'nested-kvm-build-kernel: extracting verified Linux %s\n' "$KERNEL_VERSION"
tar --extract --xz --file="$archive" --directory="$build_tmp"
kernel_src="$build_tmp/linux-$KERNEL_VERSION"
[ -d "$kernel_src" ] || fail "expected source directory is missing: $kernel_src"

printf 'nested-kvm-build-kernel: applying shadow-sync patch with fuzz=0\n'
patch --batch --forward --fuzz=0 --directory="$kernel_src" --strip=1 < "$patch_file"

printf 'nested-kvm-build-kernel: selecting x86_64_defconfig\n'
make --directory="$kernel_src" ARCH="$KERNEL_ARCH" -j4 x86_64_defconfig
config_tool="$kernel_src/scripts/config"
[ -x "$config_tool" ] || fail "kernel config helper is unavailable: $config_tool"
config="$kernel_src/.config"
set_config() {
    "$config_tool" --file "$config" "$@"
}

# Keep every KVM implementation built in so the guest can load no modules.
set_config --enable VIRTUALIZATION
set_config --enable KVM
set_config --enable KVM_AMD
set_config --enable KVM_INTEL

# The outer harness supplies a gzip initramfs and boots an ELF init process.
set_config --enable BLK_DEV_INITRD
set_config --enable RD_GZIP
set_config --enable BINFMT_ELF

# The initramfs mounts these built-in filesystems and uses the serial console.
set_config --enable TTY
set_config --enable SERIAL_8250
set_config --enable SERIAL_8250_CONSOLE
set_config --enable DEVTMPFS
set_config --enable DEVTMPFS_MOUNT
set_config --enable PROC_FS
set_config --enable SYSFS

# ACPI_SLEEP provides the system power-state path used by poweroff in the VM.
set_config --enable ACPI
set_config --enable PM_SLEEP
set_config --enable ACPI_SLEEP

# This is a built-in diagnostic image; it must not depend on loadable modules.
set_config --disable MODULES
set_config --enable DEBUG_INFO_NONE
set_config --disable DEBUG_INFO
set_config --disable SYSTEM_TRUSTED_KEYRING
set_config --disable SYSTEM_REVOCATION_LIST
set_config --set-str SYSTEM_TRUSTED_KEYS ''
set_config --set-str SYSTEM_REVOCATION_KEYS ''
set_config --set-str LOCALVERSION "$KERNEL_LOCALVERSION"

make --directory="$kernel_src" ARCH="$KERNEL_ARCH" -j4 olddefconfig
config="$kernel_src/.config"
[ -s "$config" ] || fail "kernel configuration was not generated"

require_config_y() {
    local symbol=$1
    grep -qx "${symbol}=y" "$config" || fail "$symbol was not retained as built-in"
}

require_config_disabled() {
    local symbol=$1
    if grep -Eq "^${symbol}=(y|m)$" "$config"; then
        fail "$symbol must be disabled"
    fi
}

require_config_empty_string() {
    local symbol=$1
    if grep -q "^${symbol}=" "$config"; then
        grep -qx "${symbol}=\"\"" "$config" || fail "$symbol must be empty"
    fi
}

for symbol in \
    CONFIG_X86_64 \
    CONFIG_VIRTUALIZATION \
    CONFIG_KVM \
    CONFIG_KVM_AMD \
    CONFIG_KVM_INTEL \
    CONFIG_BLK_DEV_INITRD \
    CONFIG_RD_GZIP \
    CONFIG_BINFMT_ELF \
    CONFIG_TTY \
    CONFIG_SERIAL_8250 \
    CONFIG_SERIAL_8250_CONSOLE \
    CONFIG_DEVTMPFS \
    CONFIG_DEVTMPFS_MOUNT \
    CONFIG_PROC_FS \
    CONFIG_SYSFS \
    CONFIG_ACPI \
    CONFIG_PM_SLEEP \
    CONFIG_ACPI_SLEEP; do
    require_config_y "$symbol"
done

require_config_disabled CONFIG_MODULES
require_config_disabled CONFIG_DEBUG_INFO
require_config_y CONFIG_DEBUG_INFO_NONE
require_config_disabled CONFIG_SYSTEM_TRUSTED_KEYRING
require_config_disabled CONFIG_SYSTEM_REVOCATION_LIST
require_config_empty_string CONFIG_SYSTEM_TRUSTED_KEYS
require_config_empty_string CONFIG_SYSTEM_REVOCATION_KEYS
grep -qx "CONFIG_LOCALVERSION=\"$KERNEL_LOCALVERSION\"" "$config" || \
    fail "CONFIG_LOCALVERSION was not retained"

# Retain the inputs even if compilation later fails or times out.
mkdir -p -- "$reports_dir"
cp -- "$config" "$reports_dir/nested-custom-kernel.config"
{
    printf 'kernel_source_url=%s\n' "$KERNEL_URL"
    printf 'kernel_source_sha256=%s\n' "$archive_sha256"
    sha256sum -- "$patch_file" "$config"
    printf 'build_parallelism=4\n'
} > "$reports_dir/nested-custom-build-inputs.txt"

printf 'nested-kvm-build-kernel: building bzImage with make -j4\n'
make --directory="$kernel_src" ARCH="$KERNEL_ARCH" -j4 bzImage
image="$kernel_src/arch/x86/boot/bzImage"
[ -s "$image" ] || fail "kernel image was not produced: $image"

mkdir -p -- "$reports_dir"
cp -- "$image" "$reports_dir/nested-custom-bzImage"
cp -- "$config" "$reports_dir/nested-custom-kernel.config"

patch_sha256=$(sha256sum -- "$patch_file" | awk '{print $1}')
config_sha256=$(sha256sum -- "$config" | awk '{print $1}')
image_sha256=$(sha256sum -- "$image" | awk '{print $1}')
{
    printf 'format=harmony-nested-kernel-build-v1\n'
    printf 'kernel_version=%s\n' "$KERNEL_VERSION"
    printf 'kernel_source_url=%s\n' "$KERNEL_URL"
    printf 'kernel_source_sha256=%s\n' "$KERNEL_SOURCE_SHA256"
    printf 'kernel_source_archive_sha256=%s\n' "$archive_sha256"
    printf 'sync_shadow_patch=%s\n' "$patch_sha256"
    printf 'config_sha256=%s\n' "$config_sha256"
    printf 'bzImage_sha256=%s\n' "$image_sha256"
} > "$reports_dir/nested-custom-kernel-sha256.txt"

printf 'nested-kvm-build-kernel: wrote reports/nested-custom-bzImage\n'
printf 'nested-kvm-build-kernel: wrote reports/nested-custom-kernel.config\n'
printf 'nested-kvm-build-kernel: wrote reports/nested-custom-kernel-sha256.txt\n'
