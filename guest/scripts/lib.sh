# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash
# Shared helpers for the guest/ shell scripts. Source, don't execute.
# Portability: macOS ships shasum and gtimeout (brew coreutils); Linux ships
# sha256sum and timeout. Each helper is declared exactly once, here.

# sha256_of <file>: print the lowercase hex digest, nothing else.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        echo "FAIL: need sha256sum or shasum" >&2
        exit 1
    fi
}

# run_with_timeout <seconds> <cmd...>: GNU timeout or gtimeout, whichever exists.
run_with_timeout() {
    if command -v timeout >/dev/null 2>&1; then
        timeout "$@"
    elif command -v gtimeout >/dev/null 2>&1; then
        gtimeout "$@"
    else
        echo "FAIL: need 'timeout' or 'gtimeout' (macOS: brew install coreutils)" >&2
        exit 1
    fi
}
