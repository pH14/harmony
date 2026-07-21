//! Fault-probing helpers for the instruction/MSR-sweep payloads.
//!
//! Several sweep payloads execute an instruction that *may* fault (RDPMC,
//! MONITOR/MWAIT, RDMSR/WRMSR of denied indices) and need to observe *whether*
//! it faulted without aborting. The [`crate::idt`] #UD/#GP stubs already count
//! faults and skip the faulting instruction (advancing the saved RIP by
//! `FAULT_SKIP`); this module wraps the "declare length, run, did it fault?"
//! dance so payloads don't re-implement it.
//!
//! The disposition under stock QEMU/TCG and on the deterministic box differ for
//! some probes (e.g. TCG raises #UD where the box raises #GP); callers assert
//! only environment-independent facts in the serial banner ("executed / faulted
//! and resumed") and [`report`](crate::report) the exact disposition for the
//! box oracle.

use crate::idt;
use core::sync::atomic::Ordering::SeqCst;

/// Install the #UD (vector 6) and #GP (vector 13) fault stubs and load the IDT.
/// Call once before any [`faulted`] / [`gp_faulted`] probe.
pub fn install_fault_handlers() {
    idt::set_gate(6, idt::ud_stub);
    idt::set_gate(13, idt::gp_stub);
    idt::load();
}

/// Run `f`, having declared the probed instruction's length so a fault resumes
/// just past it. Returns `true` if `f` raised **any** fault (#UD or #GP).
/// Requires [`install_fault_handlers`].
pub fn faulted(instr_len: u64, f: impl FnOnce()) -> bool {
    idt::FAULT_SKIP.store(instr_len, SeqCst);
    let before = idt::FAULT_COUNT.load(SeqCst);
    f();
    idt::FAULT_COUNT.load(SeqCst) != before
}

/// As [`faulted`], but `true` only when `f` raised **#GP** specifically (the
/// contract's default-deny disposition); a #UD alone returns `false`.
pub fn gp_faulted(instr_len: u64, f: impl FnOnce()) -> bool {
    idt::FAULT_SKIP.store(instr_len, SeqCst);
    let before = idt::GP_COUNT.load(SeqCst);
    f();
    idt::GP_COUNT.load(SeqCst) != before
}
