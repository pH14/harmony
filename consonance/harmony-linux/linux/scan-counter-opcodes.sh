#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Static counter-opcode scan of the built guest kernel — the x86 half of the
# paravirtual clock interface reachability gate (the task-100 LL/SC-scan discipline
# transposed to counter reads: rdtsc `0F 31`, rdtscp `0F 01 F9`).
#
# WHAT IT PROVES on x86: every raw counter read left in the image is a KNOWN,
# REVIEWED site — the committed allowlist records each site as
# `symbol+0xOFFSET` (the instruction's byte offset within its function), so a
# NEW rdtsc added inside an already-reviewed function is caught (a new site), a
# removed one goes stale, AND a removed+ADDED pair in one function is caught too
# (the offsets change even though the count would not — cross-model r10 P1).
# Each is allowlistable ONLY because the retained RDTSC/RDTSCP trap completes it
# with the same exit-count-derived value the pvclock page carries (§4.1 — on x86 a raw
# read is survivable-by-trap, never a determinism hole). Exact accounting in both
# directions: an unlisted (or moved) site fails the build; a stale entry fails
# too. (On ARM, where no trap exists, the transposed gate has an EMPTY allowlist
# by necessity; that discipline is validated at spike stage AA-5, not here.)
#
# ARMING: while the allowlist carries a `# GATE-UNARMED` marker line, the
# scan runs in CAPTURE mode — it prints every found site in paste-ready
# `symbol+0xOFFSET` form (one line per site) under a loud banner and then **FAILS the build**
# (fail-closed, the PR #110 r2 disposition: a disarmed reachability gate must
# never let a kernel build pass). The marker exists only for re-baselining
# (e.g. a kernel version bump): capture the printed baseline in a linux/amd64
# container or on the box, review it entry-by-entry, commit it, remove the
# marker. The committed tree ships with the marker REMOVED and the reviewed
# baseline present — the gate armed. The self-test proves the armed mode can
# fail on every invocation regardless of the marker.
#
# RUNTIME HALF — SPECCED AND STUBBED (stated per the task-110 evidence bar,
# not faked; accepted as such by the PR #110 foreman ruling): the §3.3
# ladder's third rung, W^X + rescan-on-exec (re-scanning any page the guest
# makes executable at runtime, so a JIT cannot mint a counter read the static
# scan never saw) needs vmm-side executable-page tracking — contract work
# tracked as bead hm-rfz. Until it lands, the static scan covers exactly the
# built image + the no-modules config (CONFIG_MODULES is asserted off, so
# there is no loadable code either).
#
# COVERAGE — every executable component of bzImage, not just the kernel proper
# (cross-model r4 P2). bzImage is three distinct executable artifacts glued
# together, and all three run: the real-mode `setup` code, the `compressed`
# decompressor that unpacks the kernel, and the kernel proper. Scanning only
# `vmlinux` left the first two unscanned — an rdtsc added to the decompressor
# would have sailed through a gate that calls itself a final-image reachability
# scan.
#
# TWO SCAN MODES, by artifact (cross-model r7 P2). The kernel proper (`vmlinux`)
# is a 64-bit ELF whose symbol-attributed disassembly is reliable, so it uses the
# objdump + ARTIFACT-QUALIFIED allowlist path (`artifact:symbol+0xOFFSET`, so the
# same symbol name in two artifacts cannot alias budgets). The **setup** and
# **decompressor** artifacts run 16-bit real-mode code and mode transitions, and
# a single `objdump -d` mode can mis-length an instruction there — a real
# `0f 31` / `0f 01 f9` could be consumed as another instruction's operand bytes
# and never emitted as an rdtsc mnemonic, evading the scan. So those two get a
# **fail-closed raw executable-byte scan on every build**: their executable
# (X-flag) sections are extracted and searched for the opcode byte sequences
# directly, decode-mode-independent. They carry ZERO counter reads today, so any
# hit fails the build (no allowlist — a real counter read there is never
# survivable-by-trap in the same way and must be reviewed by a human).
#
# SECOND MNEMONIC CLASS — HARDWARE RNG (rdrand `0f c7 /6`, rdseed `0f c7 /7`),
# same per-site machinery, SEPARATE allowlist, opposite justification regime:
# a counter read is survivable-by-trap, but SVM cannot intercept RDRAND/RDSEED,
# so an executed site feeds true entropy no contract can replay. The frozen
# CPUID hides both features from the guest (stock virtual_time composition);
# this scan is the audit that every site left in the image sits behind the
# feature check that hiding turns off — the x86 CPU contract x86
# RDRAND/RDSEED disposition's enforcement.
#
# Usage: scan-counter-opcodes.sh <vmlinux> [allowlist] [rng-allowlist]
#   <vmlinux> is the UNCOMPRESSED kernel ELF; the sibling boot artifacts are
#   derived from its build tree (arch/x86/boot/{setup.elf,compressed/vmlinux}).
#   The compressed bzImage itself is not directly scannable (no symbols) — it is
#   covered through the ELF products it is built from. Defaults the allowlists
#   to rdtsc-allowlist.txt / rdrand-allowlist.txt next to this script.
set -euo pipefail

VMLINUX=${1:?usage: scan-counter-opcodes.sh <vmlinux> [allowlist] [rng-allowlist]}
ALLOWLIST=${2:-"$(dirname "$0")/rdtsc-allowlist.txt"}
RNG_ALLOWLIST=${3:-"$(dirname "$0")/rdrand-allowlist.txt"}

# The boot artifacts. `vmlinux` (the kernel proper) is scanned by symbol-
# attributed objdump against the allowlist. Setup and the decompressor's mixed-
# mode head use raw executable bytes; the decompressor's 64-bit `.text` uses
# semantic disassembly. KOBJ is vmlinux's build tree.
KOBJ=$(dirname "$VMLINUX")
RAW_ARTIFACTS=(
    "setup=$KOBJ/arch/x86/boot/setup.elf"
    "decompressor=$KOBJ/arch/x86/boot/compressed/vmlinux"
)

# sites <disasm-file> [artifact-tag]: emit "[tag:]symbol+0xOFFSET" per rdtsc/
# rdtscp instruction — one line PER SITE, identified by the instruction's byte
# offset within its containing function (cross-model r10 P1). A per-function
# COUNT can miss a removed+added pair inside one function (the count is
# unchanged), so the gate would pass a moved/replaced counter read; a per-site
# offset changes, so the removed site goes stale and the added site is unlisted —
# both caught. The mnemonic match is prefix-aware (a legal `66 0f 31` renders
# `data16 rdtsc`, so the first token would misread `data16`): walk the mnemonic
# field skipping the x86 prefix tokens objdump emits separately. rdtsc/rdtscp
# take NO operands, so the first non-prefix token IS the mnemonic — a symbol
# named "...rdtsc..." in some other instruction's operand cannot false-match.
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
# section-aware scan below). FAILS if vmlinux is missing — a gate that silently
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
# displacement as RDTSC makes the gate depend on unrelated link layout while
# proving nothing about an executable instruction. Both paths still require
# ZERO counter and hardware-RNG instructions. Runs on every build (r7 P2), not
# once. Returns 1 on any hit, decode failure, or missing artifact.
# raw_byte_scan_one <path> <tag>: scan one artifact's executable sections. 0 =
# clean, 1 = a hit or a structural problem (missing/no-sections).
raw_byte_scan_one() {
    local path=$1 tag=$2 names s hex n31 n01f9 nrng m reg rc=0
    local dis counter_found rng_found
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
        # empty `hex`, match nothing, and silently green the gate (cross-model
        # r9 P1).
        if ! hex=$(objcopy -O binary --only-section="$s" "$path" /dev/stdout 2>/dev/null \
            | od -An -v -tx1 | tr -d ' \n'); then
            echo "FAIL: could not extract bytes for $tag section $s (objcopy/od/tr failed) —" >&2
            echo "  refusing to green a scan that did not read the section (fail-closed)." >&2
            return 1
        fi
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

# allowed <allowlist-file>: emit the reviewed per-site entries (comments/blank
# lines stripped), sorted; FAIL on a malformed entry. Every entry is a single
# token `[tag:]symbol+0xOFFSET` — the per-SITE identity is the gate (cross-model
# r10 P1); a bare function name (or the old `function count` form) would silently
# weaken it back to function granularity and miss a removed+added pair.
allowed() {
    local entries
    entries=$(sed -e 's/#.*$//' -e 's/[[:space:]]*$//' -e '/^$/d' "$1")
    if [ -n "$entries" ] && ! printf '%s\n' "$entries" \
        | awk 'NF != 1 || $1 !~ /\+0x[0-9a-f]+$/ { bad = 1 } END { exit bad }'; then
        echo "FAIL: malformed allowlist entry — every entry is a single" >&2
        echo "  '[tag:]symbol+0xOFFSET' site token (the per-site accounting);" >&2
        echo "  offending line(s):" >&2
        printf '%s\n' "$entries" | awk 'NF != 1 || $1 !~ /\+0x[0-9a-f]+$/' | sed 's/^/  /' >&2
        return 2
    fi
    printf '%s\n' "$entries" | sed '/^$/d' | sort
}

# scan <disasm-file> <allowlist-file>: 0 = clean, 1 = violations (printed).
# Pure text → text, so the self-test can drive it on fixtures.
scan() {
    scan_sites "$(sites "$1")" "$2"
}

# scan_sites <site-list> <allowlist-file> [kind]: the comparison itself, over
# an already-collected site list (so the real scan can feed it every artifact
# at once and the self-test can feed it a fixture). `kind` picks the failure
# guidance: `counter` (default, rdtsc/rdtscp) or `hwrng` (rdrand/rdseed).
scan_sites() {
    local found=$1 allow=$2 kind=${3:-counter}
    local allowed_entries bad=0
    allowed_entries=$(allowed "$allow") || return 2
    local unlisted stale
    unlisted=$(comm -23 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed_entries" | sed '/^$/d'))
    stale=$(comm -13 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed_entries" | sed '/^$/d'))
    if [ -n "$unlisted" ]; then
        if [ "$kind" = hwrng ]; then
            echo "FAIL: hardware-RNG instruction site(s) (rdrand/rdseed) not matching the allowlist:" >&2
            printf '%s\n' "$unlisted" | sed 's/^/  /' >&2
            echo "  Review each: the frozen CPUID hides RDRAND/RDSEED from the guest, so a" >&2
            echo "  site is allowlistable ONLY if it sits behind an X86_FEATURE_RDRAND /" >&2
            echo "  X86_FEATURE_RDSEED check (dead code under the contract); record the" >&2
            echo "  reviewed site in $allow. An unconditional site is a true-entropy hole" >&2
            echo "  (SVM cannot intercept the instruction) and must be removed instead." >&2
        else
            echo "FAIL: raw counter read(s) (rdtsc/rdtscp) not matching the allowlist" >&2
            echo "  (new function, or the instruction COUNT changed inside a reviewed one):" >&2
            printf '%s\n' "$unlisted" | sed 's/^/  /' >&2
            echo "  Review each: if it is a legitimate trap-backstopped path, record" >&2
            echo "  'function count' in $allow with a justification comment; if it is" >&2
            echo "  new timekeeping code, route it through the harmony pvclock page instead." >&2
        fi
        bad=1
    fi
    if [ -n "$stale" ]; then
        echo "FAIL: stale allowlist entr(ies) — no matching site/count in the image:" >&2
        printf '%s\n' "$stale" | sed 's/^/  /' >&2
        echo "  Update or remove them in $allow (exact accounting, both directions)." >&2
        bad=1
    fi
    return $bad
}

# unarmed <allowlist-file>: 0 iff the GATE-UNARMED marker is present.
unarmed() {
    grep -q '^# GATE-UNARMED' "$1"
}

# ---- self-test (every invocation): the ARMED gate must be able to FAIL -----
self_test() {
    local d
    d=$(mktemp -d)
    trap 'rm -rf "$d"' RETURN

    # Fixture: an allowlisted single-rdtsc site, a clean function, a function
    # whose NAME contains "rdtsc" but executes none (must not match), and a
    # call whose OPERAND mentions an rdtsc-named symbol (must not match).
    cat > "$d/clean.dis" << 'EOF'
ffffffff81000000 <native_sched_clock>:
ffffffff81000000:	0f 31                	rdtsc
ffffffff81000002:	c3                   	ret
ffffffff81000010 <harmony_pvclock_read>:
ffffffff81000010:	8b 07                	mov    (%rdi),%eax
ffffffff81000012:	e8 00 00 00 00       	call   ffffffff81000000 <native_sched_clock>
ffffffff81000017:	c3                   	ret
ffffffff81000020 <trace_rdtsc_event>:
ffffffff81000020:	31 c0                	xor    %eax,%eax
ffffffff81000022:	c3                   	ret
EOF
    printf 'native_sched_clock+0x0\n' > "$d/allow.txt"
    if ! scan "$d/clean.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — the clean fixture must pass" >&2
        exit 1
    fi

    # Planted rdtscp in a non-allowlisted function: MUST fail.
    cat > "$d/planted.dis" << 'EOF'
ffffffff81000000 <native_sched_clock>:
ffffffff81000000:	0f 31                	rdtsc
ffffffff81000002:	c3                   	ret
ffffffff81000030 <sneaky_new_timer>:
ffffffff81000030:	0f 01 f9             	rdtscp
ffffffff81000033:	c3                   	ret
EOF
    if scan "$d/planted.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a planted rdtscp in a non-allowlisted function was NOT caught" >&2
        exit 1
    fi

    # A PREFIXED counter read (`66 0f 31` → objdump renders `data16 rdtsc`):
    # MUST be caught — a first-token parse would read `data16` and miss it
    # (cross-model r8 P2).
    cat > "$d/planted-prefixed.dis" << 'EOF'
ffffffff81000040 <prefixed_reader>:
ffffffff81000040:	66 0f 31             	data16 rdtsc
ffffffff81000043:	c3                   	ret
EOF
    if scan "$d/planted-prefixed.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a prefixed 'data16 rdtsc' was NOT caught (first-token parse bug)" >&2
        exit 1
    fi

    # A SECOND rdtsc planted inside an ALREADY-allowlisted function (a new site
    # at offset 0x3): MUST fail — the per-site accounting catches the extra site.
    cat > "$d/planted-inside.dis" << 'EOF'
ffffffff81000000 <native_sched_clock>:
ffffffff81000000:	0f 31                	rdtsc
ffffffff81000002:	90                   	nop
ffffffff81000003:	0f 31                	rdtsc
ffffffff81000005:	c3                   	ret
EOF
    if scan "$d/planted-inside.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a NEW rdtsc inside an allowlisted function was NOT caught" >&2
        exit 1
    fi

    # REMOVED+ADDED pair inside one function (cross-model r10 P1): the rdtsc moves
    # from offset 0x0 to 0x3 — the COUNT is unchanged (still 1), so a per-function
    # count would pass it, but the per-SITE offset changed. MUST fail (the old
    # site goes stale AND the new one is unlisted).
    cat > "$d/moved.dis" << 'EOF'
ffffffff81000000 <native_sched_clock>:
ffffffff81000000:	90                   	nop
ffffffff81000001:	90                   	nop
ffffffff81000002:	90                   	nop
ffffffff81000003:	0f 31                	rdtsc
ffffffff81000005:	c3                   	ret
EOF
    if scan "$d/moved.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a removed+added rdtsc pair (count unchanged, site moved 0x0→0x3)" >&2
        echo "  was NOT caught — the per-site offset accounting is not working" >&2
        exit 1
    fi

    # Stale allowlist entry: MUST fail.
    printf 'native_sched_clock+0x0\nremoved_function+0x0\n' > "$d/stale.txt"
    if scan "$d/clean.dis" "$d/stale.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a stale allowlist entry was NOT caught" >&2
        exit 1
    fi

    # A bare, offset-less (function-granularity) entry: MUST be rejected as
    # malformed — it would weaken the gate back to per-function.
    printf 'native_sched_clock\n' > "$d/bare.txt"
    if scan "$d/clean.dis" "$d/bare.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a bare (offset-less) allowlist entry was NOT rejected" >&2
        exit 1
    fi

    # ARTIFACT QUALIFICATION (r4 P2): a `vmlinux:`-tagged allowlist entry must
    # not absolve a same-named site scanned under a different tag.
    cat > "$d/kernel.dis" << 'EOF'
ffffffff81000000 <startup_32>:
ffffffff81000000:	0f 31                	rdtsc
ffffffff81000002:	c3                   	ret
EOF
    printf 'vmlinux:startup_32+0x0\n' > "$d/tagged.txt"
    if ! scan_sites "$(sites "$d/kernel.dis" vmlinux)" "$d/tagged.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — the tagged clean fixture must pass" >&2
        exit 1
    fi
    if scan_sites "$(sites "$d/kernel.dis" other)" "$d/tagged.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a site tagged 'other' was absolved by the vmlinux allowlist" >&2
        echo "  entry for the same symbol name (artifact qualification broken)" >&2
        exit 1
    fi

    # HARDWARE-RNG CLASS: a reviewed rdrand site passes, a planted rdseed in a
    # non-allowlisted function fails, and the two mnemonic classes stay
    # separate — the counter scan must see NO rng site and vice versa.
    cat > "$d/rng.dis" << 'EOF'
ffffffff81000000 <x86_init_rdrand>:
ffffffff81000000:	48 0f c7 f0          	rdrand %rax
ffffffff81000004:	c3                   	ret
EOF
    printf 'vmlinux:x86_init_rdrand+0x0\n' > "$d/rng-allow.txt"
    if ! scan_sites "$(sites "$d/rng.dis" vmlinux '^(rdrand|rdseed)$')" \
        "$d/rng-allow.txt" hwrng >/dev/null 2>&1; then
        echo "FAIL: self-test — the clean rng fixture must pass" >&2
        exit 1
    fi
    cat > "$d/rng-planted.dis" << 'EOF'
ffffffff81000000 <x86_init_rdrand>:
ffffffff81000000:	48 0f c7 f0          	rdrand %rax
ffffffff81000004:	c3                   	ret
ffffffff81000010 <sneaky_seed>:
ffffffff81000010:	0f c7 f8             	rdseed %eax
ffffffff81000013:	c3                   	ret
EOF
    if scan_sites "$(sites "$d/rng-planted.dis" vmlinux '^(rdrand|rdseed)$')" \
        "$d/rng-allow.txt" hwrng >/dev/null 2>&1; then
        echo "FAIL: self-test — a planted rdseed in a non-allowlisted function was NOT caught" >&2
        exit 1
    fi
    if [ -n "$(sites "$d/rng.dis" vmlinux)" ]; then
        echo "FAIL: self-test — the counter scan matched an rng site (class separation broken)" >&2
        exit 1
    fi
    if [ -n "$(sites "$d/clean.dis" vmlinux '^(rdrand|rdseed)$')" ]; then
        echo "FAIL: self-test — the rng scan matched a counter site (class separation broken)" >&2
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
    echo "ok: scan self-test (planted-new, planted-prefixed, planted-inside-allowlisted, removed+added-pair, stale-entry, bare-entry, artifact-qualification, rng-class, ${raw_selftest:-}fixtures all caught)"
}

self_test

# ---- the real scan ----------------------------------------------------------
command -v objdump >/dev/null 2>&1 || {
    echo "FAIL: objdump not found (binutils required)" >&2
    exit 1
}
[ -f "$VMLINUX" ] || {
    echo "FAIL: $VMLINUX not found (scan the uncompressed vmlinux, not bzImage)" >&2
    exit 1
}
[ -f "$ALLOWLIST" ] || {
    echo "FAIL: allowlist $ALLOWLIST not found" >&2
    exit 1
}
[ -f "$RNG_ALLOWLIST" ] || {
    echo "FAIL: rng allowlist $RNG_ALLOWLIST not found" >&2
    exit 1
}

# (1) The kernel proper: symbol-attributed sites, artifact-qualified (r4 P2),
# one pass per mnemonic class against its own allowlist.
FOUND=$(all_sites)
RNG_FOUND=$(all_sites '^(rdrand|rdseed)$')

if unarmed "$ALLOWLIST"; then
    echo "###############################################################################" >&2
    echo "# FAIL: counter-opcode gate UNARMED ('# GATE-UNARMED' marker present in" >&2
    echo "# $ALLOWLIST) — a disarmed reachability gate never passes a build" >&2
    echo "# (fail-closed). Captured baseline, paste-ready after entry-by-entry review" >&2
    echo "# (commit it + REMOVE the marker to arm the gate):" >&2
    echo "###############################################################################" >&2
    printf '%s\n' "$FOUND" | sed 's/^/  /' >&2
    exit 1
fi
if unarmed "$RNG_ALLOWLIST"; then
    echo "###############################################################################" >&2
    echo "# FAIL: hardware-RNG gate UNARMED ('# GATE-UNARMED' marker present in" >&2
    echo "# $RNG_ALLOWLIST) — a disarmed reachability gate never passes a build" >&2
    echo "# (fail-closed). Captured baseline, paste-ready after entry-by-entry review" >&2
    echo "# (each site must sit behind X86_FEATURE_RDRAND/RDSEED; commit it + REMOVE" >&2
    echo "# the marker to arm the gate):" >&2
    echo "###############################################################################" >&2
    printf '%s\n' "$RNG_FOUND" | sed 's/^/  /' >&2
    exit 1
fi

if ! scan_sites "$FOUND" "$ALLOWLIST"; then
    exit 1
fi
if ! scan_sites "$RNG_FOUND" "$RNG_ALLOWLIST" hwrng; then
    exit 1
fi

# (2) Boot artifacts: fail-closed raw executable-byte scan for genuinely
# mode-mixed sections and a decoded zero-instruction scan for the decompressor's
# compiler-generated 64-bit `.text`, every build (r7 P2).
if ! raw_byte_scan; then
    exit 1
fi

N=$(sed -e 's/#.*$//' -e '/^[[:space:]]*$/d' "$ALLOWLIST" | wc -l | tr -d ' ')
NRNG=$(sed -e 's/#.*$//' -e '/^[[:space:]]*$/d' "$RNG_ALLOWLIST" | wc -l | tr -d ' ')
echo "ok: counter-opcode scan — kernel proper: every rdtsc/rdtscp site matches one of the $N reviewed per-site allowlist entries and every rdrand/rdseed site one of the $NRNG reviewed rng entries (symbol+offset); setup + decompressor: mixed-mode raw-byte and 64-bit decoded scans clean (${#RAW_ARTIFACTS[@]} artifacts, counter + rng opcodes)"
