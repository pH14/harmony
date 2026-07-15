// SPDX-License-Identifier: AGPL-3.0-or-later
//! The aarch64 opcode scanner.
//!
//! One decoder, three jobs, all of them load-bearing:
//!
//! 1. **Branch scanning** — decode a payload's counting window and check that the
//!    branch instructions it actually contains are exactly the ones
//!    `oracle-model` says it contains. This is what makes "the taken-branch count
//!    is known by construction" a *machine-checked* claim rather than a comment:
//!    an assembler that emitted a branch nobody modelled, or a hand edit to the
//!    asm that forgot the model, fails the gate.
//! 2. **Exclusives scanning** (`LDXR`/`STXR` family) — stage AA-4 level 2, the
//!    opcode scan of every executable guest page that makes the LSE-only contract
//!    *enforceable* rather than advisory (`docs/ARM-ALTRA.md` §4).
//! 3. **Counter-read scanning** (`MRS` of `CNTVCT_EL0` and friends) — stage AA-5's
//!    closure check. On silicon without FEAT_ECV an EL1 counter read cannot be
//!    trapped, so the shipped guest kernel is *opcode-scanned* for raw counter
//!    reads as a machine-checked acceptance criterion (`docs/ARM-ALTRA.md` §1).
//!    There is no trap to fall back on; the scan is the enforcement.
//!
//! Pure logic — no `unsafe`, no syscalls — and therefore fully testable on the
//! development Mac.

use oracle_model::BranchKind;
use serde::Serialize;

/// One decoded instruction of interest.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub struct Hit {
    /// Address of the instruction.
    pub addr: u64,
    /// The raw 32-bit encoding.
    pub word: u32,
    /// What it is.
    pub kind: HitKind,
}

/// The classes the scanner recognizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HitKind {
    /// A branch instruction.
    Branch(BranchKind),
    /// A load/store **exclusive** (`LDXR`/`LDAXR`/`STXR`/`STLXR`/`LDXP`/`STXP`).
    /// The AA-4 hazard. Deliberately excludes `LDAR`/`STLR` (ordered but not
    /// exclusive) and the LSE `CAS` family, which share the encoding class but are
    /// not hazards — a scanner that lumped them together would flag every
    /// acquire-load in the kernel and drown the signal.
    Exclusive,
    /// An `MRS` read of a counter register.
    CounterRead(CounterReg),
}

/// The counter registers whose guest reads AA-5 must close.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CounterReg {
    /// `CNTVCT_EL0` — the virtual counter. **The untrappable one** without
    /// FEAT_ECV, and the reason the paravirt clock design exists.
    Cntvct,
    /// `CNTVCTSS_EL0` — the self-synchronized virtual counter.
    Cntvctss,
    /// `CNTPCT_EL0` — the physical counter. Trappable via `CNTHCTL_EL2` even
    /// without ECV, and kept trapped as a backstop.
    Cntpct,
    /// `CNTPCTSS_EL0`.
    Cntpctss,
    /// `CNTFRQ_EL0` — the frequency. Not a time source by itself, but a guest that
    /// reads it is a guest that intends to interpolate, so it is worth surfacing.
    Cntfrq,
}

impl CounterReg {
    /// The register's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CounterReg::Cntvct => "CNTVCT_EL0",
            CounterReg::Cntvctss => "CNTVCTSS_EL0",
            CounterReg::Cntpct => "CNTPCT_EL0",
            CounterReg::Cntpctss => "CNTPCTSS_EL0",
            CounterReg::Cntfrq => "CNTFRQ_EL0",
        }
    }
}

/// Decode one 32-bit instruction word, if it is a branch.
///
/// Encodings are from the Armv8-A architecture reference; each is pinned by a unit
/// test below and, more usefully, by `arm-scan` reproducing the hand-written asm's
/// branch sequence exactly (which would not happen if a mask were wrong).
#[must_use]
pub fn decode_branch(word: u32) -> Option<BranchKind> {
    // Unconditional immediate: B / BL.
    if word & 0xFC00_0000 == 0x1400_0000 {
        return Some(BranchKind::B);
    }
    if word & 0xFC00_0000 == 0x9400_0000 {
        return Some(BranchKind::Bl);
    }
    // Conditional immediate: B.cond (bit 4 clear distinguishes it from BC.cond).
    if word & 0xFF00_0010 == 0x5400_0000 {
        return Some(BranchKind::BCond);
    }
    // Compare-and-branch: CBZ / CBNZ (bit 24 selects).
    if word & 0x7F00_0000 == 0x3400_0000 {
        return Some(BranchKind::Cbz);
    }
    if word & 0x7F00_0000 == 0x3500_0000 {
        return Some(BranchKind::Cbnz);
    }
    // Test-and-branch: TBZ / TBNZ.
    if word & 0x7F00_0000 == 0x3600_0000 {
        return Some(BranchKind::Tbz);
    }
    if word & 0x7F00_0000 == 0x3700_0000 {
        return Some(BranchKind::Tbnz);
    }
    // Unconditional register: BR / BLR / RET.
    if word & 0xFFFF_FC1F == 0xD61F_0000 {
        return Some(BranchKind::Br);
    }
    if word & 0xFFFF_FC1F == 0xD63F_0000 {
        return Some(BranchKind::Blr);
    }
    if word & 0xFFFF_FC1F == 0xD65F_0000 {
        return Some(BranchKind::Ret);
    }
    // ERET.
    if word == 0xD69F_03E0 {
        return Some(BranchKind::Eret);
    }
    None
}

/// Whether the word is a load/store **exclusive**.
///
/// The encoding class (`bits[29:24] == 0b001000`) also contains `LDAR`/`STLR` and
/// the LSE `CAS` family. Only `o2 == 0` (bit 23) is the exclusive family — the
/// monitor-based instructions that AA-4's hazard is about. `CAS` is an LSE atomic
/// and is the *answer*, not the hazard; flagging it would be a false positive that
/// makes the whole scan untrustworthy.
#[must_use]
pub fn is_exclusive(word: u32) -> bool {
    let class = (word >> 24) & 0x3F;
    let o2 = (word >> 23) & 1;
    class == 0b001000 && o2 == 0
}

/// Classify a word as one of the non-branch [`OracleOp`] classes a payload's count can
/// rest on, if it is one. Used by the window gate to verify the load-bearing non-branch
/// opcodes (an `SVC` removed, or a `subs …, #1` retuned to `#2`, leaves the branch sequence
/// and the smoke output unchanged while breaking the count).
#[must_use]
pub fn classify_oracle_op(word: u32) -> Option<oracle_model::OracleOp> {
    use oracle_model::OracleOp;
    // `SVC #imm` = 0xD400_0000 | (imm16 << 5) | 0b00001. The payloads issue `svc #0`.
    if word == 0xD400_0001 {
        return Some(OracleOp::Svc);
    }
    // `WFI` = 0xD503_207F (a fixed hint encoding).
    if word == 0xD503_207F {
        return Some(OracleOp::Wfi);
    }
    // LL/SC exclusive load/store — reuse the exclusive-family decoder.
    if is_exclusive(word) {
        return Some(OracleOp::LlscExclusive);
    }
    // `SUBS Xd, Xn, #imm` (add/subtract immediate, op=SUB, S=set-flags): bits[30:24] =
    // 0b1110001 (0x71), any `sf`. A loop-counter decrement has shift `sh == 0` and `imm12
    // == 1`; a change to `#2` (or a shifted immediate) no longer matches, which is exactly
    // the retune the count depends on. (`CMP`/`CMN` and non-flag `SUB` do not match.)
    if word & 0x7F00_0000 == 0x7100_0000 && (word >> 22) & 0x3 == 0 && (word >> 10) & 0xFFF == 1 {
        return Some(OracleOp::SubsDecrement);
    }
    None
}

/// The non-branch [`OracleOp`] classes present in a window, in program order.
#[must_use]
pub fn window_oracle_ops(code: &[u8]) -> Vec<oracle_model::OracleOp> {
    code.chunks_exact(4)
        .filter_map(|chunk| {
            let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            classify_oracle_op(word)
        })
        .collect()
}

/// Decode an `MRS` of a counter register, if that is what this is.
///
/// `MRS Xt, <sysreg>` is `0xD53_....`; the system register is
/// `(op0, op1, CRn, CRm, op2)`. The counter registers all live at
/// `op0=3, op1=3, CRn=14, CRm=0`, distinguished by `op2`.
#[must_use]
pub fn decode_counter_read(word: u32) -> Option<CounterReg> {
    if word & 0xFFF0_0000 != 0xD530_0000 {
        return None;
    }
    let o0 = (word >> 19) & 0x1; // op0 - 2
    let op1 = (word >> 16) & 0x7;
    let crn = (word >> 12) & 0xF;
    let crm = (word >> 8) & 0xF;
    let op2 = (word >> 5) & 0x7;

    if o0 != 1 || op1 != 3 || crn != 14 || crm != 0 {
        return None;
    }
    match op2 {
        0 => Some(CounterReg::Cntfrq),
        1 => Some(CounterReg::Cntpct),
        2 => Some(CounterReg::Cntvct),
        5 => Some(CounterReg::Cntpctss),
        6 => Some(CounterReg::Cntvctss),
        _ => None,
    }
}

/// Scan a byte range of instructions, starting at `base`.
///
/// Trailing bytes that do not form a whole instruction are ignored rather than
/// panicking: this runs over kernel images and guest pages, which are untrusted
/// input as far as this code is concerned.
#[must_use]
pub fn scan(base: u64, code: &[u8]) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, chunk) in code.chunks_exact(4).enumerate() {
        // chunks_exact(4) yields exactly 4 bytes; the conversion cannot fail.
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let addr = base.saturating_add((i as u64).saturating_mul(4));

        if let Some(kind) = decode_branch(word) {
            hits.push(Hit {
                addr,
                word,
                kind: HitKind::Branch(kind),
            });
        } else if is_exclusive(word) {
            hits.push(Hit {
                addr,
                word,
                kind: HitKind::Exclusive,
            });
        } else if let Some(reg) = decode_counter_read(word) {
            hits.push(Hit {
                addr,
                word,
                kind: HitKind::CounterRead(reg),
            });
        }
    }
    hits
}

/// The branch instructions in a range, in address order.
#[must_use]
pub fn branch_sequence(base: u64, code: &[u8]) -> Vec<BranchKind> {
    scan(base, code)
        .into_iter()
        .filter_map(|h| match h.kind {
            HitKind::Branch(k) => Some(k),
            _ => None,
        })
        .collect()
}

/// The 4-bit condition code of a `B.cond`, or `None` if the word is not a `B.cond`.
///
/// This is what distinguishes `b.ne` (`0x1`) from `b.eq` (`0x0`) — instructions that
/// share [`BranchKind::BCond`] but flip the control flow, and so the taken-branch
/// count. Class-only verification cannot tell them apart; this is the discriminator
/// the window verifier checks against the model.
#[must_use]
pub fn decode_cond(word: u32) -> Option<u8> {
    if word & 0xFF00_0010 == 0x5400_0000 {
        Some((word & 0xF) as u8)
    } else {
        None
    }
}

/// The resolved target address of an **immediate** branch (`B`/`BL`/`B.cond`/`CBZ`/
/// `CBNZ`/`TBZ`/`TBNZ`), or `None` for register/indirect branches whose target is a
/// runtime register value. Used to check that a window branch's target stays inside
/// the window — a redirected target that leaves the counted loop changes the control
/// flow while keeping the branch's class and condition.
#[must_use]
pub fn branch_target(word: u32, addr: u64) -> Option<u64> {
    // Sign-extend an `bits`-wide immediate.
    let sext = |imm: u64, bits: u32| -> i64 {
        let shift = 64 - bits;
        ((imm << shift) as i64) >> shift
    };
    let off = if word & 0xFC00_0000 == 0x1400_0000 || word & 0xFC00_0000 == 0x9400_0000 {
        // B / BL: imm26.
        sext((word & 0x03FF_FFFF) as u64, 26)
    } else if word & 0xFF00_0010 == 0x5400_0000 {
        // B.cond: imm19 at bits [23:5].
        sext(((word >> 5) & 0x7_FFFF) as u64, 19)
    } else if word & 0x7E00_0000 == 0x3400_0000 {
        // CBZ / CBNZ: imm19 at bits [23:5].
        sext(((word >> 5) & 0x7_FFFF) as u64, 19)
    } else if word & 0x7E00_0000 == 0x3600_0000 {
        // TBZ / TBNZ: imm14 at bits [18:5].
        sext(((word >> 5) & 0x3FFF) as u64, 14)
    } else {
        return None;
    };
    // The immediate is in units of 4-byte instructions, relative to the branch.
    Some(addr.wrapping_add((off << 2) as u64))
}

/// The **predicate operand** of a register-tested branch — the register (and, for a
/// bit test, the bit) whose value decides whether the branch is taken. `None` for a
/// `B.cond` (its predicate is the condition code, see [`decode_cond`]) and for the
/// unconditional/register-indirect classes.
///
/// - `CBZ`/`CBNZ`: `(sf << 5) | Rt` — the source register and its 32/64-bit size.
/// - `TBZ`/`TBNZ`: `(bit << 5) | Rt` — the tested bit position (`b5:b40`) and register.
///
/// Changing the tested register or bit while keeping the branch class and target is a
/// different predicate — a different taken-branch count — that a class+target check
/// would pass. This is the discriminator the verifier checks against the model.
#[must_use]
pub fn branch_test_operand(word: u32) -> Option<u32> {
    // CBZ / CBNZ: bits[30:24] == 0b0110100, then bit 24 selects Z/NZ.
    if word & 0x7E00_0000 == 0x3400_0000 {
        let rt = word & 0x1F;
        let sf = (word >> 31) & 1;
        return Some((sf << 5) | rt);
    }
    // TBZ / TBNZ: bits[30:25] == 0b011011.
    if word & 0x7E00_0000 == 0x3600_0000 {
        let rt = word & 0x1F;
        let b40 = (word >> 19) & 0x1F;
        let b5 = (word >> 31) & 1;
        let bit = (b5 << 5) | b40;
        return Some((bit << 5) | rt);
    }
    None
}

/// One window branch, decoded to the detail the "known by construction" gate needs:
/// its class, its condition (for `B.cond`), its resolved target, and its register/bit
/// predicate operand (for `CBZ`/`TBZ`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowBranch {
    /// Address of the branch.
    pub addr: u64,
    /// The branch class.
    pub kind: BranchKind,
    /// The 4-bit condition, for `B.cond` only.
    pub cond: Option<u8>,
    /// The resolved target, for immediate branches.
    pub target: Option<u64>,
    /// The register/bit predicate operand, for `CBZ`/`CBNZ`/`TBZ`/`TBNZ`.
    pub operand: Option<u32>,
}

/// Every branch in a range, decoded to [`WindowBranch`] detail, in address order.
#[must_use]
pub fn window_branches(base: u64, code: &[u8]) -> Vec<WindowBranch> {
    scan(base, code)
        .into_iter()
        .filter_map(|h| match h.kind {
            HitKind::Branch(kind) => Some(WindowBranch {
                addr: h.addr,
                kind,
                cond: decode_cond(h.word),
                target: branch_target(h.word, h.addr),
                operand: branch_test_operand(h.word),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Encodings assembled by the same toolchain that builds the payloads. They are
    // pinned here, but the load-bearing validation is `arm-scan` reproducing each
    // payload's hand-written branch sequence exactly — a wrong mask could not do
    // that.

    #[test]
    fn decodes_every_branch_class() {
        assert_eq!(decode_branch(0x1400_0001), Some(BranchKind::B)); // b .+4
        assert_eq!(decode_branch(0x9400_0001), Some(BranchKind::Bl)); // bl .+4
        assert_eq!(decode_branch(0x5400_0021), Some(BranchKind::BCond)); // b.ne .+4
        assert_eq!(decode_branch(0x3400_0020), Some(BranchKind::Cbz)); // cbz w0
        assert_eq!(decode_branch(0x3500_0020), Some(BranchKind::Cbnz)); // cbnz w0
        assert_eq!(decode_branch(0x3600_0020), Some(BranchKind::Tbz)); // tbz w0,#0
        assert_eq!(decode_branch(0x3700_0020), Some(BranchKind::Tbnz)); // tbnz w0,#0
        assert_eq!(decode_branch(0xD61F_0000), Some(BranchKind::Br)); // br x0
        assert_eq!(decode_branch(0xD63F_0000), Some(BranchKind::Blr)); // blr x0
        assert_eq!(decode_branch(0xD65F_03C0), Some(BranchKind::Ret)); // ret
        assert_eq!(decode_branch(0xD69F_03E0), Some(BranchKind::Eret)); // eret
    }

    #[test]
    fn does_not_mistake_ordinary_instructions_for_branches() {
        assert_eq!(decode_branch(0xD503_201F), None); // nop
        assert_eq!(decode_branch(0x8B01_0000), None); // add x0, x0, x1
        assert_eq!(decode_branch(0xD400_0001), None); // svc #0
        assert_eq!(decode_branch(0xD503_207F), None); // wfi
    }

    #[test]
    fn svc_is_not_a_branch_and_wfi_is_not_a_branch() {
        // Both are *ambiguity classes* in the oracle — changes of flow or of state
        // that are not branch instructions — and the whole identifiability argument
        // depends on them not being counted as inline branches by the scanner.
        assert_eq!(decode_branch(0xD400_0001), None); // svc #0
        assert_eq!(decode_branch(0xD503_207F), None); // wfi
        // ERET, by contrast, *is* decoded — it is a branch instruction whose
        // BR_RETIRED weight is nonetheless unknown. The distinction matters.
        assert_eq!(decode_branch(0xD69F_03E0), Some(BranchKind::Eret));
    }

    #[test]
    fn identifies_the_exclusive_family() {
        assert!(is_exclusive(0xC85F_7C41)); // ldxr x1, [x2]
        assert!(is_exclusive(0xC800_7C41)); // stxr w0, x1, [x2]
        assert!(is_exclusive(0xC85F_FC41)); // ldaxr x1, [x2]
        assert!(is_exclusive(0xC800_FC41)); // stlxr w0, x1, [x2]
        assert!(is_exclusive(0x885F_7C41)); // ldxr w1, [x2]
    }

    #[test]
    fn does_not_flag_ldar_stlr_or_the_lse_atomics() {
        // The false-positive class that would make the AA-4 scan useless: these
        // share the encoding class with the exclusives but carry no monitor, so
        // they are not the hazard. LDAR in particular appears all over a kernel.
        assert!(!is_exclusive(0xC8DF_FC41)); // ldar x1, [x2]
        assert!(!is_exclusive(0xC89F_FC41)); // stlr x1, [x2]
        assert!(!is_exclusive(0xC8A0_7C41)); // cas x0, x1, [x2]
        assert!(!is_exclusive(0xB820_0041)); // ldadd w0, w1, [x2]  (LSE)
        assert!(!is_exclusive(0xF820_0041)); // ldadd x0, x1, [x2]  (LSE)
    }

    #[test]
    fn classifies_the_oracle_load_bearing_opcodes() {
        use oracle_model::OracleOp;
        // The defining ops.
        assert_eq!(classify_oracle_op(0xD400_0001), Some(OracleOp::Svc)); // svc #0
        assert_eq!(classify_oracle_op(0xD503_207F), Some(OracleOp::Wfi)); // wfi
        assert_eq!(
            classify_oracle_op(0xC85F_7C41),
            Some(OracleOp::LlscExclusive)
        ); // ldxr x1,[x2]
        // The loop-counter decrement, and its exact-immediate sensitivity.
        assert_eq!(
            classify_oracle_op(0xF100_0421),
            Some(OracleOp::SubsDecrement)
        ); // subs x1,x1,#1
        assert_eq!(
            classify_oracle_op(0x7100_0421),
            Some(OracleOp::SubsDecrement)
        ); // subs w1,w1,#1
        assert_eq!(classify_oracle_op(0xF100_0821), None); // subs x1,x1,#2 — not a -1 decrement
        assert_eq!(classify_oracle_op(0xF140_0421), None); // subs …,#1,lsl#12 — a shifted imm
        // A plain SUB (no flags) and a CMP are not the flag-setting decrement the loop needs.
        assert_eq!(classify_oracle_op(0xD100_0421), None); // sub x1,x1,#1 (S=0)
        // svc #1 is not the counted svc #0.
        assert_eq!(classify_oracle_op(0xD400_0021), None); // svc #1
        // Unrelated instructions.
        assert_eq!(classify_oracle_op(0xD503_201F), None); // nop
        assert_eq!(classify_oracle_op(0x1400_0001), None); // b .+4
    }

    #[test]
    fn window_oracle_ops_preserves_order_and_multiplicity() {
        use oracle_model::OracleOp;
        // A window with SVC, then a decrement, then a SECOND SVC (the doubling the exact-
        // sequence gate must catch — `contains` would not). Interleaved with a NOP that is
        // not classified.
        let mut code = Vec::new();
        for w in [
            0xD400_0001u32, // svc #0
            0xD503_201F,    // nop (ignored)
            0xF100_0421,    // subs x1,x1,#1
            0xD400_0001,    // svc #0 (second)
        ] {
            code.extend_from_slice(&w.to_le_bytes());
        }
        assert_eq!(
            window_oracle_ops(&code),
            vec![OracleOp::Svc, OracleOp::SubsDecrement, OracleOp::Svc],
            "the classified sequence keeps order and multiplicity, dropping only unclassified \
             instructions"
        );
    }

    #[test]
    fn decodes_the_counter_reads() {
        assert_eq!(decode_counter_read(0xD53B_E040), Some(CounterReg::Cntvct)); // mrs x0, cntvct_el0
        assert_eq!(decode_counter_read(0xD53B_E020), Some(CounterReg::Cntpct)); // mrs x0, cntpct_el0
        assert_eq!(decode_counter_read(0xD53B_E000), Some(CounterReg::Cntfrq)); // mrs x0, cntfrq_el0
        assert_eq!(decode_counter_read(0xD53B_E0C0), Some(CounterReg::Cntvctss));
        assert_eq!(decode_counter_read(0xD53B_E0A0), Some(CounterReg::Cntpctss));
        // The register field is the low 5 bits: any destination register must
        // decode, or a scan would miss `mrs x7, cntvct_el0` and call the image clean.
        assert_eq!(decode_counter_read(0xD53B_E047), Some(CounterReg::Cntvct)); // mrs x7
        assert_eq!(decode_counter_read(0xD53B_E05F), Some(CounterReg::Cntvct)); // mrs xzr
    }

    #[test]
    fn does_not_flag_other_system_register_reads() {
        assert_eq!(decode_counter_read(0xD53B_E060), None); // cntv_tval_el0 (op2=3)
        assert_eq!(decode_counter_read(0xD538_0000), None); // midr_el1
        assert_eq!(decode_counter_read(0xD51B_E040), None); // MSR (a write), not MRS
    }

    #[test]
    fn scans_a_range_in_address_order() {
        // ret; b .+4; nop; eret
        let code = [
            0xC0, 0x03, 0x5F, 0xD6, //
            0x01, 0x00, 0x00, 0x14, //
            0x1F, 0x20, 0x03, 0xD5, //
            0xE0, 0x03, 0x9F, 0xD6, //
        ];
        let seq = branch_sequence(0x4008_0000, &code);
        assert_eq!(seq, vec![BranchKind::Ret, BranchKind::B, BranchKind::Eret]);

        let hits = scan(0x4008_0000, &code);
        assert_eq!(hits[0].addr, 0x4008_0000);
        assert_eq!(hits[1].addr, 0x4008_0004);
        assert_eq!(hits[2].addr, 0x4008_000C);
    }

    #[test]
    fn a_trailing_partial_instruction_is_ignored_not_panicked_on() {
        // Kernel images and guest pages are untrusted input here.
        let code = [0xC0, 0x03, 0x5F, 0xD6, 0xAA, 0xBB];
        assert_eq!(branch_sequence(0, &code), vec![BranchKind::Ret]);
        assert!(branch_sequence(0, &[]).is_empty());
        assert!(branch_sequence(0, &[0x00]).is_empty());
    }
}
