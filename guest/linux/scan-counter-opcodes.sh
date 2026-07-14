#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Static counter-opcode scan of the built guest kernel — the x86 half of the
# PARAVIRT-CLOCK.md §3.3 reachability gate (the task-100 LL/SC-scan discipline
# transposed to counter reads: rdtsc `0F 31`, rdtscp `0F 01 F9`).
#
# WHAT IT PROVES on x86: every raw counter read left in the image is a KNOWN,
# REVIEWED site (the committed allowlist) — each survivable because the
# retained RDTSC/RDTSCP trap completes it with the same work-derived value
# (PARAVIRT-CLOCK.md §4.1). A site NOT in the allowlist fails the build: no
# new raw-counter path can slip into the image unreviewed. A stale allowlist
# entry (no matching site) fails too — the accounting is exact both ways.
# (On ARM, where no trap exists, the transposed gate has an EMPTY allowlist by
# necessity; that discipline is validated at spike stage AA-5, not here.)
#
# RUNTIME HALF — SPECCED AND STUBBED (stated per the task-110 evidence bar,
# not faked): the §3.3 ladder's third rung, W^X + rescan-on-exec (re-scanning
# any page the guest makes executable at runtime, so a JIT cannot mint a
# counter read the static scan never saw) needs vmm-side executable-page
# tracking — contract work that does not exist yet. Until it lands, the static
# scan covers exactly the built image + the no-modules config
# (CONFIG_MODULES is asserted off, so there is no loadable code either).
#
# Usage: scan-counter-opcodes.sh <vmlinux> [allowlist]
#   Scans the UNCOMPRESSED vmlinux ELF (disassembly needs symbols; the
#   compressed bzImage is not scannable). Defaults the allowlist to
#   rdtsc-allowlist.txt next to this script.
#
# SELF-TEST: every invocation first proves the gate can fail — a fixture with
# a planted rdtsc in a non-allowlisted function MUST be caught, and a stale
# allowlist entry MUST be caught — before the real image is scanned. A gate
# that cannot fail never passes anything (the PR-98/PR-108 vacuity bar).
set -euo pipefail

VMLINUX=${1:?usage: scan-counter-opcodes.sh <vmlinux> [allowlist]}
ALLOWLIST=${2:-"$(dirname "$0")/rdtsc-allowlist.txt"}

# scan <disasm-file> <allowlist-file>: 0 = clean, 1 = violations (printed).
# Pure text → text, so the self-test can drive it on fixtures.
scan() {
    local disasm=$1 allow=$2
    # Symbol-attributed counter-read sites: walk the objdump text, tracking
    # the enclosing function; record it when an instruction line's mnemonic
    # is exactly rdtsc/rdtscp (the mnemonic field, never a symbol-name or
    # byte-pattern substring match).
    local found
    found=$(awk '
        /^[0-9a-f]+ <[^>]+>:$/ {
            sym = $2; gsub(/[<>:]/, "", sym); next
        }
        /^[[:space:]]*[0-9a-f]+:\t/ {
            # objdump instruction line: addr:\tbytes\tmnemonic [operands]
            n = split($0, f, "\t")
            if (n >= 3) {
                mn = f[3]; sub(/[[:space:]].*$/, "", mn)
                if (mn == "rdtsc" || mn == "rdtscp") print sym
            }
        }
    ' "$disasm" | sort -u)
    # Reviewed allowlist entries (comments/blank lines stripped).
    local allowed
    allowed=$(sed -e 's/#.*$//' -e 's/[[:space:]]*$//' -e '/^$/d' "$allow" | sort -u)
    local bad=0
    # Any found site not in the allowlist → violation.
    local unlisted
    unlisted=$(comm -23 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed" | sed '/^$/d'))
    if [ -n "$unlisted" ]; then
        echo "FAIL: raw counter read (rdtsc/rdtscp) in NON-allowlisted function(s):" >&2
        printf '%s\n' "$unlisted" | sed 's/^/  /' >&2
        echo "  Review each site: if it is a legitimate trap-backstopped path, add the" >&2
        echo "  function name to $ALLOWLIST with a justification comment; if it is new" >&2
        echo "  timekeeping code, route it through the harmony pvclock page instead." >&2
        bad=1
    fi
    # Any allowlist entry with no matching site → stale accounting.
    local stale
    stale=$(comm -13 <(printf '%s\n' "$found" | sed '/^$/d') \
        <(printf '%s\n' "$allowed" | sed '/^$/d'))
    if [ -n "$stale" ]; then
        echo "FAIL: stale allowlist entr(ies) — no matching rdtsc/rdtscp site in the image:" >&2
        printf '%s\n' "$stale" | sed 's/^/  /' >&2
        echo "  Remove them from $ALLOWLIST (exact accounting, both directions)." >&2
        bad=1
    fi
    return $bad
}

# ---- self-test (every invocation): the gate must be able to FAIL -----------
self_test() {
    local d
    d=$(mktemp -d)
    trap 'rm -rf "$d"' RETURN

    # Fixture disassembly: one allowlisted rdtsc site, one clean function, one
    # function whose NAME contains "rdtsc" but executes none (must not match),
    # and one call whose OPERAND mentions an rdtsc-named symbol (must not match).
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
    printf 'native_sched_clock\n' > "$d/allow.txt"
    if ! scan "$d/clean.dis" "$d/allow.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — the clean fixture must pass" >&2
        exit 1
    fi

    # Planted rdtsc in a non-allowlisted function: the gate MUST fail.
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

    # Stale allowlist entry: the gate MUST fail.
    printf 'native_sched_clock\nremoved_function\n' > "$d/stale.txt"
    if scan "$d/clean.dis" "$d/stale.txt" >/dev/null 2>&1; then
        echo "FAIL: self-test — a stale allowlist entry was NOT caught" >&2
        exit 1
    fi
    echo "ok: scan self-test (planted-opcode + stale-entry fixtures both caught)"
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

if scan "$DIS" "$ALLOWLIST"; then
    N=$(sed -e 's/#.*$//' -e '/^[[:space:]]*$/d' "$ALLOWLIST" | wc -l | tr -d ' ')
    echo "ok: counter-opcode scan — every rdtsc/rdtscp site is one of the $N reviewed allowlist entries"
else
    exit 1
fi
