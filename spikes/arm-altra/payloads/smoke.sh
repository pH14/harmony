#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The TCG payload smoke — LIVENESS AND PROTOCOL ONLY.
#
# `docs/ARM-ALTRA.md` §The TCG smoke: qemu-system-aarch64 (TCG) is the slow oracle
# for liveness and protocol, and NOTHING ELSE. Each oracle payload boots under TCG,
# runs its counting window to completion, and its console/exit protocol round-trips.
# This script diffs *structure* against golden/ (never counts), and it exercises the
# one thing counts-checking would need but that TCG cannot provide: nothing here
# says anything about BR_RETIRED, PMIs, or skid. Those are silicon's, stage AA-1's.
#
# What this smoke genuinely proves, and it is not nothing:
#   - the runtime boots (MMU, GICv3, PL011, exception vectors) on the emulated N1;
#   - every payload's window opens and closes and the payload exits 0 (its in-guest
#     self-checks — atomic counter exact, seqlock quiescent, all interrupts taken —
#     all held);
#   - the deterministic accumulators (branch-dense, straight-line) match the values
#     the ORACLE MODEL predicts, so the branch predicates and PRNG in the asm agree
#     with the model bit-for-bit. That is the strongest statement emulation can make
#     about the oracle.
#
# Evidence integrity #1 (a done-marker is never success): every constituent RC is
# propagated. QEMU's exit status is the payload's own semihosting exit code; a
# nonzero code, a timeout, or a golden mismatch fails the whole script. There is no
# "reached the end" success path.

set -euo pipefail

cd "$(dirname "$0")"

CPU=neoverse-n1
BIN=target/aarch64-unknown-none/release
PAYLOADS="ident straight-line branch-dense svc exception-abort wfi-idle \
llsc-atomics lse-atomics clock-page aa4-self-modify"

if ! command -v qemu-system-aarch64 >/dev/null 2>&1; then
    echo "FAIL: qemu-system-aarch64 not found." >&2
    echo "      macOS: brew install qemu    Linux: apt-get install qemu-system-arm" >&2
    exit 1
fi

# `timeout` is coreutils; stock macOS has neither, and Homebrew's coreutils installs
# it as `gtimeout` unless the gnubin dir is on PATH. Resolve whichever exists so the
# documented Mac-local smoke does not exit 127 before QEMU even starts.
if command -v timeout >/dev/null 2>&1; then
    TIMEOUT=timeout
elif command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT=gtimeout
else
    echo "FAIL: no timeout command found." >&2
    echo "      macOS: brew install coreutils (provides gtimeout)    Linux: coreutils has timeout" >&2
    exit 1
fi

echo "== building payloads (aarch64-unknown-none, release)"
cargo build --release

echo "== oracle-model self-check + TCG-observed accumulator pins (host target)"
host_triple=$(rustc -vV | sed -n 's/^host: //p')
( cd ../oracle-model && cargo test --features std --target "$host_triple" --quiet )

echo "== window verification: every payload's branches match the oracle model"
( cd ../harness && cargo run --quiet --bin arm-scan -- \
    windows "../payloads/$BIN" )

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# Normalize a captured stream to its environment-INDEPENDENT structure:
#   - drop the two window mark bytes (STX/ETX) — they are protocol, not text;
#   - blank the CPU-specific hex in `ident`'s ID/CAP rows (those values ARE the
#     environment: MIDR, feature bits — pinning them would assert "this exact QEMU
#     CPU", which is not what the smoke checks). Everything else — banners, the
#     deterministic ACC/retry/final values, the mode tokens, PASS/EXIT — is kept
#     verbatim, because it is genuinely environment-independent and drift in it is
#     a real regression.
normalize() {
    tr -d '\r' | tr -d '\002\003' \
        | sed -E 's/^(ID [a-z0-9_]+=)0x[0-9a-f]+$/\1<hex>/' \
        | sed -E 's/^(CAP [a-z_]+=)[0-9]+( expect=.*)$/\1<n>\2/'
}

run_one() {
    payload=$1
    run=$2
    raw="$tmpdir/$payload.$run.raw"
    got="$tmpdir/$payload.$run.txt"
    status=0
    # A generous per-payload timeout: WFI/idle takes real emulated interrupts.
    "$TIMEOUT" 120 qemu-system-aarch64 \
        -M virt,gic-version=3 -cpu "$CPU" -m 512 -nographic -no-reboot \
        -semihosting-config enable=on,target=native \
        -kernel "$BIN/$payload" </dev/null >"$raw" 2>"$tmpdir/qemu.err" || status=$?

    if [ "$status" -eq 124 ]; then
        echo "FAIL: $payload (run $run): timed out after 120 s (a hung payload — likely" >&2
        echo "      a lost interrupt or a livelocked exclusive; NOT a pass)" >&2
        exit 1
    fi
    # The payload's semihosting SYS_EXIT makes QEMU exit with the payload's own
    # code. Anything but 0 is a failed in-guest self-check — a real failure, never
    # rounded up.
    if [ "$status" -ne 0 ]; then
        echo "FAIL: $payload (run $run): payload exit status $status (want 0)" >&2
        echo "--- captured serial output:" >&2
        normalize <"$raw" >&2 || true
        echo "--- QEMU stderr:" >&2
        cat "$tmpdir/qemu.err" >&2 || true
        exit 1
    fi

    normalize <"$raw" >"$got"
    if ! cmp -s "$got" "golden/$payload.txt"; then
        echo "FAIL: $payload (run $run): structure differs from golden/$payload.txt" >&2
        echo "--- got:" >&2
        cat "$got" >&2
        echo "--- want:" >&2
        cat "golden/$payload.txt" >&2
        exit 1
    fi
    echo "ok: $payload (run $run)"
}

# Twice: under TCG two runs must already be byte-identical after normalization —
# that is what timing-independence of the *protocol* means (the counts are not
# checked here at all).
for run in 1 2; do
    echo "== payload suite, run $run"
    for payload in $PAYLOADS; do
        run_one "$payload" "$run"
    done
done

echo "PASS: all payloads boot, round-trip their protocol, and match golden structure (TCG)"
echo "NOTE: this proves liveness and protocol only. Counts, PMIs and skid are silicon's"
echo "      (stage AA-1) and are asserted by nothing here."
