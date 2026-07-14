//! Pins of values the *executed assembly* produced under emulation.
//!
//! These are not self-consistency checks — they are the model measured against
//! reality, as far as reality is available before silicon. The numbers below were
//! captured from `qemu-system-aarch64` (TCG, `-cpu neoverse-n1`) running the built
//! payloads at the smoke scale with the default seed. They pin the model against
//! silent drift: change the xorshift, the predicate order, or a branch's sense in
//! `oracles/src/asm/*.s` without changing the model here, and the payload's
//! *live* accumulator will disagree with the model — which is what
//! `payloads/smoke.sh` checks on every run — while a change to the *model* alone
//! is caught here.
//!
//! What they establish: the seven `branch-dense` predicates and the xorshift64*
//! stream that drives them agree, bit for bit, between the hand-written asm and
//! the Rust oracle, across a thousand trips. Since each branch adds a distinct
//! weight on its not-taken path, a matching accumulator means every predicate
//! evaluated identically — so the *taken count* the model derives from those same
//! predicates is derived from a validated function.
//!
//! What they do NOT establish, and no emulator can: that the hardware's
//! `BR_RETIRED` counter counts those branches. That is stage AA-1's question, and
//! only Neoverse N1 silicon answers it.

use oracle_model::{
    DEFAULT_SEED, Payload, Scale, branch_dense_accumulator, straight_line_accumulator, trips,
};

/// Observed: `branch-dense` printed `ACC value=0x4433` under TCG.
const TCG_BRANCH_DENSE_ACC: u64 = 0x4433;

/// Observed: `straight-line` printed `ACC value=0xbed0dc627afccfa5` under TCG.
const TCG_STRAIGHT_LINE_ACC: u64 = 0xbed0_dc62_7afc_cfa5;

#[test]
fn branch_dense_accumulator_matches_the_executed_asm() {
    let n = trips(Payload::BranchDense, Scale::Smoke);
    assert_eq!(
        branch_dense_accumulator(DEFAULT_SEED, n),
        TCG_BRANCH_DENSE_ACC
    );
}

#[test]
fn straight_line_accumulator_matches_the_executed_asm() {
    let n = trips(Payload::StraightLine, Scale::Smoke);
    assert_eq!(straight_line_accumulator(n), TCG_STRAIGHT_LINE_ACC);
}
