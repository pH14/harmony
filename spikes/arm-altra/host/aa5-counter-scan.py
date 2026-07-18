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
import importlib.util
import re
import subprocess
import sys
from pathlib import Path

OP2 = {0: "CNTFRQ_EL0", 1: "CNTPCT_EL0", 2: "CNTVCT_EL0", 5: "CNTPCTSS_EL0", 6: "CNTVCTSS_EL0"}
LIVE_COUNTERS = {"CNTPCT_EL0", "CNTVCT_EL0", "CNTPCTSS_EL0", "CNTVCTSS_EL0"}
TIMER_PROGRAMS = {
    (2, 0): "CNTP_TVAL_EL0",
    (2, 2): "CNTP_CVAL_EL0",
    (3, 0): "CNTV_TVAL_EL0",
    (3, 2): "CNTV_CVAL_EL0",
}


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


def decode_timer_program(word: int):
    """Return a live-domain TVAL/CVAL target for an MSR, else None."""
    if word & 0xFFF00000 != 0xD5100000:
        return None
    o0 = (word >> 19) & 0x1
    op1 = (word >> 16) & 0x7
    crn = (word >> 12) & 0xF
    crm = (word >> 8) & 0xF
    op2 = (word >> 5) & 0x7
    if o0 != 1 or op1 != 3 or crn != 14:
        return None
    return TIMER_PROGRAMS.get((crm, op2))


def executable_words(path: str):
    """Use the AA-4 scanner's hardened ELF64 executable-section byte walk."""
    parser_path = Path(__file__).with_name("aa4-exclusive-scan.py")
    spec = importlib.util.spec_from_file_location("harmony_aa4_exec_parser", parser_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"SCANNER ELF ERROR: cannot load {parser_path}")
    parser = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(parser)
    return parser.executable_words(path)


def scan(path: str):
    words = executable_words(path)
    dz = subprocess.run(["objdump", "-d", path], capture_output=True, text=True, check=True).stdout
    line_re = re.compile(r"^\s*([0-9a-f]+):\s+([0-9a-f]{8})\s+(.*)$")
    decoded = {}
    disagreements = []
    for line in dz.splitlines():
        m = line_re.match(line)
        if not m:
            continue
        addr, raw, asm = m.group(1), m.group(2), m.group(3).strip()
        word = int(raw, 16)
        low = asm.lower()
        counter = decode_counter_read(word)
        timer = decode_timer_program(word)
        mnem_counter = low.startswith("mrs") and bool(
            re.search(r"cnt(frq|pct|vct)(ss)?_el0", low)
        )
        mnem_timer = low.startswith("msr") and bool(
            re.search(r"cnt[vp]_(tval|cval)_el0", low)
        )
        if (counter is not None) != mnem_counter or (timer is not None) != mnem_timer:
            disagreements.append(addr)
        decoded[int(addr, 16)] = (word, asm)

    if not decoded:
        raise SystemExit(f"SCANNER SELF-CHECK FAILED for {path}: objdump parsed zero instructions")
    if disagreements:
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: decoder disagrees at {disagreements}"
        )
    raw_by_address = {address: word for address, word, _section in words}
    mismatches = [
        f"{address:x}"
        for address, (word, _asm) in decoded.items()
        if raw_by_address.get(address) != word
    ]
    if mismatches:
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: ELF/objdump mismatch at {mismatches}"
        )

    reads = []
    timer_programs = []
    for address, word, section in words:
        asm = decoded.get(address, (word, f"<raw executable word in {section}>"))[1]
        counter = decode_counter_read(word)
        if counter:
            reads.append((f"{address:x}", f"{word:08x}", asm, counter))
        timer = decode_timer_program(word)
        if timer:
            timer_programs.append((f"{address:x}", f"{word:08x}", asm, timer))
    return reads, timer_programs


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: aa5-counter-scan.py <aarch64-elf> [<elf> ...]")
    any_live = False
    for path in sys.argv[1:]:
        reads, timer_programs = scan(path)
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
            print(f"[CLEAN]  {name}: no live generic-timer counter reads{note} (raw ELF/mask self-check passed)")
        if timer_programs:
            any_live = True
            print(f"[REJECT] {name}: {len(timer_programs)} live-domain timer program(s)")
            for addr, raw, asm, reg in timer_programs:
                print(f"    {addr}: {raw}  {asm}   [{reg}]")
        else:
            print(f"[CLEAN]  {name}: no CNTV/CNTP CVAL/TVAL programs (raw ELF/mask self-check passed)")
    sys.exit(1 if any_live else 0)


if __name__ == "__main__":
    main()
