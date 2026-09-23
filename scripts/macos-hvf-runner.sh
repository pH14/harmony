#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Cargo runner for aarch64-apple-darwin. Hypervisor.framework refuses a process
# without the hypervisor entitlement, so each binary is signed once before it
# runs. Signing a private copy and renaming it over the original keeps parallel
# test processes from executing a half-written file.
set -eu

binary=$1
marker=$binary.hvf-signed
if [ ! -e "$marker" ] || [ "$binary" -nt "$marker" ]; then
  staged=$binary.signing.$$
  cp -p "$binary" "$staged"
  codesign --sign - --force \
    --entitlements "$(dirname "$0")/../consonance/vmm-backend/hvf.entitlements.plist" \
    "$staged" 2>/dev/null
  mv -f "$staged" "$binary"
  touch "$marker"
fi
exec "$@"
