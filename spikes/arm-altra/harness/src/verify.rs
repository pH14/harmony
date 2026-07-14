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
use crate::scan::branch_sequence;
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
