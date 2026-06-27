#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Part A gate: build every payload, boot each under QEMU TCG and compare the
# serial output byte-for-byte against the committed golden. The whole suite
# runs twice — under TCG, two runs must already produce identical output
# (that is what timing-independence means).
#
# The captured stream is trimmed to the first 'PAYLOAD ' byte before the
# comparison: SeaBIOS/iPXE print version strings and PMM addresses on the
# serial console first, and those are environment-dependent by nature.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=../scripts/lib.sh disable=SC1091
. ../scripts/lib.sh

if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "FAIL: qemu-system-x86_64 not found." >&2
    echo "      macOS: brew install qemu    Linux: apt-get install qemu-system-x86" >&2
    exit 1
fi

PAYLOADS="hello compute clocks interrupts features \
insn-rdtsc insn-rng insn-cpuid insn-rdpmc insn-hlt insn-mwait \
msr-allowed msr-denied irq-landing irq-landing-rng pit-pic-stub"
BIN=target/x86_64-unknown-none/release

echo "== building payloads (x86_64-unknown-none, release)"
cargo build --release

echo "== checking compute digest host-side (compute-core test, host target)"
host_triple=$(rustc -vV | sed -n 's/^host: //p')
cargo test -p compute-core --release --target "$host_triple" --quiet

echo "== checking conformance tables derive from the contract (contract-data test, host target)"
cargo test -p contract-data --release --target "$host_triple" --quiet

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

# run_one <payload> <run-number>: boot, check exit status, compare output.
run_one() {
    payload=$1
    run=$2
    out="$tmpdir/$payload.$run.raw"
    trimmed="$tmpdir/$payload.$run.txt"
    status=0
    run_with_timeout 60 qemu-system-x86_64 \
        -m 256 -nographic -no-reboot \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -serial mon:stdio \
        -kernel "$BIN/$payload" </dev/null >"$out" 2>"$tmpdir/qemu.err" || status=$?
    if [ "$status" -eq 124 ]; then
        echo "FAIL: $payload (run $run): timed out after 60 s" >&2
        exit 1
    fi
    # isa-debug-exit makes QEMU's exit status (code << 1) | 1; payload 0 => 1.
    if [ "$status" -ne 1 ]; then
        echo "FAIL: $payload (run $run): QEMU exit status $status, want 1 (payload code 0)" >&2
        echo "--- captured serial output:" >&2
        tr -d '\r' <"$out" >&2 || true
        echo "--- QEMU stderr:" >&2
        cat "$tmpdir/qemu.err" >&2 || true
        exit 1
    fi
    # Trim the firmware preamble: anchor on this payload's own START banner,
    # and require it to be the first 'PAYLOAD ' bytes in the stream so a
    # payload emitting protocol lines before START fails loudly. (Other bytes
    # the payload might wrongly emit before START are indistinguishable from
    # the firmware's environment-dependent output without pinning firmware
    # version strings, which would be worse.)
    marker="PAYLOAD $payload START"
    off=$(grep -abo -F "$marker" "$out" | head -n 1 | cut -d: -f1 || true)
    first=$(grep -abo 'PAYLOAD ' "$out" | head -n 1 | cut -d: -f1 || true)
    if [ -z "$off" ]; then
        echo "FAIL: $payload (run $run): no '$marker' banner in serial output" >&2
        exit 1
    fi
    if [ "$first" != "$off" ]; then
        echo "FAIL: $payload (run $run): 'PAYLOAD ' bytes precede the START banner" >&2
        exit 1
    fi
    tail -c "+$((off + 1))" "$out" >"$trimmed"
    if ! cmp -s "$trimmed" "../golden/$payload.txt"; then
        echo "FAIL: $payload (run $run): output differs from golden/$payload.txt" >&2
        echo "--- got:" >&2
        cat "$trimmed" >&2
        echo "--- want:" >&2
        cat "../golden/$payload.txt" >&2
        exit 1
    fi
    echo "ok: $payload (run $run)"
}

for run in 1 2; do
    echo "== payload suite, run $run"
    for payload in $PAYLOADS; do
        run_one "$payload" "$run"
    done
done

echo "PASS: all payloads, both runs"
