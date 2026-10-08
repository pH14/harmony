#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Static scan of the built x86 guest kernel for raw counter and hardware-RNG
# instructions: rdtsc `0f 31`, rdtscp `0f 01 f9`, rdrand `0f c7 /6` and rdseed
# `0f c7 /7`.
#
# Stock KVM cannot intercept RDTSC, RDTSCP, RDRAND or RDSEED, so any of them
# executed by the guest kernel reads host state that replay cannot reproduce.
# The Harmony kernel routes rdtsc() and rdtsc_ordered() through
# RDMSR(IA32_TSC), which Consonance answers from virtual time, and reports
# RDRAND and RDSEED as unavailable. The image therefore carries none of these
# instructions, and any occurrence fails the build. User-mode reads are trapped
# separately through CR4.TSD.
#
# bzImage contains three executable artifacts, and all three run: the real-mode
# setup code, the decompressor, and the kernel proper. The kernel proper is a
# 64-bit ELF and is scanned by disassembly. Setup and the decompressor's
# mode-mixed head run 16-bit code, where one disassembly mode can consume a real
# opcode as another instruction's operand, so their executable sections are
# searched for the raw opcode bytes. The decompressor's 64-bit `.text` is
# disassembled.
#
# CONFIG_MODULES is off, so the built image is all the kernel code that can run.
#
# Usage: scan-counter-opcodes.sh <vmlinux>
#   <vmlinux> is the uncompressed kernel ELF. The boot artifacts are found in its
#   build tree (arch/x86/boot/{setup.elf,compressed/vmlinux}).
set -euo pipefail

VMLINUX=${1:?usage: scan-counter-opcodes.sh <vmlinux>}

# KOBJ is vmlinux's build tree.
KOBJ=$(dirname "$VMLINUX")
RAW_ARTIFACTS=(
    "setup=$KOBJ/arch/x86/boot/setup.elf"
    "decompressor=$KOBJ/arch/x86/boot/compressed/vmlinux"
)

# sites <disasm-file> [artifact-tag] [mnemonic-regex]: emit
# "[tag:]symbol+0xOFFSET" for each matching instruction, so a failure names
# where it is. objdump prints prefixes such as `data16` as separate tokens, so
# the match skips them and reads the first non-prefix token as the mnemonic.
# The matched instructions take no operand that names a symbol, so a symbol
# containing "rdtsc" in another instruction's operand cannot match.
sites() {
    awk -v mnre="${3:-^(rdtsc|rdtscp)$}" '
        /^[0-9a-f]+ <[^>]+>:$/ {
            fstart = $1; sym = $2; gsub(/[<>:]/, "", sym); next
        }
        /^[[:space:]]*[0-9a-f]+:\t/ {
            addr = $1; sub(/:$/, "", addr)
            n = split($0, f, "\t")
            if (n >= 3) {
                cnt = split(f[3], toks, /[[:space:]]+/)
                mn = ""
                for (i = 1; i <= cnt; i++) {
                    t = toks[i]
                    if (t ~ /^(data16|data32|addr16|addr32|lock|rep|repz|repe|repnz|repne|rex|rex\..*|cs|ds|es|fs|gs|ss|bnd|notrack)$/)
                        continue
                    mn = t
                    break
                }
                if (mn ~ mnre) print sym, addr, fstart
            }
        }
    ' "$1" | while read -r sym addr fstart; do
        # Offset = instruction address − function start. bash arithmetic is
        # 64-bit two's-complement, so even with the high kernel bit set the
        # same-function subtraction yields the exact low-bits offset.
        printf '%s%s+0x%x\n' "${2:+$2:}" "$sym" "$(( 0x$addr - 0x$fstart ))"
    done | sort
}

# all_sites [mnemonic-regex]: disassemble the kernel proper and emit its
# artifact-qualified site list for that mnemonic class (boot artifacts use the
# section-aware scan below). FAILS if vmlinux is missing — a check that silently
# skips its target passes vacuously.
all_sites() {
    if [ ! -f "$VMLINUX" ]; then
        echo "FAIL: kernel ELF '$VMLINUX' not found — scan the uncompressed vmlinux." >&2
        return 1
    fi
    local dis
    dis=$(mktemp)
    objdump -d "$VMLINUX" > "$dis"
    sites "$dis" vmlinux "${1:-}"
    rm -f "$dis"
}

# raw_byte_scan: fail-closed opcode scan of the boot artifacts. Setup and the
# decompressor's `.head.text` really are 16-bit / mode-mixed, so their scan is
# raw and decode-independent: `0f 31`, `0f 01 f9`, or a hardware-RNG encoding
# anywhere in an executable section fails. The decompressor's ordinary `.text`
# is compiler-generated 64-bit code, however, and is disassembled in that
# architectural mode. Treating a `0f31` pair inside a 64-bit instruction's
# displacement as RDTSC makes the check depend on unrelated link layout while
# proving nothing about an executable instruction. Both paths still require
# ZERO counter and hardware-RNG instructions. Runs on every build (r7 P2), not
# once. Returns 1 on any hit, decode failure, or missing artifact.
# raw_byte_scan_one <path> <tag>: scan one artifact's executable sections. 0 =
# clean, 1 = a hit or a structural problem (missing/no-sections).
raw_byte_scan_one() {
    local path=$1 tag=$2 names s hex n31 n01f9 nrng m reg rc=0
    local dis counter_found rng_found bin
    if [ ! -f "$path" ]; then
        echo "FAIL: boot artifact '$tag' not found at $path — every executable component of" >&2
        echo "  bzImage must be scanned (setup + decompressor + kernel); build first." >&2
        return 1
    fi
    # Executable section names: every PROGBITS section whose flags field contains
    # the X (executable) bit — NOT just an exact `AX` token, so combined flags
    # like `WAX` / `AXl` are covered too (cross-model r9 P2). readelf -SW columns
    # after PROGBITS are addr, off, size, ES, Flg — so the flags are the field 5
    # past PROGBITS, and the section name is the field just before it.
    names=$(readelf -SW "$path" 2>/dev/null | awk '
        { for (i = 1; i <= NF; i++)
              if ($i == "PROGBITS") { if ($(i + 5) ~ /X/) print $(i - 1); break } }')
    if [ -z "$names" ]; then
        echo "FAIL: $tag ($path) has no executable sections — cannot have been scanned" >&2
        return 1
    fi
    for s in $names; do
        # Extract the section's raw bytes as hex. FAIL-CLOSED on any extraction
        # failure (objcopy / od / tr) — `pipefail` is set, so the pipeline's exit
        # status is checked explicitly here; otherwise a swallowed failure (this
        # runs under `if ! raw_byte_scan`, where errexit is off) would leave an
        # empty `hex`, match nothing, and silently green the check (cross-model
        # r9 P1).
        # objcopy must write to a regular file: with `/dev/stdout` it emits zero
        # bytes on binutils 2.44, which would read no section at all.
        bin=$(mktemp)
        if ! objcopy -O binary --only-section="$s" "$path" "$bin" 2>/dev/null \
            || ! hex=$(od -An -v -tx1 "$bin" | tr -d ' \n'); then
            rm -f "$bin"
            echo "FAIL: could not extract bytes for $tag section $s (objcopy/od/tr failed) —" >&2
            echo "  refusing to green a scan that did not read the section (fail-closed)." >&2
            return 1
        fi
        rm -f "$bin"
        if [ "$tag" = decompressor ] && [ "$s" = .text ]; then
            dis=$(mktemp)
            if ! objdump -d -j "$s" "$path" >"$dis" 2>/dev/null; then
                echo "FAIL: could not disassemble 64-bit $tag section $s" >&2
                rm -f "$dis"
                return 1
            fi
            counter_found=$(sites "$dis" "$tag")
            rng_found=$(sites "$dis" "$tag" '^(rdrand|rdseed)$')
            rm -f "$dis"
            if [ -n "$counter_found" ]; then
                echo "FAIL: decoded counter instruction(s) in 64-bit $tag section $s:" >&2
                printf '%s\n' "$counter_found" | sed 's/^/  /' >&2
                rc=1
            fi
            if [ -n "$rng_found" ]; then
                echo "FAIL: decoded hardware-RNG instruction(s) in 64-bit $tag section $s:" >&2
                printf '%s\n' "$rng_found" | sed 's/^/  /' >&2
                rc=1
            fi
            continue
        fi
        n31=$(grep -oE '0f31' <<<"$hex" | wc -l | tr -d ' ')
        n01f9=$(grep -oE '0f01f9' <<<"$hex" | wc -l | tr -d ' ')
        if [ "$n31" != 0 ] || [ "$n01f9" != 0 ]; then
            echo "FAIL: raw counter-opcode bytes in $tag section $s — rdtsc(0f31)=$n31" >&2
            echo "  rdtscp(0f01f9)=$n01f9. These 16-bit/mode-mixed artifacts must carry NONE;" >&2
            echo "  a real counter read here is not trap-survivable — review by hand." >&2
            # Preserve enough evidence to localize a toolchain-dependent raw-byte
            # hit without publishing the rejected boot artifact. Hex-string byte
            # offsets are section-relative; the context deliberately includes
            # bytes on both sides because an opcode may straddle instructions.
            for opcode in 0f31 0f01f9; do
                while IFS=: read -r hex_offset _; do
                    [ -n "$hex_offset" ] || continue
                    byte_offset=$((hex_offset / 2))
                    context_start=$((byte_offset > 8 ? byte_offset - 8 : 0))
                    echo "  $opcode section-byte-offset=$byte_offset context=${hex:$((context_start * 2)):40}" >&2
                done < <(grep -boE "$opcode" <<<"$hex" || true)
            done
            echo "  decoded counter instructions in $tag section $s (empty means the raw bytes occur inside another instruction):" >&2
            objdump -d -j "$s" "$path" 2>/dev/null \
                | grep -Ei -C 2 '\<(rdtsc|rdtscp)\>' >&2 || true
            rc=1
        fi
        # rdrand/rdseed are `0f c7` with modrm reg field 6/7 (`/1` is cmpxchg8b,
        # a legitimate instruction), so decode the modrm byte of each candidate.
        nrng=0
        for m in $(grep -oE '0fc7[0-9a-f]{2}' <<<"$hex"); do
            reg=$(( (0x${m:4:2} >> 3) & 7 ))
            if [ "$reg" = 6 ] || [ "$reg" = 7 ]; then
                nrng=$((nrng + 1))
            fi
        done
        if [ "$nrng" != 0 ]; then
            echo "FAIL: raw hardware-RNG opcode bytes in $tag section $s —" >&2
            echo "  rdrand/rdseed(0fc7 /6|/7)=$nrng. These artifacts must carry NONE: the" >&2
            echo "  frozen CPUID hides the feature but SVM cannot intercept the instruction," >&2
            echo "  so an executed site is a true-entropy hole — review by hand." >&2
            rc=1
        fi
    done
    return $rc
}

# raw_byte_scan: fail-closed raw-byte scan across every RAW_ARTIFACTS entry.
raw_byte_scan() {
    command -v objcopy >/dev/null 2>&1 && command -v readelf >/dev/null 2>&1 || {
        echo "FAIL: objcopy/readelf not found (binutils required for the raw-byte scan)" >&2
        return 1
    }
    local a rc=0
    for a in "${RAW_ARTIFACTS[@]}"; do
        raw_byte_scan_one "${a#*=}" "${a%%=*}" || rc=1
    done
    return $rc
}

# ---- self-test (every invocation): the scan must be able to fail -----------
self_test() {
    local d
    d=$(mktemp -d)
    trap 'rm -rf "$d"' RETURN

    # A function whose name contains "rdtsc" and a call whose operand names an
    # rdtsc-named symbol must not match.
    cat > "$d/clean.dis" << 'EOF'
ffffffff81000010 <harmony_pvclock_read>:
ffffffff81000010:	8b 07                	mov    (%rdi),%eax
ffffffff81000012:	e8 00 00 00 00       	call   ffffffff81000020 <trace_rdtsc_event>
ffffffff81000017:	c3                   	ret
ffffffff81000020 <trace_rdtsc_event>:
ffffffff81000020:	31 c0                	xor    %eax,%eax
ffffffff81000022:	c3                   	ret
EOF
    if [ -n "$(sites "$d/clean.dis" vmlinux)$(sites "$d/clean.dis" vmlinux '^(rdrand|rdseed)$')" ]; then
        echo "FAIL: self-test — the clean fixture matched an instruction" >&2
        exit 1
    fi

    cat > "$d/planted.dis" << 'EOF'
ffffffff81000030 <sneaky_new_timer>:
ffffffff81000030:	0f 01 f9             	rdtscp
ffffffff81000033:	c3                   	ret
ffffffff81000040 <prefixed_reader>:
ffffffff81000040:	66 0f 31             	data16 rdtsc
ffffffff81000043:	c3                   	ret
EOF
    if [ "$(sites "$d/planted.dis" vmlinux)" != "$(printf '%s\n' \
        vmlinux:prefixed_reader+0x0 vmlinux:sneaky_new_timer+0x0)" ]; then
        echo "FAIL: self-test — a planted rdtscp or prefixed 'data16 rdtsc' was NOT caught" >&2
        exit 1
    fi

    cat > "$d/rng.dis" << 'EOF'
ffffffff81000000 <x86_init_rdrand>:
ffffffff81000000:	48 0f c7 f0          	rdrand %rax
ffffffff81000004:	c3                   	ret
ffffffff81000010 <sneaky_seed>:
ffffffff81000010:	0f c7 f8             	rdseed %eax
ffffffff81000013:	c3                   	ret
EOF
    if [ "$(sites "$d/rng.dis" vmlinux '^(rdrand|rdseed)$' | wc -l | tr -d ' ')" != 2 ]; then
        echo "FAIL: self-test — a planted rdrand or rdseed was NOT caught" >&2
        exit 1
    fi

    # RAW-BYTE SCAN (r7 P2): the fail-closed scan must catch a counter opcode in
    # an executable section regardless of decode mode. Assemble a tiny ELF whose
    # AX `.text` carries a raw `0f 31` (rdtsc), and a clean one, and drive
    # `raw_byte_scan_one` against each. `as` is part of the binutils this script
    # already needs.
    if command -v as >/dev/null 2>&1 && command -v objcopy >/dev/null 2>&1 \
        && command -v readelf >/dev/null 2>&1; then
        printf '.section .text,"ax"\n.byte 0x0f,0x31\n.byte 0xc3\n' \
            | as -o "$d/dirty.elf" - 2>/dev/null && dirty_ok=1
        printf '.section .text,"ax"\n.byte 0x90,0x90\n.byte 0xc3\n' \
            | as -o "$d/clean.elf" - 2>/dev/null && clean_ok=1
        # A WRITABLE-executable section (flags "awx" → readelf "WAX"): the X-flag
        # selection must still scan it (an exact-`AX`-token match would miss it,
        # and a forbidden opcode there would pass — cross-model r9 P2).
        printf '.section .wtext,"awx"\n.byte 0x0f,0x31\n.byte 0xc3\n' \
            | as -o "$d/wax.elf" - 2>/dev/null && wax_ok=1
        if [ "${dirty_ok:-0}" = 1 ] && [ "${clean_ok:-0}" = 1 ]; then
            if ! raw_byte_scan_one "$d/clean.elf" clean >/dev/null 2>&1; then
                echo "FAIL: self-test — raw-byte scan flagged a clean fixture" >&2
                exit 1
            fi
            if raw_byte_scan_one "$d/dirty.elf" dirty >/dev/null 2>&1; then
                echo "FAIL: self-test — raw-byte scan MISSED a planted 0f 31 in a .text section" >&2
                exit 1
            fi
            # The decompressor's compiler-generated `.text` is known 64-bit.
            # A real decoded RDTSC must still fail there, while the same bytes
            # inside a LEA displacement must pass. The mixed-mode tags retain
            # the stricter raw rule and therefore reject that LEA fixture.
            if raw_byte_scan_one "$d/dirty.elf" decompressor >/dev/null 2>&1; then
                echo "FAIL: self-test — 64-bit decompressor scan MISSED a planted RDTSC" >&2
                exit 1
            fi
            printf '.section .text,"ax"\n.global fixture\nfixture:\n.byte 0x48,0x8d,0x3d,0x0f,0x31,0,0\n.byte 0xc3\n' \
                | as -o "$d/lea.elf" - 2>/dev/null && lea_ok=1
            if [ "${lea_ok:-0}" = 1 ]; then
                if ! raw_byte_scan_one "$d/lea.elf" decompressor >/dev/null 2>&1; then
                    echo "FAIL: self-test — 64-bit LEA displacement was misread as RDTSC" >&2
                    exit 1
                fi
                if raw_byte_scan_one "$d/lea.elf" setup >/dev/null 2>&1; then
                    echo "FAIL: self-test — mixed-mode raw scan accepted a planted 0f 31 pair" >&2
                    exit 1
                fi
            fi
            if [ "${wax_ok:-0}" = 1 ] \
                && raw_byte_scan_one "$d/wax.elf" wax >/dev/null 2>&1; then
                echo "FAIL: self-test — raw-byte scan MISSED a 0f 31 in a WAX (writable+exec)" >&2
                echo "  section — the X-flag selection is not catching combined flags" >&2
                exit 1
            fi
            # rdrand (`0f c7 f0`, modrm reg 6) must be caught; cmpxchg8b
            # (`0f c7 0f`, modrm reg 1) shares the opcode bytes and must pass.
            printf '.section .text,"ax"\n.byte 0x0f,0xc7,0xf0\n.byte 0xc3\n' \
                | as -o "$d/rng.elf" - 2>/dev/null && rng_ok=1
            printf '.section .text,"ax"\n.byte 0x0f,0xc7,0x0f\n.byte 0xc3\n' \
                | as -o "$d/cmpxchg.elf" - 2>/dev/null && cx_ok=1
            if [ "${rng_ok:-0}" = 1 ] && [ "${cx_ok:-0}" = 1 ]; then
                if raw_byte_scan_one "$d/rng.elf" rng >/dev/null 2>&1; then
                    echo "FAIL: self-test — raw-byte scan MISSED a planted 0f c7 /6 (rdrand)" >&2
                    exit 1
                fi
                if ! raw_byte_scan_one "$d/cmpxchg.elf" cmpxchg >/dev/null 2>&1; then
                    echo "FAIL: self-test — raw-byte scan flagged cmpxchg8b (0f c7 /1) as an" >&2
                    echo "  rng opcode — the modrm reg-field decode is wrong" >&2
                    exit 1
                fi
            fi
            raw_selftest="raw-byte-scan, "
        fi
    fi
    echo "ok: scan self-test (planted counter, prefixed counter, rng, ${raw_selftest:-}clean fixtures)"
}

self_test

command -v objdump >/dev/null 2>&1 || {
    echo "FAIL: objdump not found (binutils required)" >&2
    exit 1
}

FOUND=$(all_sites)
RNG_FOUND=$(all_sites '^(rdrand|rdseed)$')
failed=0
if [ -n "$FOUND" ]; then
    echo "FAIL: raw counter instruction(s) (rdtsc/rdtscp) in the kernel:" >&2
    printf '%s\n' "$FOUND" | sed 's/^/  /' >&2
    echo "  Read the counter through rdtsc() or rdtsc_ordered(), which use RDMSR(IA32_TSC)" >&2
    echo "  in the Harmony kernel, or through the Harmony clock page." >&2
    failed=1
fi
if [ -n "$RNG_FOUND" ]; then
    echo "FAIL: hardware-RNG instruction(s) (rdrand/rdseed) in the kernel:" >&2
    printf '%s\n' "$RNG_FOUND" | sed 's/^/  /' >&2
    echo "  The Harmony kernel reports RDRAND and RDSEED as unavailable; use" >&2
    echo "  rdrand_long() or rdseed_long() instead of the instruction." >&2
    failed=1
fi
if ! raw_byte_scan; then
    failed=1
fi
if [ "$failed" -ne 0 ]; then
    exit 1
fi

echo "ok: counter-opcode scan — no rdtsc, rdtscp, rdrand or rdseed in the kernel proper, setup or decompressor"
