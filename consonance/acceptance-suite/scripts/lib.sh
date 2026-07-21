#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash
# Portable helpers shared by acceptance-suite gates. Source, don't execute.

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
