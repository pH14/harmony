// SPDX-License-Identifier: AGPL-3.0-or-later
//! Window verification: does the built payload contain the branches the oracle
//! says it contains?
//!
//! This is the gate that makes "known by construction" a *checked* property. The
//! oracle model declares, per payload, the exact sequence of branch instructions
//! its counting window emits ([`oracle_model::Expectation::inline_branch_seq`]).
//! Here we decode the window out of the linked ELF and compare. If the assembler,
//! a compiler upgrade, or a hand edit ever put a branch in the window that nobody
//! modelled — or took one away — the predicted count would be silently wrong, and
//! every count-exactness claim built on it would be worthless. This catches it
//! before a single measurement is spent.
//!
//! It also checks the exception handlers. Three payloads place their handler
//! *inline in a vector slot* precisely so the exception path contributes zero
//! branch instructions beyond the `ERET`; that claim is load-bearing for the
//! identifiability argument, so it is verified rather than asserted.

use crate::elf::{Elf, ElfError};
use crate::scan::{branch_sequence, window_branches};
use oracle_model::{BranchKind, Payload, Scale, expected};
use serde::Serialize;

/// The verdict for one payload.
#[derive(Clone, Debug, Serialize)]
pub struct Verdict {
    /// The payload.
    pub payload: Payload,
    /// What the model says the window contains.
    pub expected_branches: Vec<BranchKind>,
    /// What the linked ELF actually contains.
    pub found_branches: Vec<BranchKind>,
    /// The handler's branches, when the payload has one.
    pub handler_branches: Option<Vec<BranchKind>>,
    /// Everything agreed.
    pub ok: bool,
    /// Why not, when `ok` is false.
    pub failures: Vec<String>,
}

/// The vector-handler symbol stem for payloads that install their own table.
fn handler_stem(payload: Payload) -> Option<&'static str> {
    match payload {
        Payload::Svc => Some("svc"),
        Payload::ExceptionAbort => Some("abort"),
        Payload::WfiIdle => Some("wfi"),
        _ => None,
    }
}

/// Verify one payload's linked ELF against the oracle model.
///
/// # Errors
/// Propagates [`ElfError`] when the window brackets are missing or malformed — a
/// payload whose window symbols are absent cannot be verified, and that is a
/// failure, never a skip.
pub fn verify(elf: &Elf, payload: Payload) -> Result<Verdict, ElfError> {
    // The seed and scale do not affect which *instructions* are in the window,
    // only how many times they run, so any scale serves for the sequence check.
    let model = expected(payload, Scale::Smoke, oracle_model::DEFAULT_SEED);
    let expected_branches: Vec<BranchKind> = model.inline_branch_seq.to_vec();

    let mut failures = Vec::new();

    let (base, code) = elf.window(payload.name())?;
    let found_branches = branch_sequence(base, code);

    if found_branches != expected_branches {
        failures.push(format!(
            "window branch sequence differs: model says {expected_branches:?}, ELF has {found_branches:?}"
        ));
    } else {
        // The class sequence matches — now verify the PREDICATE and TARGET of each
        // branch, not just its class. A `b.ne` → `b.eq` flip keeps the class
        // (`BCond`) but reverses the loop, changing the taken-branch count while the
        // console output and exit status can stay identical (the svc loop, e.g., does
        // not count its own iterations). And a redirected target changes the control
        // flow within the same class. Class-only verification passes both; this does
        // not.
        let end = base.saturating_add(code.len() as u64);
        for (i, b) in window_branches(base, code).iter().enumerate() {
            // Predicate: a `B.cond`'s condition must match the model's.
            if let Some(expected_cond) = model.inline_branch_conds.get(i).copied().flatten() {
                match b.cond {
                    Some(c) if c == expected_cond => {}
                    Some(c) => failures.push(format!(
                        "branch #{i} at {:#x}: condition {c:#x} != model's {expected_cond:#x} \
                         (a flipped predicate keeps the class but changes the taken-branch count)",
                        b.addr
                    )),
                    None => failures.push(format!(
                        "branch #{i} at {:#x}: model expects a conditional ({expected_cond:#x}) \
                         but this is not a B.cond",
                        b.addr
                    )),
                }
            }
            // Predicate operand: a CBZ/CBNZ/TBZ/TBNZ tests a specific register (and, for
            // a bit test, a specific bit). Changing which register or bit is tested keeps
            // the class and the target but changes the taken-branch count — a regression
            // the class+condition+target checks all pass. This does not.
            if let Some(expected_op) = model.inline_branch_operands.get(i).copied().flatten() {
                match b.operand {
                    Some(op) if op == expected_op => {}
                    Some(op) => failures.push(format!(
                        "branch #{i} at {:#x}: predicate operand {op:#x} != model's {expected_op:#x} \
                         (a changed CBZ/TBZ register or bit changes the taken-branch count)",
                        b.addr
                    )),
                    None => failures.push(format!(
                        "branch #{i} at {:#x}: model expects a register/bit predicate ({expected_op:#x}) \
                         but this branch class carries none",
                        b.addr
                    )),
                }
            }
            // Target: an immediate branch must land at the EXACT address the model
            // declares (`window_base + offset`), not merely somewhere in the window.
            // In-window-only verification accepts a backedge or CBZ/TBZ retargeted to a
            // different in-window label — same class, same condition, different control
            // flow and taken-branch count. The exact check catches that.
            match (
                b.target,
                model.inline_branch_targets.get(i).copied().flatten(),
            ) {
                (Some(target), Some(off)) => {
                    let want = base.wrapping_add_signed(i64::from(off));
                    if target != want {
                        failures.push(format!(
                            "branch #{i} at {:#x}: target {target:#x} != model's {want:#x} \
                             (window_base {base:#x} + offset {off}) — a retargeted branch \
                             changes the counted control flow",
                            b.addr
                        ));
                    }
                }
                // The model declares no exact target for this immediate branch (a
                // modelling gap): fall back to the weaker in-window requirement rather
                // than accepting anything.
                (Some(target), None) => {
                    if !(base..end).contains(&target) {
                        failures.push(format!(
                            "branch #{i} at {:#x}: target {target:#x} is outside the window \
                             [{base:#x}, {end:#x})",
                            b.addr
                        ));
                    }
                }
                // A register/indirect branch has no static target; the model agrees
                // (None). Its destination is a runtime register value, out of scope here.
                (None, _) => {}
            }
        }
    }

    let handler_branches = match handler_stem(payload) {
        Some(stem) => {
            let (hbase, hcode) = elf.handler(stem)?;
            let hb = branch_sequence(hbase, hcode);
            // The handler must be exactly one ERET and nothing else. Any other
            // branch would be a taken branch on the exception path that the model
            // does not account for — the identifiability argument assumes the
            // exception path contributes no branch *instructions*, and this is
            // where that assumption is checked.
            if hb != vec![BranchKind::Eret] {
                failures.push(format!(
                    "handler `{stem}` must be exactly one ERET and no other branch, found {hb:?}"
                ));
            }
            Some(hb)
        }
        None => None,
    };

    Ok(Verdict {
        payload,
        expected_branches,
        found_branches,
        handler_branches,
        ok: failures.is_empty(),
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_exception_payloads_own_a_handler() {
        // The oracle's identifiability argument turns on exactly these three
        // carrying an exception path. If a fourth ever grows one, the model owes an
        // ambiguity term for it, and this test is what asks the question.
        assert_eq!(handler_stem(Payload::Svc), Some("svc"));
        assert_eq!(handler_stem(Payload::ExceptionAbort), Some("abort"));
        assert_eq!(handler_stem(Payload::WfiIdle), Some("wfi"));
        for p in [
            Payload::Ident,
            Payload::StraightLine,
            Payload::BranchDense,
            Payload::LlscAtomics,
            Payload::LseAtomics,
            Payload::ClockPage,
        ] {
            assert_eq!(handler_stem(p), None, "{}", p.name());
        }
    }
}
