//! Minimal 64-bit IDT plus the interrupt/fault stubs the payloads need.
//! `extern "x86-interrupt"` is unstable, so the entry stubs are naked
//! functions that save the caller-saved registers, call a Rust handler, and
//! `iretq`. Counters are plain atomics — there is exactly one vCPU.

use core::arch::naked_asm;
use core::sync::atomic::{AtomicU64, Ordering::SeqCst};

/// Software interrupts observed by [`swint_stub`] (vector 0x40 self-test).
pub static SWINT_COUNT: AtomicU64 = AtomicU64::new(0);
/// PIT ticks observed by [`timer_stub`].
pub static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
/// LAPIC timer interrupts observed by [`apic_timer_stub`].
pub static APIC_TICKS: AtomicU64 = AtomicU64::new(0);
/// Faults caught by [`ud_stub`] and [`gp_stub`] together.
pub static FAULT_COUNT: AtomicU64 = AtomicU64::new(0);
/// Faults caught by [`gp_stub`] alone.
pub static GP_COUNT: AtomicU64 = AtomicU64::new(0);
/// Bytes the fault stubs add to the saved RIP — the length of the
/// instruction under test. Set it before probing a maybe-faulting
/// instruction so execution resumes right after it.
pub static FAULT_SKIP: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    offset_lo: u16,
    selector: u16,
    options: u16,
    offset_mid: u16,
    offset_hi: u32,
    reserved: u32,
}

const VACANT: Entry = Entry {
    offset_lo: 0,
    selector: 0,
    options: 0,
    offset_mid: 0,
    offset_hi: 0,
    reserved: 0,
};

struct IdtStore(core::cell::UnsafeCell<[Entry; 256]>);

// SAFETY: single vCPU, no preemption of the mutation sites: gates are only
// written while interrupts are off, and the CPU only reads the table when an
// interrupt or fault is delivered.
unsafe impl Sync for IdtStore {}

static IDT: IdtStore = IdtStore(core::cell::UnsafeCell::new([VACANT; 256]));

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

/// Point `vector` at `stub`: interrupt gate, DPL 0, the boot GDT's 64-bit
/// code segment.
pub fn set_gate(vector: u8, stub: extern "C" fn()) {
    let addr = stub as usize as u64;
    let entry = Entry {
        offset_lo: addr as u16,
        selector: 0x08,  // boot GDT 64-bit code segment
        options: 0x8E00, // present | interrupt gate | DPL 0 | IST 0
        offset_mid: (addr >> 16) as u16,
        offset_hi: (addr >> 32) as u32,
        reserved: 0,
    };
    // SAFETY: see IdtStore — gates are set only while interrupts are off.
    unsafe { (*IDT.0.get())[vector as usize] = entry };
}

/// Load the IDT register.
pub fn load() {
    let idtr = Idtr {
        limit: (core::mem::size_of::<[Entry; 256]>() - 1) as u16,
        base: IDT.0.get() as u64,
    };
    // SAFETY: IDT is 'static, so the base stays valid after lidt returns.
    unsafe { core::arch::asm!("lidt [{}]", in(reg) &idtr, options(nostack)) };
}

extern "C" fn swint_handler() {
    SWINT_COUNT.fetch_add(1, SeqCst);
}

extern "C" fn timer_handler() {
    TIMER_TICKS.fetch_add(1, SeqCst);
    crate::io::outb(0x20, 0x20); // EOI to the master PIC
}

extern "C" fn apic_timer_handler() {
    APIC_TICKS.fetch_add(1, SeqCst);
    crate::apic::eoi(); // EOI to the LAPIC (MMIO offset 0xB0)
}

unsafe extern "C" fn ud_handler(rip_slot: *mut u64) {
    FAULT_COUNT.fetch_add(1, SeqCst);
    // SAFETY: rip_slot points at the saved RIP in the live interrupt frame.
    unsafe { *rip_slot = (*rip_slot).wrapping_add(FAULT_SKIP.load(SeqCst)) };
}

unsafe extern "C" fn gp_handler(rip_slot: *mut u64) {
    GP_COUNT.fetch_add(1, SeqCst);
    // SAFETY: same frame contract as ud_handler.
    unsafe { ud_handler(rip_slot) };
}

/// Stub for the vector 0x40 software-interrupt self-test.
///
/// No error code: the CPU pushes 5 qwords, so RSP ≡ 8 (mod 16) here and the
/// 9 register pushes restore 16-byte alignment for the call.
#[unsafe(naked)]
pub extern "C" fn swint_stub() {
    naked_asm!(
        "push rax", "push rcx", "push rdx", "push rsi", "push rdi",
        "push r8", "push r9", "push r10", "push r11",
        "call {h}",
        "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi",
        "pop rsi", "pop rdx", "pop rcx", "pop rax",
        "iretq",
        h = sym swint_handler,
    );
}

/// Stub for vector 0x20 (PIT via remapped master PIC, IRQ0).
#[unsafe(naked)]
pub extern "C" fn timer_stub() {
    naked_asm!(
        "push rax", "push rcx", "push rdx", "push rsi", "push rdi",
        "push r8", "push r9", "push r10", "push r11",
        "call {h}",
        "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi",
        "pop rsi", "pop rdx", "pop rcx", "pop rax",
        "iretq",
        h = sym timer_handler,
    );
}

/// Stub for the LAPIC timer (the `irq-landing` payload arms it at vector 0x40,
/// reusing the software-interrupt vector since the PIC is left masked). Like
/// [`timer_stub`] but the EOI goes to the LAPIC, not the master PIC.
#[unsafe(naked)]
pub extern "C" fn apic_timer_stub() {
    naked_asm!(
        "push rax", "push rcx", "push rdx", "push rsi", "push rdi",
        "push r8", "push r9", "push r10", "push r11",
        "call {h}",
        "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi",
        "pop rsi", "pop rdx", "pop rcx", "pop rax",
        "iretq",
        h = sym apic_timer_handler,
    );
}

/// #UD (vector 6) stub: count the fault and skip the faulting instruction.
/// No error code; saved RIP sits above the 9 pushed registers (72 bytes).
#[unsafe(naked)]
pub extern "C" fn ud_stub() {
    naked_asm!(
        "push rax", "push rcx", "push rdx", "push rsi", "push rdi",
        "push r8", "push r9", "push r10", "push r11",
        "lea rdi, [rsp + 72]",
        "call {h}",
        "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi",
        "pop rsi", "pop rdx", "pop rcx", "pop rax",
        "iretq",
        h = sym ud_handler,
    );
}

/// #GP (vector 13) stub: as [`ud_stub`], but the CPU pushes an error code
/// (6 qwords, RSP ≡ 0 mod 16 here — hence the alignment pad) which must be
/// dropped before `iretq`.
#[unsafe(naked)]
pub extern "C" fn gp_stub() {
    naked_asm!(
        "push rax", "push rcx", "push rdx", "push rsi", "push rdi",
        "push r8", "push r9", "push r10", "push r11",
        "sub rsp, 8",          // ABI stack alignment for the call
        "lea rdi, [rsp + 88]", // saved RIP: 8 pad + 72 regs + 8 error code
        "call {h}",
        "add rsp, 8",
        "pop r11", "pop r10", "pop r9", "pop r8", "pop rdi",
        "pop rsi", "pop rdx", "pop rcx", "pop rax",
        "add rsp, 8",          // drop the error code
        "iretq",
        h = sym gp_handler,
    );
}
