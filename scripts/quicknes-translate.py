#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Translate a ROM's profiled 6502 code into C++ blocks for the patched QuickNES CPU."""

import argparse
import collections
import hashlib
from pathlib import Path
import sys
PAGE = 2048

CLOCK = [
    7,6,2,8,3,3,5,5,3,2,2,2,4,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,4,2,2,2,4,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,3,2,2,2,3,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,4,2,2,2,5,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    2,6,2,6,3,3,3,3,2,2,2,2,4,4,4,4,
    3,6,2,6,4,4,4,4,2,5,2,5,5,5,5,5,
    2,6,2,6,3,3,3,3,2,2,2,2,4,4,4,4,
    3,5,2,5,4,4,4,4,2,4,2,4,4,4,4,4,
    2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6,
    3,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
]

MODE_LEN = {"imp": 1, "imm": 2, "zp": 2, "zpx": 2, "zpy": 2, "abs": 3, "abx": 3, "aby": 3, "inx": 2, "iny": 2, "rel": 2}

OPS = {}


def op(code, name, mode):
    assert code not in OPS
    OPS[code] = (name, mode)


for name, base in (("CMP", 0xC5), ("AND", 0x25), ("EOR", 0x45), ("ORA", 0x05), ("SBC", 0xE5), ("ADC", 0x65)):
    for mode, delta in (("inx", -4), ("iny", 0x0C), ("zpx", 0x10), ("zp", 0), ("aby", 0x14), ("abx", 0x18), ("abs", 0x08), ("imm", 0x04)):
        op(base + delta, name, mode)
for code, mode in ((0xA9, "imm"), (0xA5, "zp"), (0xB5, "zpx"), (0xAD, "abs"), (0xBD, "abx"), (0xB9, "aby"), (0xB1, "iny"), (0xA1, "inx")):
    op(code, "LDA", mode)
for code, mode in ((0xA2, "imm"), (0xA6, "zp"), (0xB6, "zpy"), (0xAE, "abs"), (0xBE, "aby")):
    op(code, "LDX", mode)
for code, mode in ((0xA0, "imm"), (0xA4, "zp"), (0xB4, "zpx"), (0xAC, "abs"), (0xBC, "abx")):
    op(code, "LDY", mode)
for code, mode in ((0x85, "zp"), (0x95, "zpx"), (0x8D, "abs"), (0x9D, "abx"), (0x99, "aby"), (0x91, "iny"), (0x81, "inx")):
    op(code, "STA", mode)
for code, mode in ((0x86, "zp"), (0x96, "zpy"), (0x8E, "abs")):
    op(code, "STX", mode)
for code, mode in ((0x84, "zp"), (0x94, "zpx"), (0x8C, "abs")):
    op(code, "STY", mode)
for code, mode in ((0xE0, "imm"), (0xE4, "zp"), (0xEC, "abs")):
    op(code, "CPX", mode)
for code, mode in ((0xC0, "imm"), (0xC4, "zp"), (0xCC, "abs")):
    op(code, "CPY", mode)
op(0x24, "BIT", "zp")
op(0x2C, "BIT", "abs")
for code, name in ((0xE8, "INX"), (0xC8, "INY"), (0xCA, "DEX"), (0x88, "DEY"), (0xAA, "TAX"), (0xA8, "TAY"),
                   (0x8A, "TXA"), (0x98, "TYA"), (0xBA, "TSX"), (0x9A, "TXS"), (0x48, "PHA"), (0x68, "PLA"),
                   (0x08, "PHP"), (0x18, "CLC"), (0x38, "SEC"), (0xB8, "CLV"), (0xD8, "CLD"), (0xF8, "SED"),
                   (0xEA, "NOP"), (0x4A, "LSRA"), (0x6A, "RORA"), (0x0A, "ASLA"), (0x2A, "ROLA"), (0x60, "RTS")):
    op(code, name, "imp")
for code, name, mode in ((0x0E, "ASL", "abs"), (0x2E, "ROL", "abs"), (0x1E, "ASL", "abx"), (0x3E, "ROL", "abx"),
                         (0x4E, "LSR", "abs"), (0x6E, "ROR", "abs"), (0x5E, "LSR", "abx"), (0x7E, "ROR", "abx"),
                         (0x06, "ASL", "zp"), (0x26, "ROL", "zp"), (0x16, "ASL", "zpx"), (0x36, "ROL", "zpx"),
                         (0x46, "LSR", "zp"), (0x66, "ROR", "zp"), (0x56, "LSR", "zpx"), (0x76, "ROR", "zpx"),
                         (0xE6, "INC", "zp"), (0xF6, "INC", "zpx"), (0xEE, "INC", "abs"), (0xFE, "INC", "abx"),
                         (0xC6, "DEC", "zp"), (0xD6, "DEC", "zpx"), (0xCE, "DEC", "abs"), (0xDE, "DEC", "abx"),
                         (0x20, "JSR", "abs"), (0x4C, "JMP", "abs")):
    op(code, name, mode)
BRANCHES = {0x10: "!IS_NEG", 0x30: "IS_NEG", 0x50: "!(status & st_v)", 0x70: "(status & st_v)",
            0x90: "!(c & 0x100)", 0xB0: "(c & 0x100)", 0xD0: "(uint8_t) nz", 0xF0: "!(uint8_t) nz"}
for code in BRANCHES:
    op(code, "BRANCH", "rel")

CONTROL = {"JSR", "JMP", "RTS", "BRANCH"}
PAGE_CROSS = {("LDA", "abx"), ("LDA", "aby"), ("LDA", "iny"), ("LDX", "aby"), ("LDY", "abx")}
for n in ("CMP", "AND", "EOR", "ORA", "SBC", "ADC"):
    for m in ("abx", "aby", "iny"):
        PAGE_CROSS.add((n, m))


def length(code):
    return MODE_LEN[OPS[code][1]]


def safe_read(lo, hi):
    return lo >= 0 and (hi < 0x2000 or (lo > 0x7FFF and hi <= 0xFFFF))


def safe_write(lo, hi):
    return lo >= 0 and hi < 0x2000


def instr_body(code, lo, hi):
    name, mode = OPS[code]
    a16 = lo | hi << 8
    H = "0x%X" % hi
    A16 = "0x%X" % a16
    adc = ("{ int carry = (c >> 8) & 1; int ov = (a ^ 0x80) + carry + (int8_t) data; status &= ~st_v; "
           "status |= (ov >> 2) & 0x40; c = nz = a + data + carry; a = (uint8_t) nz; }")
    if name in ("CMP", "AND", "EOR", "ORA", "SBC", "ADC"):
        safe = True
        if mode == "imm":
            f = ""
        elif mode == "zp":
            f = "data = READ_LOW( data );"
        elif mode == "zpx":
            f = "data = uint8_t (data + x); data = READ_LOW( data );"
        elif mode == "abs":
            safe = safe_read(a16, a16)
            f = "data += 0x100 * %s; data = READ( data );" % H
        elif mode in ("abx", "aby"):
            idx = "x" if mode == "abx" else "y"
            safe = safe_read(a16 - 0x100, a16 + 0xFF)
            f = ("data += %s; HANDLE_PAGE_CROSSING( data ); { int temp = data; data += 0x100 * %s; "
                 "if ( temp & 0x100 ) READ( data - 0x100 ); } data = READ( data );") % (idx, H)
        elif mode == "inx":
            safe = False
            f = ("{ int temp = data + x; data = 0x100 * READ_LOW( uint8_t (temp + 1) ) + READ_LOW( uint8_t (temp) ); } "
                 "data = READ( data );")
        else:
            safe = False
            f = ("{ int temp = READ_LOW( data ) + y; data = temp + 0x100 * READ_LOW( uint8_t (data + 1) ); "
                 "HANDLE_PAGE_CROSSING( temp ); if ( temp & 0x100 ) READ( data - ( temp & 0x100 ) ); } data = READ( data );")
        tail = {"CMP": "nz = a - data; c = ~nz; nz &= 0xFF;", "AND": "nz = (a &= data);", "EOR": "nz = (a ^= data);",
                "ORA": "nz = (a |= data);", "SBC": "data ^= 0xFF; " + adc, "ADC": adc}[name]
        return f + " " + tail, safe
    if name == "LDA":
        if mode == "imm":
            return "a = data; nz = data;", True
        if mode == "zp":
            return "a = nz = READ_LOW( data );", True
        if mode == "zpx":
            return "data = uint8_t (data + x); a = nz = READ_LOW( data );", True
        if mode == "abs":
            return "a = nz = READ_LIKELY_PPU( %s );" % A16, (a16 < 0x2000 or a16 > 0x7FFF)
        if mode in ("abx", "aby"):
            idx = "x" if mode == "abx" else "y"
            return (("{ unsigned msb = %s; data += %s; HANDLE_PAGE_CROSSING( data ); int temp = data; data += msb * 0x100; "
                     "a = nz = READ_PROG( uint16_t( data ) ); if ( (unsigned) (data - 0x2000) < 0x6000 ) { "
                     "if ( temp & 0x100 ) READ( data - 0x100 ); a = nz = READ( data ); } }") % (H, idx),
                    (a16 + 0xFF < 0x2000 or (a16 > 0x7FFF and a16 + 0xFF <= 0xFFFF)))
        if mode == "iny":
            return ("{ unsigned msb = READ_LOW( (uint8_t) (data + 1) ); data = READ_LOW( data ) + y; HANDLE_PAGE_CROSSING( data ); "
                    "int temp = data; data += msb * 0x100; a = nz = READ_PROG( uint16_t( data ) ); "
                    "if ( (unsigned) (data - 0x2000) < 0x6000 ) { if ( temp & 0x100 ) READ( data - 0x100 ); a = nz = READ( data ); } }"), False
        return ("{ int temp = data + x; data = 0x100 * READ_LOW( uint8_t (temp + 1) ) + READ_LOW( uint8_t (temp) ); } "
                "a = nz = READ( data );"), False
    if name in ("LDX", "LDY"):
        r = name[2].lower()
        if mode == "imm":
            return "%s = data; nz = data;" % r, True
        if mode == "zp":
            return "data = READ_LOW( data ); %s = data; nz = data;" % r, True
        if mode in ("zpx", "zpy"):
            idx = "x" if mode == "zpx" else "y"
            return "data = uint8_t (data + %s); data = READ_LOW( data ); %s = data; nz = data;" % (idx, r), True
        if mode == "abs":
            return ("{ unsigned addr = data + 0x100 * %s; if ( data & 0x100 ) READ( addr - 0x100 ); %s = nz = READ( addr ); }"
                    % (H, r)), safe_read(a16, a16)
        idx = "x" if mode == "abx" else "y"
        return ("data += %s; HANDLE_PAGE_CROSSING( data ); { unsigned addr = data + 0x100 * %s; "
                "if ( data & 0x100 ) READ( addr - 0x100 ); %s = nz = READ( addr ); }" % (idx, H, r)), \
            safe_read(a16 - 0x100, a16 + 0xFF)
    if name in ("STA", "STX", "STY"):
        r = name[2].lower()
        if mode == "zp":
            return "WRITE_LOW( data, %s );" % r, True
        if mode in ("zpx", "zpy"):
            idx = "x" if mode == "zpx" else "y"
            return "data = uint8_t (data + %s); WRITE_LOW( data, %s );" % (idx, r), True
        if mode == "abs":
            if name == "STA":
                return "data += 0x100 * %s; WRITE( data, a );" % H, safe_write(a16, a16)
            return "WRITE( %s, %s );" % (A16, r), safe_write(a16, a16)
        if mode in ("abx", "aby"):
            idx = "x" if mode == "abx" else "y"
            return ("data += %s; { int temp = data; data += 0x100 * %s; READ( data - ( temp & 0x100 ) ); } WRITE( data, a );"
                    % (idx, H)), safe_write(a16 - 0x100, a16 + 0xFF)
        if mode == "iny":
            return ("{ int temp = READ_LOW( data ) + y; data = temp + 0x100 * READ_LOW( uint8_t (data + 1) ); "
                    "READ( data - ( temp & 0x100 ) ); } WRITE( data, a );"), False
        return ("{ int temp = data + x; data = 0x100 * READ_LOW( uint8_t (temp + 1) ) + READ_LOW( uint8_t (temp) ); } "
                "WRITE( data, a );"), False
    if name in ("CPX", "CPY"):
        r = name[2].lower()
        f = {"imm": "", "zp": "data = READ_LOW( data );", "abs": "data = READ( %s );" % A16}[mode]
        return f + " nz = %s - data; c = ~nz; nz &= 0xFF;" % r, (mode != "abs" or safe_read(a16, a16))
    if name == "BIT":
        if mode == "zp":
            return ("nz = READ_LOW( data ); status &= ~st_v; status |= nz & st_v; "
                    "if ( !(a & nz) ) nz = nz << 4 & 0x800;"), True
        return ("status &= ~st_v; nz = READ_LIKELY_PPU( %s ); status |= nz & st_v; "
                "if ( !(a & nz) ) nz = nz << 4 & 0x800;" % A16), (a16 < 0x2000 or a16 > 0x7FFF)
    simple = {
        "INX": "x = uint8_t (nz = x + 1);", "INY": "y = uint8_t (nz = y + 1);",
        "DEX": "x = uint8_t (nz = x + -1);", "DEY": "y = uint8_t (nz = y + -1);",
        "TAX": "x = a; a = nz = x;", "TAY": "y = a; a = nz = y;", "TXA": "a = nz = x;", "TYA": "a = nz = y;",
        "TSX": "x = nz = GET_SP();", "TXS": "SET_SP( x );", "PHA": "PUSH( a );",
        "PLA": "a = nz = READ_LOW( sp ); sp = (sp - 0xFF) | 0x100;",
        "PHP": "{ int temp; CALC_STATUS( temp ); PUSH( temp | st_b | st_r ); }",
        "CLC": "c = 0;", "SEC": "c = ~0;", "CLV": "status &= ~st_v;", "CLD": "status &= ~st_d;", "SED": "status |= st_d;",
        "NOP": "",
        "LSRA": "c = 0; nz = (c >> 1) & 0x80; c = a << 8; nz |= a >> 1; a = nz;",
        "RORA": "nz = (c >> 1) & 0x80; c = a << 8; nz |= a >> 1; a = nz;",
        "ASLA": "nz = a << 1; c = nz; a = (uint8_t) nz;",
        "ROLA": "nz = a << 1; { int temp = (c >> 8) & 1; c = nz; nz |= temp; } a = (uint8_t) nz;",
    }
    if name in simple:
        return simple[name], True
    if name in ("ASL", "ROL", "LSR", "ROR"):
        pre = "c = 0; " if name in ("ASL", "LSR") else ""
        left = name in ("ASL", "ROL")
        if mode in ("zp", "zpx"):
            ix = "data = uint8_t (data + x); " if mode == "zpx" else ""
            if left:
                body = "nz = (c >> 8) & 1; nz |= (c = READ_LOW( data ) << 1);"
            else:
                body = "{ int temp = READ_LOW( data ); nz = ((c >> 1) & 0x80) | (temp >> 1); c = temp << 8; }"
            return ix + pre + body + " WRITE_LOW( data, nz );", True
        ix = "data += x; " if mode == "abx" else ""
        dummy = "READ( data - ( temp & 0x100 ) ); " if mode == "abx" else ""
        if left:
            core = "nz = (c >> 8) & 1; nz |= (c = temp << 1);"
        else:
            core = "nz = ((c >> 1) & 0x80) | (temp >> 1); c = temp << 8;"
        rng = (a16 - 0x100, a16 + 0xFF) if mode == "abx" else (a16, a16)
        return (ix + pre + "{ int temp = data; data += 0x100 * %s; %sWRITE( data, temp = READ( data ) ); %s } "
                "WRITE( data, (uint8_t) nz );" % (H, dummy, core)), safe_write(*rng)
    if name in ("INC", "DEC"):
        n = "1" if name == "INC" else "-1"
        if mode in ("zp", "zpx"):
            ix = "data = uint8_t (data + x); " if mode == "zpx" else ""
            return ix + "nz = %s; nz += READ_LOW( data ); WRITE_LOW( data, nz );" % n, True
        if mode == "abs":
            pre = "data = %s; " % A16
            rng = (a16, a16)
        else:
            pre = "{ int temp = data + x; data = x + %s; READ( data - ( temp & 0x100 ) ); } " % A16
            rng = (a16 - 0x100, a16 + 0xFF)
        return (pre + "nz = %s; { int temp; WRITE( data, temp = READ( data ) ); nz += temp; WRITE( data, (uint8_t) nz ); }" % n,
                safe_write(*rng))
    raise KeyError(name)


def fnv(b):
    h = 0xcbf29ce484222325
    for x in b:
        h = ((h ^ x) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def prg_of(rom):
    d = open(rom, "rb").read()
    assert d[:4] == b"NES\x1a", rom
    off = 16 + (512 if d[6] & 4 else 0)
    return d[off: off + d[4] * 16384]


def read_profile(paths):
    c = collections.Counter()
    for p in paths:
        for line in open(p):
            f = line.split()
            if len(f) == 3:
                c[(int(f[0]), int(f[1]))] += int(f[2])
    return c


def translatable(prg, pc, off):
    if off >= len(prg) or pc >= 0x10000 or pc < 0x8000 or prg[off] not in OPS:
        return False
    ln = length(prg[off])
    return (pc & (PAGE - 1)) + ln <= PAGE and off + ln <= len(prg)


def gen_set(k, prg, prof, top):
    chosen = {}
    for (pc, off), n in sorted(prof.items(), key=lambda item: (-item[1], item[0])):
        if len(chosen) >= top:
            break
        if translatable(prg, pc, off):
            chosen[(pc, off)] = n
    variants = collections.defaultdict(dict)
    for (pc, off) in chosen:
        code = prg[off]
        lo = prg[off + 1] if length(code) > 1 else 0
        hi = prg[off + 2] if length(code) > 2 else 0
        variants[(pc >> 11, off - (pc & (PAGE - 1)))][pc] = (code, lo, hi)
    targets = set()
    for ins in variants.values():
        for pc, (code, lo, hi) in ins.items():
            name = OPS[code][0]
            if name == "BRANCH":
                off8 = lo - 256 if lo & 0x80 else lo
                targets.add((pc + 2 + off8) & 0xFFFF)
                targets.add(pc + 2)
            elif name in ("JSR", "JMP"):
                targets.add(lo | hi << 8)
                if name == "JSR":
                    targets.add(pc + 3)
    starts = collections.defaultdict(set)
    for key, ins in variants.items():
        ends = {}
        for p, (c2, _, _) in ins.items():
            if OPS[c2][0] not in CONTROL:
                ends[p + length(c2)] = p
        for pc in ins:
            if pc not in ends or pc in targets:
                starts[key].add(pc)
    out = []
    dispatch = collections.defaultdict(list)
    for key in sorted(variants):
        pgi, pgoff = key
        ins = variants[key]
        exp = "XL_EXP( %d )" % (pgoff - pgi * PAGE)

        def label(t):
            return "xb%d_%04X_%X" % (k, t, pgoff)

        def go(t):
            if t in ins and t in starts[key]:
                return "goto %s;" % label(t)
            return "{ pc = 0x%X; goto xl_dyn; }" % t

        for s in sorted(starts[key]):
            block = []
            pc = s
            while True:
                code = ins[pc][0]
                block.append(pc)
                nxt = pc + length(code)
                if OPS[code][0] in CONTROL or nxt not in ins or nxt in starts[key]:
                    break
                pc = nxt
            cyc = [CLOCK[ins[p][0]] + (1 if OPS[ins[p][0]] in PAGE_CROSS else 0) for p in block]
            dispatch[s].append((pgi, exp, label(s)))
            out.append("%s:" % label(s))
            out.append("\tif ( code_map [%d] != %s || clock_count + %d >= clock_limit ) { pc = 0x%X; goto loop; }"
                       % (pgi, exp, sum(cyc[:-1]), s))
            for i, p in enumerate(block):
                code, lo, hi = ins[p]
                name = OPS[code][0]
                nxt = p + length(code)
                last = i == len(block) - 1
                out.append("\tclock_count += %d;" % CLOCK[code])
                if name == "BRANCH":
                    off8 = lo - 256 if lo & 0x80 else lo
                    t = (p + 2 + off8) & 0xFFFF
                    extra = ((((p + 2) & 0xFF) + off8) >> 8) & 1
                    idle = "pc = 0x%X; IDLE_CHECK() " % t if off8 < 0 else ""
                    out.append("\tif ( %s ) { clock_count += %d; %s%s }" % (BRANCHES[code], extra, idle, go(t)))
                    out.append("\tclock_count--; %s" % go(nxt))
                    continue
                if name == "JSR":
                    t = lo | hi << 8
                    out.append("\tWRITE_LOW( 0x100 | (sp - 1), 0x%X ); sp = (sp - 2) | 0x100; WRITE_LOW( sp, 0x%X ); %s"
                               % ((p + 2) >> 8, (p + 2) & 0xFF, go(t)))
                    continue
                if name == "JMP":
                    t = lo | hi << 8
                    if t < p + 1:
                        out.append("\tpc = 0x%X; IDLE_CHECK() %s" % (t, go(t)))
                    else:
                        out.append("\t%s" % go(t))
                    continue
                if name == "RTS":
                    out.append("\tpc = 1 + READ_LOW( sp ); pc += READ_LOW( 0x100 | (sp - 0xFF) ) * 0x100; "
                               "sp = (sp - 0xFE) | 0x100; goto xl_dyn;")
                    continue
                body, safe = instr_body(code, lo, hi)
                if "data" in body:
                    out.append("\tdata = 0x%X;" % lo)
                out.append("\t" + body)
                if last:
                    if nxt in ins and nxt in starts[key]:
                        out.append("\tgoto %s;" % label(nxt))
                    else:
                        out.append("\tpc = 0x%X; goto loop;" % nxt)
                elif not safe:
                    out.append("\tif ( code_map [%d] != %s || clock_count + %d >= clock_limit ) { pc = 0x%X; goto loop; }"
                               % (pgi, exp, sum(cyc[i + 1:-1]), nxt))
    labels = []
    for pc in sorted(dispatch):
        lab = "xd%d_%04X" % (k, pc)
        labels.append((pc, lab))
        out.append("%s:" % lab)
        for pgi, exp, blab in dispatch[pc]:
            out.append("\tif ( code_map [%d] == %s ) goto %s;" % (pgi, exp, blab))
        out.append("\tgoto loop;")
    return out, labels, len(chosen), sum(chosen.values()), sum(prof.values())


def write_generated(src, sets):
    code = []
    all_labels = []
    tables = []
    for k, (prg, prof, top) in enumerate(sets):
        out, labels, n, hit, tot = gen_set(k, prg, prof, top)
        print("set %d: %d instructions, %.1f%% of profiled instructions, %d entry points"
              % (k, n, 100.0 * hit / max(tot, 1), len(labels)), file=sys.stderr)
        base = len(all_labels)
        all_labels += [lab for _, lab in labels]
        size = 1
        while size < 4 * max(len(labels), 1):
            size *= 2
        tab = [0] * size
        for i, (pc, _) in enumerate(labels):
            h = ((pc * 0x9E3779B1) & 0xFFFFFFFF) >> 16 & (size - 1)
            while tab[h]:
                h = (h + 1) & (size - 1)
            tab[h] = pc << 16 | (base + i + 1)
        tables.append((len(prg), fnv(prg), size - 1, tab))
        code += out
    assert len(all_labels) < 0xFFFF
    h = ["enum { xl_set_count = %d };" % len(sets)]
    for k, (_, _, _, tab) in enumerate(tables):
        h.append("static uint32_t const xl_tab_%d [%d] = { %s };" % (k, len(tab), ",".join(str(v) for v in tab)))
    h.append("static long const xl_set_size [%d] = { %s };" % (len(sets) + 1, ",".join([str(t[0]) for t in tables] + ["0"])))
    h.append("static uint64_t const xl_set_hash [%d] = { %s };" % (len(sets) + 1, ",".join(["%dull" % t[1] for t in tables] + ["0"])))
    h.append("static unsigned const xl_set_mask [%d] = { %s };" % (len(sets) + 1, ",".join([str(t[2]) for t in tables] + ["0"])))
    h.append("static uint32_t const* const xl_set_table [%d] = { %s };"
             % (len(sets) + 1, ",".join(["xl_tab_%d" % k for k in range(len(sets))] + ["0"])))
    (Path(src) / "nes_emu/Nes_Cpu_xlat.h").write_text("\n".join(h) + "\n")
    (Path(src) / "nes_emu/Nes_Cpu_xlat.inc").write_text(
        "xl_dyn:\n\tXL_ENTER\n\tgoto loop;\nxl_switch:\n\tswitch ( xl_i ) {\n"
        + "".join("\tcase %d: goto %s;\n" % (i, label) for i, label in enumerate(all_labels))
        + "\tdefault: goto loop;\n\t}\n" + "\n".join(code) + "\n")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def profiles(directory):
    found = {}
    for path in sorted(Path(directory).glob("*.txt")):
        first = path.read_text().split("\n", 1)[0].split()
        if len(first) == 2 and first[0] == "rom_sha256":
            found[first[1]] = path
    return found


def generate(args):
    known = profiles(args.profiles)
    sets = []
    for rom in args.roms:
        path = known.get(sha256(rom))
        if path is None:
            print("no translation profile for %s" % rom, file=sys.stderr)
            continue
        prof = read_profile([path])
        sets.append((prg_of(rom), prof, len(prof)))
    write_generated(args.core, sets)


def profile(args):
    prg = prg_of(args.rom)
    counts = read_profile(args.dumps)
    lines = ["rom_sha256 %s" % sha256(args.rom)]
    ranked = sorted(((key, n) for key, n in counts.items() if translatable(prg, *key)),
                    key=lambda item: (-item[1], item[0]))[: args.top]
    lines += ["%d %d %d" % (pc, offset, count) for (pc, offset), count in ranked]
    Path(args.out).write_text("\n".join(lines) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    gen = commands.add_parser("generate", help="write the translation sources into a patched core tree")
    gen.add_argument("core")
    gen.add_argument("profiles")
    gen.add_argument("roms", nargs="*")
    gen.set_defaults(run=generate)
    prof = commands.add_parser("profile", help="merge profile dumps into a committed profile")
    prof.add_argument("rom")
    prof.add_argument("out")
    prof.add_argument("dumps", nargs="+")
    prof.add_argument("--top", type=int, default=6000)
    prof.set_defaults(run=profile)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
