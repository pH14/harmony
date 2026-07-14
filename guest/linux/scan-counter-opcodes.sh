#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Static counter-opcode scan of the built guest kernel — the x86 half of the
# PARAVIRT-CLOCK.md §3.3 reachability gate (the task-100 LL/SC-scan discipline
# transposed to counter reads: rdtsc `0F 31`, rdtscp `0F 01 F9`).
#
# WHAT IT PROVES on x86: every raw counter read left in the image is a KNOWN,
# REVIEWED site — the committed allowlist records each containing function AND
# its exact instruction count, so a NEW rdtsc added inside an already-reviewed
# function is caught (its count moves), as is a removed one (the entry goes
# stale). Each is allowlistable ONLY because the retained RDTSC/RDTSCP trap
# completes it with the same work-derived value the pvclock page carries
# (§4.1 — on x86 a raw read is survivable-by-trap, never a determinism hole).
# Exact accounting in both directions: an unlisted (or count-changed) site
# fails the build; a stale entry fails too. (On ARM, where no trap exists, the
# transposed gate has an EMPTY allowlist by necessity; that discipline is
# validated at spike stage AA-5, not here.)
#
# ARMING: while the allowlist carries a `# GATE-UNARMED` marker line, the
# scan runs in CAPTURE mode — it prints every found site in paste-ready
# `function count` form under a loud banner and then **FAILS the build**
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
# Usage: scan-counter-opcodes.sh <vmlinux> [allowlist]
#   Scans the UNCOMPRESSED vmlinux ELF (disassembly needs symbols; the
#   compressed bzImage is not scannable). Defaults the allowlist to
#   rdtsc-allowlist.txt next to this script.
set -euo pipefail

VMLINUX=${1:?usage: scan-counter-opcodes.sh <vmlinux> [allowlist]}
ALLOWLIST=${2:-"$(dirname "$0")/rdtsc-allowlist.txt"}

# sites <disasm-file>: emit "function count" per function containing rdtsc/
# rdtscp instructions (mnemonic-field match — never a symbol-name or
# byte-pattern substring), sorted by name.
sites() {
    awk '
        /^[0-9a-f]+ <[^>]+>:$/ {
            sym = $2; gsub(/[<>:]/, "", sym); next
        }
        /^[[:space:]]*[0-9a-f]+:\t/ {
            # objdump instruction line: addr:\tbytes\tmnemonic [operands]
            n = split($0, f, "\t")
            if (n >= 3) {
                mn = f[3]; sub(/[[:space:]].*$/, "", mn)
                if (mn == "rdtsc" || mn == "rdtscp") count[sym]++
            }
        }
        END { for (s in count) print s, count[s] }
    ' "$1" | sort
}

# allowed <allowlist-file>: emit the reviewed "function count" entries
# (comments/blank lines stripped), sorted; FAIL on a count-less entry (the
# per-instruction accounting is the gate — a bare name would silently weaken
# it back to function granularity).
allowed() {
    local entries
    entries=$(sed -e 's/#.*$//' -e 's/[[:space:]]*$//' -e '/^$/d' "$1")
    if [ -n "$entries" ] && ! printf '%s\n' "$entries" \
        | awk 'NF != 2 || $2 !~ /^[0-9]+$/ { bad = 1 } END { exit bad }'; then
        echo "FAIL: malformed allowlist entry — every entry is 'function count' (the" >&2
        echo "  per-instruction accounting); offending line(s):" >&2
        printf '%s\n' "$entries" | awk 'NF != 2 || $2 !~ /^[0-9]+$/' | sed 's/^/  /' >&2
        return 2
    fi
    printf '%s\n' "$entries" | sed '/^$/d' | sort
}

# scan <disasm-file> <allowlist-file>: 0 = clean, 1 = violations (printed).
# Pure text → text, so the self-test can drive it on fixtures.
scan() {
    local disasm=$1 allow=$2
    local found allowed_entries bad=0
    found=$(sites "$disasm")
    allowed_entries=$(allowed "$allow") || return 2
    local unlisted stale
    unlisted=$(comm -23 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed_entries" | sed '/^$/d'))
    stale=$(comm -13 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed_entries" | sed '/^$/d'))
    if [ -n "$unlisted" ]; then
        echo "FAIL: raw counter read(s) (rdtsc/rdtscp) not matching the allowlist" >&2
        echo "  (new function, or the instruction COUNT changed inside a reviewed one):" >&2
        printf '%s\n' "$unlisted" | sed 's/^/  /' >&2
        echo "  Review each: if it is a legitimate trap-backstopped path, record" >&2
        echo "  'function count' in $ALLOWLIST with a justification comment; if it is" >&2
        echo "  new timekeeping code, route it through the harmony pvclock page instead." >&2
        bad=1
    fi
    if [ -n "$stale" ]; then
        echo "FAIL: stale allowlist entr(ies) — no matching site/count in the image:" >&2
        printf '%s\n' "$stale" | sed 's/^/  /' >&2
        echo "  Update or remove them in $ALLOWLIST (exact accounting, both directions)." >&2
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
    printf 'native_sched_clock 1\n' > "$d/allow.txt"
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

    # A SECOND rdtsc planted inside an ALREADY-allowlisted function (count
    # 1 → 2): MUST fail — the per-instruction accounting (cross-model r1 P1).
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

    # Stale allowlist entry: MUST fail.
    printf 'native_sched_clock 1\nremoved_function 1\n' > "$d/stale.txt"
    if scan "$d/clean.dis" "$d/stale.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a stale allowlist entry was NOT caught" >&2
        exit 1
    fi

    # A count-less (function-granularity) entry: MUST be rejected as malformed.
    printf 'native_sched_clock\n' > "$d/bare.txt"
    if scan "$d/clean.dis" "$d/bare.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a count-less allowlist entry was NOT rejected" >&2
        exit 1
    fi
    echo "ok: scan self-test (planted-new, planted-inside-allowlisted, stale-entry, and bare-entry fixtures all caught)"
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

DIS=$(mktemp)
trap 'rm -f "$DIS"' EXIT
objdump -d "$VMLINUX" > "$DIS"

if unarmed "$ALLOWLIST"; then
    echo "###############################################################################" >&2
    echo "# FAIL: counter-opcode gate UNARMED ('# GATE-UNARMED' marker present in" >&2
    echo "# $ALLOWLIST) — a disarmed reachability gate never passes a build" >&2
    echo "# (fail-closed). Captured baseline, paste-ready after entry-by-entry review" >&2
    echo "# (commit it + REMOVE the marker to arm the gate):" >&2
    echo "###############################################################################" >&2
    sites "$DIS" | sed 's/^/  /' >&2
    exit 1
fi

if scan "$DIS" "$ALLOWLIST"; then
    N=$(sed -e 's/#.*$//' -e '/^[[:space:]]*$/d' "$ALLOWLIST" | wc -l | tr -d ' ')
    echo "ok: counter-opcode scan — every rdtsc/rdtscp site matches one of the $N reviewed allowlist entries (function + count)"
else
    exit 1
fi
