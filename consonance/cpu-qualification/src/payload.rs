// SPDX-License-Identifier: AGPL-3.0-or-later
//! The analytical-oracle payloads: loops whose work-clock count is known by
//! construction.
//!
//! Count exactness is judged only against payloads whose event count is an
//! arithmetic function of the iteration count, never against a second counter,
//! which would be circular. Every body is written entirely in assembly, so the
//! count does not depend on what a compiler chose to emit.
//!
//! Each class states its events per iteration. The measurement uses the
//! differential method — run at scales `n1` and `n2` and require
//! `count(n2) - count(n1) == events_per_iteration * (n2 - n1)` exactly — so the
//! fixed prologue contribution cancels and only the per-iteration exactness is
//! under test. A differential that varies is a mismatch, not a calibration.
//!
//! The bodies are written for x86-64 and aarch64. The class table is portable, so
//! the analytical numbers and the class set are checked everywhere.

/// One payload class and its analytical oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PayloadSpec {
    /// The class name, used in records.
    pub name: &'static str,
    /// Work-clock events one iteration of the body contributes.
    pub events_per_iteration: u64,
    /// Why the number is what it is.
    pub derivation: &'static str,
    /// Whether the class exists only on x86.
    pub x86_only: bool,
}

/// A single conditional backedge and nothing else.
pub const LOOP_BACKEDGE: PayloadSpec = PayloadSpec {
    name: "loop_backedge",
    events_per_iteration: 1,
    derivation: "the conditional backedge is the body's only branch",
    x86_only: false,
};

/// Eight unconditional jumps to the next instruction, then the backedge. The
/// jumps retire as branches but not as *conditional* branches, so what one
/// iteration contributes depends on which branches the architecture's work clock
/// counts.
pub const BRANCH_DENSE: PayloadSpec = PayloadSpec {
    name: "branch_dense",
    events_per_iteration: if cfg!(target_arch = "x86_64") { 1 } else { 9 },
    derivation: if cfg!(target_arch = "x86_64") {
        "the x86 work clock counts retired conditional branches — 0x1c4 on Intel, \
         0x5100d1 on AMD — and the eight jumps are unconditional, so only the \
         backedge counts"
    } else {
        "the aarch64 work clock BR_RETIRED counts every retired branch, so the eight \
         jumps and the backedge all count"
    },
    x86_only: false,
};

/// A call to a subroutine that returns immediately, then the backedge. Like
/// `branch_dense`, what it contributes depends on which branches the work clock
/// counts.
pub const CALL_RET: PayloadSpec = PayloadSpec {
    name: "call_ret",
    events_per_iteration: if cfg!(target_arch = "x86_64") { 1 } else { 3 },
    derivation: if cfg!(target_arch = "x86_64") {
        "the x86 work clock counts retired conditional branches, and neither the call \
         nor the return is conditional, so only the backedge counts"
    } else {
        "the aarch64 work clock BR_RETIRED counts every retired branch, so the call, \
         the return, and the backedge all count"
    },
    x86_only: false,
};

/// Sixteen non-branch arithmetic operations, then the backedge. Its count must
/// equal `loop_backedge`'s at the same scale: straight-line code contributes
/// nothing.
pub const STRAIGHT_LINE: PayloadSpec = PayloadSpec {
    name: "straight_line",
    events_per_iteration: 1,
    derivation: "sixteen non-branch operations contribute nothing; only the backedge counts",
    x86_only: false,
};

/// A lock-prefixed atomic add, then the backedge. A locked operation is not a
/// branch, so the oracle is the backedge alone; any excess is the speculative
/// lock-mapping erratum this class exists to expose.
pub const LOCKED: PayloadSpec = PayloadSpec {
    name: "locked",
    events_per_iteration: 1,
    derivation: "a locked memory operation is not a branch; only the backedge counts",
    x86_only: true,
};

/// Every payload class, in a stable order.
pub const PAYLOADS: &[PayloadSpec] =
    &[LOOP_BACKEDGE, BRANCH_DENSE, CALL_RET, STRAIGHT_LINE, LOCKED];

/// The class with this name.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static PayloadSpec> {
    PAYLOADS.iter().find(|p| p.name == name)
}

/// The classes this build can run.
#[must_use]
pub fn runnable() -> Vec<&'static PayloadSpec> {
    PAYLOADS
        .iter()
        .filter(|p| !p.x86_only || cfg!(target_arch = "x86_64"))
        .collect()
}

/// Run `n` iterations of `spec`'s body, returning its sink so nothing is elided.
///
/// Returns `None` for a class this build has no body for.
#[must_use]
pub fn run(spec: &PayloadSpec, n: u64) -> Option<u64> {
    if n == 0 {
        return Some(0);
    }
    match spec.name {
        "loop_backedge" => Some(loop_backedge(n)),
        "branch_dense" => Some(branch_dense(n)),
        "call_ret" => Some(call_ret(n)),
        "straight_line" => Some(straight_line(n)),
        #[cfg(target_arch = "x86_64")]
        "locked" => Some(locked(n)),
        _ => None,
    }
}

#[cfg(target_arch = "x86_64")]
mod bodies {
    use core::arch::asm;

    /// One add and the conditional backedge.
    pub(super) fn loop_backedge(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: pure register arithmetic on two operands the compiler
        // allocated; no memory is touched and no register outside the operands
        // is written.
        unsafe {
            asm!(
                "2:",
                "add {sink}, 1",
                "dec {left}",
                "jnz 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }

    /// Eight unconditional jumps, an add, and the conditional backedge.
    pub(super) fn branch_dense(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: as above; every jump targets a label inside this block.
        unsafe {
            asm!(
                "2:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "jmp 3f", "3:",
                "add {sink}, 1",
                "dec {left}",
                "jnz 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }

    /// A call to a subroutine that returns immediately, then the backedge. The
    /// call uses the stack, so this body does not claim `nostack`.
    pub(super) fn call_ret(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: the subroutine is inside this block and returns immediately,
        // so the stack is balanced at every point the block can be left.
        unsafe {
            asm!(
                "jmp 4f",
                "3:",
                "ret",
                "4:",
                "2:",
                "call 3b",
                "add {sink}, 1",
                "dec {left}",
                "jnz 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
            );
        }
        sink
    }

    /// Sixteen adds and the conditional backedge.
    pub(super) fn straight_line(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: as `loop_backedge`.
        unsafe {
            asm!(
                "2:",
                "add {sink}, 1", "add {sink}, 1", "add {sink}, 1", "add {sink}, 1",
                "add {sink}, 1", "add {sink}, 1", "add {sink}, 1", "add {sink}, 1",
                "add {sink}, 1", "add {sink}, 1", "add {sink}, 1", "add {sink}, 1",
                "add {sink}, 1", "add {sink}, 1", "add {sink}, 1", "add {sink}, 1",
                "dec {left}",
                "jnz 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }

    /// A lock-prefixed add to a local, then the backedge.
    pub(super) fn locked(n: u64) -> u64 {
        let mut cell: u64 = 0;
        let left = n;
        // SAFETY: the only memory the block touches is `cell`, a live local
        // whose address is passed in and which is not aliased for the duration.
        unsafe {
            asm!(
                "2:",
                "lock add qword ptr [{cell}], 1",
                "dec {left}",
                "jnz 2b",
                cell = in(reg) std::ptr::from_mut(&mut cell),
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        cell
    }
}

#[cfg(target_arch = "aarch64")]
mod bodies {
    use core::arch::asm;

    /// One add and the conditional backedge.
    pub(super) fn loop_backedge(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: pure register arithmetic on two operands the compiler
        // allocated; no memory is touched and no register outside the operands
        // is written.
        unsafe {
            asm!(
                "2:",
                "add {sink}, {sink}, #1",
                "subs {left}, {left}, #1",
                "b.ne 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }

    /// Eight unconditional branches, an add, and the conditional backedge.
    pub(super) fn branch_dense(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: as above; every branch targets a label inside this block.
        unsafe {
            asm!(
                "2:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "b 3f", "3:",
                "add {sink}, {sink}, #1",
                "subs {left}, {left}, #1",
                "b.ne 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }

    /// A branch-and-link to a subroutine that returns immediately, then the
    /// backedge. The link register is saved and restored inside the block, so
    /// the caller sees it unchanged.
    pub(super) fn call_ret(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: the subroutine is inside this block; the link register is
        // saved into an operand register on entry and restored on exit, so no
        // register outside the operands is left modified.
        unsafe {
            asm!(
                "mov {saved_lr}, x30",
                "b 4f",
                "3:",
                "ret",
                "4:",
                "2:",
                "bl 3b",
                "add {sink}, {sink}, #1",
                "subs {left}, {left}, #1",
                "b.ne 2b",
                "mov x30, {saved_lr}",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                saved_lr = out(reg) _,
                options(nostack),
            );
        }
        sink
    }

    /// Sixteen adds and the conditional backedge.
    pub(super) fn straight_line(n: u64) -> u64 {
        let mut sink: u64 = 0;
        let left = n;
        // SAFETY: as `loop_backedge`.
        unsafe {
            asm!(
                "2:",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "add {sink}, {sink}, #1", "add {sink}, {sink}, #1",
                "subs {left}, {left}, #1",
                "b.ne 2b",
                sink = inout(reg) sink,
                left = inout(reg) left => _,
                options(nostack),
            );
        }
        sink
    }
}

#[cfg(target_arch = "x86_64")]
use bodies::locked;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use bodies::{branch_dense, call_ret, loop_backedge, straight_line};

// A build for any other architecture has no bodies. The class table still
// compiles, so the analytical numbers stay under test; `run` reports that it
// cannot execute anything rather than returning a count of nothing.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod bodies {}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[allow(clippy::missing_const_for_fn)]
fn loop_backedge(_n: u64) -> u64 {
    0
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[allow(clippy::missing_const_for_fn)]
fn branch_dense(_n: u64) -> u64 {
    0
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[allow(clippy::missing_const_for_fn)]
fn call_ret(_n: u64) -> u64 {
    0
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[allow(clippy::missing_const_for_fn)]
fn straight_line(_n: u64) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_states_its_oracle_and_why() {
        assert_eq!(PAYLOADS.len(), 5);
        for spec in PAYLOADS {
            assert!(spec.events_per_iteration >= 1, "{}", spec.name);
            assert!(!spec.derivation.is_empty(), "{}", spec.name);
        }
        assert_eq!(LOOP_BACKEDGE.events_per_iteration, 1);
        assert_eq!(STRAIGHT_LINE.events_per_iteration, 1);
        assert_eq!(LOCKED.events_per_iteration, 1);
    }

    /// The two classes whose bodies contain branches the work clock may or may
    /// not count. Getting these wrong is not a small error: the same number sets
    /// the exactness oracle and the iteration count an overflow arm needs, so a
    /// wrong value both fails exactness and loses every overflow.
    #[test]
    #[cfg(target_arch = "x86_64")]
    fn the_x86_oracles_count_only_conditional_branches() {
        assert_eq!(
            BRANCH_DENSE.events_per_iteration, 1,
            "eight unconditional jumps contribute nothing to a conditional-branch clock"
        );
        assert_eq!(
            CALL_RET.events_per_iteration, 1,
            "neither a call nor a return is a conditional branch"
        );
        for spec in [BRANCH_DENSE, CALL_RET] {
            assert!(spec.derivation.contains("conditional"), "{}", spec.name);
        }
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn the_aarch64_oracles_count_every_retired_branch() {
        assert_eq!(BRANCH_DENSE.events_per_iteration, 9);
        assert_eq!(CALL_RET.events_per_iteration, 3);
        for spec in [BRANCH_DENSE, CALL_RET] {
            assert!(spec.derivation.contains("BR_RETIRED"), "{}", spec.name);
        }
    }

    #[test]
    fn class_names_are_distinct_and_findable() {
        let mut names: Vec<&str> = PAYLOADS.iter().map(|p| p.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
        for spec in PAYLOADS {
            assert_eq!(by_name(spec.name), Some(spec));
        }
        assert_eq!(by_name("no_such_class"), None);
    }

    #[test]
    fn the_locked_class_is_the_only_one_confined_to_x86() {
        const { assert!(LOCKED.x86_only) };
        for spec in PAYLOADS.iter().filter(|p| p.name != "locked") {
            assert!(!spec.x86_only, "{}", spec.name);
        }
        let runnable = runnable();
        assert_eq!(
            runnable.len(),
            if cfg!(target_arch = "x86_64") { 5 } else { 4 }
        );
    }

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    #[test]
    #[cfg_attr(
        miri,
        ignore = "the payload bodies are inline asm, which Miri cannot execute"
    )]
    fn every_runnable_body_executes_its_iterations() {
        // Each body's sink counts one increment per iteration, except the
        // straight-line class, whose sixteen adds make sixteen. A body that
        // failed to loop the requested number of times would show here.
        for (spec, per_iteration) in [
            (&LOOP_BACKEDGE, 1u64),
            (&BRANCH_DENSE, 1),
            (&CALL_RET, 1),
            (&STRAIGHT_LINE, 16),
        ] {
            for n in [1u64, 2, 7, 1000] {
                assert_eq!(
                    run(spec, n),
                    Some(per_iteration * n),
                    "{} at n={n}",
                    spec.name
                );
            }
        }
        #[cfg(target_arch = "x86_64")]
        for n in [1u64, 2, 7, 1000] {
            assert_eq!(run(&LOCKED, n), Some(n), "locked at n={n}");
        }
    }

    #[test]
    fn a_zero_iteration_run_does_nothing_rather_than_looping_forever() {
        for spec in runnable() {
            assert_eq!(run(spec, 0), Some(0), "{}", spec.name);
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    #[test]
    fn a_class_this_build_cannot_run_says_so_rather_than_returning_a_count() {
        assert_eq!(run(&LOCKED, 10), None);
    }
}
