#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-5(b) closure scan: the counter-read opcode scan. FEAT_ECV is ABSENT on N1 (AA-0:
# ID_AA64MMFR0_EL1.ECV = 0x0), so the guest's CNTVCT_EL0 cannot be trapped in hardware — raw
# counter access must be closed at the CONTRACT level. One layer of that closure is a build-time
# / rescan-on-exec scan that rejects any image emitting an MRS of a generic-timer counter, so the
# guest reads time ONLY via the work-derived clock page.
#
#   aa5-counter-scan.py <aarch64-elf> [<elf> ...]
#
# This mirrors the harness primitive `scan::decode_counter_read` (harness/src/scan.rs, unit
# tested): MRS Xt, <sysreg> is 0xD53_xxxxx, the counter registers live at op0=3,op1=3,CRn=14,
# CRm=0, distinguished by op2 (0=CNTFRQ 1=CNTPCT 2=CNTVCT 5=CNTPCTSS 6=CNTVCTSS). The raw-opcode
# decode SELF-VALIDATES against objdump's mnemonic on every instruction, so it can neither miss a
# counter read nor invent one. CNTFRQ_EL0 (a constant frequency register, not a live counter) is
# reported but does NOT trip the reject: reading the frequency is deterministic; reading the
# COUNT is the hazard.
import re
import subprocess
import sys

OP2 = {0: "CNTFRQ_EL0", 1: "CNTPCT_EL0", 2: "CNTVCT_EL0", 5: "CNTPCTSS_EL0", 6: "CNTVCTSS_EL0"}
LIVE_COUNTERS = {"CNTPCT_EL0", "CNTVCT_EL0", "CNTPCTSS_EL0", "CNTVCTSS_EL0"}


def decode_counter_read(word: int):
    """Return the counter register name for an MRS of one, else None (mirrors scan::decode_counter_read)."""
    if word & 0xFFF00000 != 0xD5300000:
        return None
    o0 = (word >> 19) & 0x1  # op0 - 2
    op1 = (word >> 16) & 0x7
    crn = (word >> 12) & 0xF
    crm = (word >> 8) & 0xF
    op2 = (word >> 5) & 0x7
    if o0 != 1 or op1 != 3 or crn != 14 or crm != 0:
        return None
    return OP2.get(op2)


def scan(path: str):
    dz = subprocess.run(["objdump", "-d", path], capture_output=True, text=True, check=True).stdout
    line_re = re.compile(r"^\s*([0-9a-f]+):\s+([0-9a-f]{8})\s+(.*)$")
    reads = []          # (addr, raw, asm, reg)  raw-decode hits
    mnem_cnt = []       # objdump mnemonic says a counter MRS
    for line in dz.splitlines():
        m = line_re.match(line)
        if not m:
            continue
        addr, raw, asm = m.group(1), m.group(2), m.group(3).strip()
        word = int(raw, 16)
        reg = decode_counter_read(word)
        if reg:
            reads.append((addr, raw, asm, reg))
        # objdump renders these as `mrs x0, cntvct_el0` etc.
        low = asm.lower()
        if low.startswith("mrs") and re.search(r"cnt(frq|pct|vct)(ss)?_el0", low):
            mnem_cnt.append((addr, raw, asm))

    # Self-validation: raw decode must agree with the disassembler.
    if {r[0] for r in reads} != {c[0] for c in mnem_cnt}:
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: raw={sorted(r[0] for r in reads)} "
            f"objdump={sorted(c[0] for c in mnem_cnt)}"
        )
    return reads


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: aa5-counter-scan.py <aarch64-elf> [<elf> ...]")
    any_live = False
    for path in sys.argv[1:]:
        reads = scan(path)
        name = path.split("/")[-1]
        live = [r for r in reads if r[3] in LIVE_COUNTERS]
        freq = [r for r in reads if r[3] not in LIVE_COUNTERS]
        if live:
            any_live = True
            print(f"[REJECT] {name}: {len(live)} live counter read(s) — closure requires the clock page, not the counter")
            for addr, raw, asm, reg in live:
                print(f"    {addr}: {raw}  {asm}   [{reg}]")
        else:
            note = f" ({len(freq)} CNTFRQ_EL0 constant-freq read(s), allowed)" if freq else ""
            print(f"[CLEAN]  {name}: no live generic-timer counter reads{note} (mask self-check passed)")
    sys.exit(1 if any_live else 0)


if __name__ == "__main__":
    main()
