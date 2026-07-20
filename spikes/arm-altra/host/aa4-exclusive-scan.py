#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-4(c) level-2: the executable-page exclusive-opcode scan. This is the primitive a W^X
# rescan-on-exec would run to enforce "no LL/SC in the guest": scan the raw instruction words of
# an aarch64 image's executable sections for the load/store-EXCLUSIVE family and reject any hit.
#
# The LL/SC exclusive family (monitor-based): LDXR/LDXRB/LDXRH/LDAXR.., STXR/STLXR..,
# LDXP/STXP/LDAXP/STLXP. In the "Load/store exclusive" encoding all share bits[29:24]=001000 with
# o2 (bit 23) = 0 -- the monitor bit. That is  (insn & 0x3f800000) == 0x08000000 . It deliberately
# EXCLUDES the acquire/release non-exclusives LDAR/STLR (o2=1) and the LSE compare-and-swap CAS*
# (o2=1, o1=1): those carry no reservation monitor and are deterministic, so an LSE-only build is
# clean under this mask by construction.
#
# The scan operates on RAW bytes (what a page rescan sees), then SELF-VALIDATES against
# `objdump -d`: every mask hit must disassemble to an exclusive mnemonic, and every exclusive
# mnemonic objdump reports must be a mask hit. A disagreement fails the scan (the mask is wrong),
# so the primitive cannot silently under- or over-report.
import re
import subprocess
import sys

EXCL_MASK = 0x3F800000
EXCL_MATCH = 0x08000000
# Mnemonics objdump prints for the monitor-based exclusive family (for cross-validation).
EXCL_MNEMONICS = re.compile(r"\b(ld(a?)xr[bh]?|st(l?)xr[bh]?|ld(a?)xp|st(l?)xp|casp?)\b")
# CASP is the LSE exclusive-pair compare-and-swap; it is LSE (no monitor) -- keep it OUT of the
# ban set. The regex above lists it only so the cross-check can classify it explicitly.
LSE_OK = re.compile(r"\bcas[ablph]*\b")


def is_llsc_exclusive(word: int) -> bool:
    return (word & EXCL_MASK) == EXCL_MATCH


def scan(path: str):
    # `objdump -d` gives address, raw word, and mnemonic in one pass: parse the word for the raw
    # scan and keep the mnemonic for the self-check.
    out = subprocess.run(
        ["objdump", "-d", "--no-show-raw-insn", path],
        capture_output=True, text=True, check=True,
    ).stdout
    dz = subprocess.run(
        ["objdump", "-d", path], capture_output=True, text=True, check=True
    ).stdout

    hits = []          # mask says LL/SC exclusive
    mnem_excl = []     # objdump mnemonic says exclusive-monitor
    line_re = re.compile(r"^\s*([0-9a-f]+):\s+([0-9a-f]{8})\s+(.*)$")
    for line in dz.splitlines():
        m = line_re.match(line)
        if not m:
            continue
        addr, raw, asm = m.group(1), m.group(2), m.group(3).strip()
        # objdump prints the raw word in target-endian text (already big-endian display of the
        # 32-bit value), so int(raw,16) is the instruction word directly.
        word = int(raw, 16)
        mnem = asm.split()[0] if asm else ""
        mask_hit = is_llsc_exclusive(word)
        # An exclusive-monitor mnemonic per objdump: starts with ldxr/stxr/ldaxr/stlxr/ldxp/stxp..
        mnem_is_excl = bool(re.match(r"(ld(a)?xr[bh]?|st(l)?xr[bh]?|ld(a)?xp|st(l)?xp)$", mnem))
        if mask_hit:
            hits.append((addr, raw, asm))
        if mnem_is_excl:
            mnem_excl.append((addr, raw, asm))

    # Self-validation: the raw mask and the disassembler must agree exactly.
    hit_addrs = {h[0] for h in hits}
    mnem_addrs = {h[0] for h in mnem_excl}
    if hit_addrs != mnem_addrs:
        only_mask = sorted(hit_addrs - mnem_addrs)
        only_mnem = sorted(mnem_addrs - hit_addrs)
        raise SystemExit(
            f"SCANNER SELF-CHECK FAILED for {path}: mask-only={only_mask} mnemonic-only={only_mnem}"
        )
    return hits


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: aa4-exclusive-scan.py <aarch64-elf> [<elf> ...]")
    any_hits = False
    for path in sys.argv[1:]:
        hits = scan(path)
        name = path.split("/")[-1]
        if hits:
            any_hits = True
            print(f"[BANNED] {name}: {len(hits)} LL/SC exclusive instruction(s)")
            for addr, raw, asm in hits:
                print(f"    {addr}: {raw}  {asm}")
        else:
            print(f"[CLEAN]  {name}: no LL/SC exclusive instructions (mask self-check passed)")
    # Exit 1 if any scanned image carries an exclusive -- a rescan-on-exec would reject it.
    sys.exit(1 if any_hits else 0)


if __name__ == "__main__":
    main()
