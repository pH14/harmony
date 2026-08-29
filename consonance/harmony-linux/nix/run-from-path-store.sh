#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Execute a Nix app closure from a fresh path-backed store without inheriting
# that store's user namespace. The caller must already be in a private mount
# namespace and must hold any host coordination locks for this process's full
# lifetime.
set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: run-from-path-store.sh STORE_ROOT APP_STORE_PATH ARG..." >&2
    exit 2
fi
store_root=$(realpath -e "$1")
app=$2
shift 2
case "$app" in
    /nix/store/*-harmony-build-guest-images) ;;
    *)
        echo "FAIL: unexpected guest-image app store path: $app" >&2
        exit 1
        ;;
esac
[ -x "$store_root$app/bin/harmony-build-guest-images" ] || {
    echo "FAIL: guest-image app is absent from the path store: $app" >&2
    exit 1
}
mount --bind "$store_root/nix/store" /nix/store
mount -o remount,bind,ro /nix/store
exec "$app/bin/harmony-build-guest-images" "$@"
