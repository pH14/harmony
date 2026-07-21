// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny deterministic register VM used to test the harness (and later as a
//! sanity oracle next to the real VMM).
//!
//! Normative state (tests depend on it): 8 × u64 registers `r0..r7`, 65 536
//! bytes of memory, a program counter, an append-only output log, an
//! xorshift64* PRNG seeded from `spawn(seed)` (zero seed maps to
//! [`ZERO_SEED_STATE`]), and a halted flag. `work` = instructions retired;
//! every instruction costs exactly 1 work unit.

use crate::flaky::{Perturbable, Perturbation};
use crate::{RunOutcome, Subject, SubjectError, SubjectFactory};
use sha2::{Digest, Sha256};

/// Bytes of toy-machine memory.
pub const MEM_SIZE: usize = 65_536;
/// `LOAD`/`STORE` addresses are reduced modulo this, so an 8-byte
/// little-endian access always fits in memory.
pub const ADDR_MOD: u64 = 65_528;
/// PRNG state substituted for seed 0 (xorshift64* has no zero state).
/// Note `spawn(0)` and `spawn(ZERO_SEED_STATE)` are therefore identical.
pub const ZERO_SEED_STATE: u64 = 0x9E37_79B9_7F4A_7C15;

/// One toy-machine instruction. Register indices are taken modulo 8 at
/// execution time, so every encoding is safe to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instr {
    /// `rd = imm`.
    Loadi {
        /// Destination register.
        rd: u8,
        /// Immediate value.
        imm: u64,
    },
    /// `rd = rd + rs` (wrapping).
    Add {
        /// Destination register.
        rd: u8,
        /// Source register.
        rs: u8,
    },
    /// `rd = rd - rs` (wrapping).
    Sub {
        /// Destination register.
        rd: u8,
        /// Source register.
        rs: u8,
    },
    /// `rd = rd ^ rs`.
    Xor {
        /// Destination register.
        rd: u8,
        /// Source register.
        rs: u8,
    },
    /// `rd = mem[r[rs] % ADDR_MOD]` (u64, little-endian).
    Load {
        /// Destination register.
        rd: u8,
        /// Register holding the address.
        rs: u8,
    },
    /// `mem[r[rd] % ADDR_MOD] = r[rs]` (u64, little-endian).
    Store {
        /// Register holding the address.
        rd: u8,
        /// Source register.
        rs: u8,
    },
    /// `if r[rs] != 0 { pc = target }`.
    Jnz {
        /// Register tested against zero.
        rs: u8,
        /// Program counter to jump to when taken.
        target: u32,
    },
    /// `rd = next xorshift64* value`.
    Rand {
        /// Destination register.
        rd: u8,
    },
    /// Append `r[rs]` as 8 little-endian bytes to the output log.
    Out {
        /// Source register.
        rs: u8,
    },
    /// Set the halted flag.
    Halt,
}

/// Tiny assembler helpers for writing test programs by hand.
pub mod asm {
    use super::Instr;

    /// `LOADI rd, imm`.
    pub fn loadi(rd: u8, imm: u64) -> Instr {
        Instr::Loadi { rd, imm }
    }
    /// `ADD rd, rs`.
    pub fn add(rd: u8, rs: u8) -> Instr {
        Instr::Add { rd, rs }
    }
    /// `SUB rd, rs`.
    pub fn sub(rd: u8, rs: u8) -> Instr {
        Instr::Sub { rd, rs }
    }
    /// `XOR rd, rs`.
    pub fn xor(rd: u8, rs: u8) -> Instr {
        Instr::Xor { rd, rs }
    }
    /// `LOAD rd, [rs]`.
    pub fn load(rd: u8, rs: u8) -> Instr {
        Instr::Load { rd, rs }
    }
    /// `STORE [rd], rs`.
    pub fn store(rd: u8, rs: u8) -> Instr {
        Instr::Store { rd, rs }
    }
    /// `JNZ rs, target_pc`.
    pub fn jnz(rs: u8, target: u32) -> Instr {
        Instr::Jnz { rs, target }
    }
    /// `RAND rd`.
    pub fn rand(rd: u8) -> Instr {
        Instr::Rand { rd }
    }
    /// `OUT rs`.
    pub fn out(rs: u8) -> Instr {
        Instr::Out { rs }
    }
    /// `HALT`.
    pub fn halt() -> Instr {
        Instr::Halt
    }
}

/// Register index modulo the register count.
fn reg(i: u8) -> usize {
    usize::from(i & 7)
}

/// One xorshift64* step: advance `state`, return the next output value.
/// `state` must be nonzero (a zero state is a fixed point producing zeros).
pub(crate) fn xorshift64star(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// The toy deterministic register VM. See the module docs for the state model.
#[derive(Debug, Clone)]
pub struct ToyMachine {
    program: Vec<Instr>,
    regs: [u64; 8],
    mem: Box<[u8]>,
    pc: u64,
    out_log: Vec<u8>,
    prng: u64,
    halted: bool,
    work: u64,
}

impl ToyMachine {
    /// Create a machine with zeroed registers/memory/pc, an empty output log,
    /// and PRNG state `seed` (0 is mapped to [`ZERO_SEED_STATE`]).
    pub fn new(program: Vec<Instr>, seed: u64) -> Self {
        Self {
            program,
            regs: [0; 8],
            mem: vec![0; MEM_SIZE].into_boxed_slice(),
            pc: 0,
            out_log: Vec::new(),
            prng: if seed == 0 { ZERO_SEED_STATE } else { seed },
            halted: false,
            work: 0,
        }
    }

    /// Execute one instruction. Running off the end of the program halts the
    /// machine without retiring an instruction (no work is counted).
    fn step(&mut self) {
        let Some(&instr) = self
            .program
            .get(usize::try_from(self.pc).unwrap_or(usize::MAX))
        else {
            self.halted = true;
            return;
        };
        self.work += 1;
        self.pc += 1;
        match instr {
            Instr::Loadi { rd, imm } => self.regs[reg(rd)] = imm,
            Instr::Add { rd, rs } => {
                self.regs[reg(rd)] = self.regs[reg(rd)].wrapping_add(self.regs[reg(rs)]);
            }
            Instr::Sub { rd, rs } => {
                self.regs[reg(rd)] = self.regs[reg(rd)].wrapping_sub(self.regs[reg(rs)]);
            }
            Instr::Xor { rd, rs } => self.regs[reg(rd)] ^= self.regs[reg(rs)],
            Instr::Load { rd, rs } => {
                // Always < ADDR_MOD, so the cast and the 8-byte slice are in range.
                let addr = (self.regs[reg(rs)] % ADDR_MOD) as usize;
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&self.mem[addr..addr + 8]);
                self.regs[reg(rd)] = u64::from_le_bytes(buf);
            }
            Instr::Store { rd, rs } => {
                let addr = (self.regs[reg(rd)] % ADDR_MOD) as usize;
                self.mem[addr..addr + 8].copy_from_slice(&self.regs[reg(rs)].to_le_bytes());
            }
            Instr::Jnz { rs, target } => {
                if self.regs[reg(rs)] != 0 {
                    self.pc = u64::from(target);
                }
            }
            Instr::Rand { rd } => self.regs[reg(rd)] = xorshift64star(&mut self.prng),
            Instr::Out { rs } => {
                self.out_log
                    .extend_from_slice(&self.regs[reg(rs)].to_le_bytes());
            }
            Instr::Halt => self.halted = true,
        }
    }
}

impl Subject for ToyMachine {
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, SubjectError> {
        if target < self.work {
            return Err(SubjectError::TargetBehind {
                target,
                current: self.work,
            });
        }
        while self.work < target && !self.halted {
            self.step();
        }
        Ok(if self.halted {
            RunOutcome::Halted
        } else {
            RunOutcome::ReachedTarget
        })
    }

    fn work(&self) -> u64 {
        self.work
    }

    fn state_hash(&self) -> [u8; 32] {
        // Canonical layout, all integers little-endian:
        //   "unison-toy-v1"    17-byte domain tag
        //   r0..r7                 8 × 8 bytes
        //   pc                     8 bytes
        //   memory                 65 536 bytes
        //   output log length      8 bytes (u64)
        //   output log             <length> bytes
        //   PRNG state             8 bytes
        //   halted flag            1 byte (0x00 or 0x01)
        let mut h = Sha256::new();
        h.update(b"unison-toy-v1");
        for r in &self.regs {
            h.update(r.to_le_bytes());
        }
        h.update(self.pc.to_le_bytes());
        h.update(&self.mem);
        h.update((self.out_log.len() as u64).to_le_bytes());
        h.update(&self.out_log);
        h.update(self.prng.to_le_bytes());
        h.update([u8::from(self.halted)]);
        h.finalize().into()
    }

    fn observable_digest(&self) -> [u8; 32] {
        // Only the guest-emitted output log — NOT registers, memory, the PRNG
        // state, or the halted flag. A domain tag distinct from `state_hash`'s
        // so the two digests can never collide for the same underlying bytes.
        // This is the seed-INDEPENDENT view a pure payload must keep stable and
        // an RNG-consuming payload must vary (acceptance-suite O3).
        let mut h = Sha256::new();
        h.update(b"unison-toy-observable-v1");
        h.update((self.out_log.len() as u64).to_le_bytes());
        h.update(&self.out_log);
        h.finalize().into()
    }
}

impl Perturbable for ToyMachine {
    fn apply_perturbation(&mut self, p: &Perturbation) {
        match *p {
            Perturbation::XorReg { reg: r, mask } => self.regs[reg(r)] ^= mask,
            Perturbation::XorPrng { mask } => self.prng ^= mask,
            Perturbation::ForceHalt => self.halted = true,
        }
    }
}

/// Creates [`ToyMachine`]s running a fixed program; the PRNG seed comes from
/// [`SubjectFactory::spawn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToyFactory {
    /// The program every spawned machine runs.
    pub program: Vec<Instr>,
}

impl SubjectFactory for ToyFactory {
    type M = ToyMachine;

    fn spawn(&self, seed: u64) -> ToyMachine {
        ToyMachine::new(self.program.clone(), seed)
    }
}

/// A generated test program plus its exact (seed-independent) run length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedProgram {
    /// The instructions.
    pub instrs: Vec<Instr>,
    /// Exact work count at which a fresh machine halts. Independent of the
    /// machine seed: control flow only depends on r6/r7, which the random
    /// body never touches.
    pub work_to_halt: u64,
}

/// Deterministically generate a random program guaranteed to keep running for
/// at least `min_work` instructions before halting (`work_to_halt > min_work`).
///
/// Shape: a bounded loop skeleton with a random straight-line body —
/// `r7` is the loop counter and `r6` a scratch decrement; the body is 4–24
/// instructions drawn from {LOADI, ADD, SUB, XOR, LOAD, STORE, RAND, OUT}
/// over `r0..r5` only, with no jumps and no halts. The same `gen_seed`
/// always yields the same program (the generator is its own xorshift64*
/// stream, unrelated to machine seeds).
pub fn generate_program(gen_seed: u64, min_work: u64) -> GeneratedProgram {
    let mut s = if gen_seed == 0 {
        ZERO_SEED_STATE
    } else {
        gen_seed
    };
    let body_len = 4 + xorshift64star(&mut s) % 21; // 4..=24
    let mut body = Vec::with_capacity(body_len as usize);
    for _ in 0..body_len {
        let op = xorshift64star(&mut s) % 8;
        // Cast is exact: % 6 < 256.
        let rd = (xorshift64star(&mut s) % 6) as u8;
        let rs = (xorshift64star(&mut s) % 6) as u8;
        body.push(match op {
            0 => Instr::Loadi {
                rd,
                imm: xorshift64star(&mut s),
            },
            1 => Instr::Add { rd, rs },
            2 => Instr::Sub { rd, rs },
            3 => Instr::Xor { rd, rs },
            4 => Instr::Load { rd, rs },
            5 => Instr::Store { rd, rs },
            6 => Instr::Rand { rd },
            _ => Instr::Out { rs },
        });
    }
    // Each loop iteration retires body_len + 3 instructions (body, LOADI r6,
    // SUB r7, JNZ); one LOADI before the loop and one HALT after it.
    let per_iter = body_len + 3;
    let iters = min_work / per_iter + 1;
    let mut instrs = Vec::with_capacity(body.len() + 5);
    instrs.push(Instr::Loadi { rd: 7, imm: iters });
    instrs.extend_from_slice(&body);
    instrs.push(Instr::Loadi { rd: 6, imm: 1 });
    instrs.push(Instr::Sub { rd: 7, rs: 6 });
    instrs.push(Instr::Jnz { rs: 7, target: 1 });
    instrs.push(Instr::Halt);
    GeneratedProgram {
        instrs,
        work_to_halt: iters.saturating_mul(per_iter).saturating_add(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(program: Vec<Instr>, seed: u64, target: u64) -> ToyMachine {
        let mut m = ToyMachine::new(program, seed);
        m.run_to(target).unwrap();
        m
    }

    #[test]
    fn alu_semantics() {
        let prog = vec![
            asm::loadi(0, 10),
            asm::loadi(1, 3),
            asm::add(0, 1), // r0 = 13
            asm::sub(0, 1), // r0 = 10
            asm::xor(0, 1), // r0 = 9
            asm::halt(),
        ];
        let m = run(prog, 1, 100);
        assert_eq!(m.regs[0], 9);
        assert_eq!(m.work(), 6);
        assert!(m.halted);
    }

    #[test]
    fn wrapping_arithmetic_does_not_panic() {
        let prog = vec![
            asm::loadi(0, u64::MAX),
            asm::loadi(1, 2),
            asm::add(0, 1), // wraps to 1
            asm::sub(1, 0), // 2 - 1 = 1
            asm::sub(0, 1), // 0
            asm::sub(0, 1), // wraps to MAX
            asm::halt(),
        ];
        let m = run(prog, 1, 100);
        assert_eq!(m.regs[0], u64::MAX);
    }

    #[test]
    fn load_store_round_trip_with_address_wrap() {
        let prog = vec![
            asm::loadi(0, 65_530),           // address, wraps to 65_530 % 65_528 = 2
            asm::loadi(1, 0xDEAD_BEEF_CAFE), // value
            asm::store(0, 1),
            asm::loadi(2, 2), // same effective address, directly
            asm::load(3, 2),
            asm::halt(),
        ];
        let m = run(prog, 1, 100);
        assert_eq!(m.regs[3], 0xDEAD_BEEF_CAFE);
        assert_eq!(&m.mem[2..10], &0xDEAD_BEEF_CAFEu64.to_le_bytes());
    }

    #[test]
    fn jnz_taken_and_not_taken() {
        // r0 = 2; loop: r0 -= 1; jnz r0, loop; halt
        let prog = vec![
            asm::loadi(0, 2),
            asm::loadi(1, 1),
            asm::sub(0, 1),
            asm::jnz(0, 2),
            asm::halt(),
        ];
        let m = run(prog, 1, 100);
        // loadi, loadi, sub, jnz(taken), sub, jnz(not taken), halt = 7
        assert_eq!(m.work(), 7);
        assert_eq!(m.regs[0], 0);
    }

    #[test]
    fn running_off_the_end_halts_without_work() {
        let prog = vec![asm::loadi(0, 1)];
        let mut m = ToyMachine::new(prog, 1);
        assert_eq!(m.run_to(10).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 1); // only the LOADI retired
        // Same for a JNZ jumping past the end.
        let prog = vec![asm::loadi(0, 1), asm::jnz(0, 99)];
        let mut m = ToyMachine::new(prog, 1);
        assert_eq!(m.run_to(10).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 2);
    }

    #[test]
    fn out_appends_little_endian() {
        let prog = vec![asm::loadi(0, 0x0102_0304), asm::out(0), asm::out(0)];
        let m = run(prog, 1, 3);
        let mut expect = 0x0102_0304u64.to_le_bytes().to_vec();
        expect.extend_from_slice(&0x0102_0304u64.to_le_bytes());
        assert_eq!(m.out_log, expect);
    }

    #[test]
    fn rand_is_seed_deterministic_and_zero_seed_maps_to_constant() {
        let prog = vec![asm::rand(0), asm::rand(1), asm::halt()];
        let a = run(prog.clone(), 7, 100);
        let b = run(prog.clone(), 7, 100);
        let c = run(prog.clone(), 8, 100);
        assert_eq!(a.state_hash(), b.state_hash());
        assert_ne!(a.state_hash(), c.state_hash());
        let zero = run(prog.clone(), 0, 100);
        let subst = run(prog, ZERO_SEED_STATE, 100);
        assert_eq!(zero.state_hash(), subst.state_hash());
    }

    #[test]
    fn register_indices_wrap_modulo_8() {
        let prog = vec![asm::loadi(8, 5), asm::halt()]; // 8 & 7 == 0
        let m = run(prog, 1, 100);
        assert_eq!(m.regs[0], 5);
    }

    #[test]
    fn run_to_backwards_is_an_error() {
        let prog = vec![asm::loadi(0, 1), asm::loadi(1, 1), asm::halt()];
        let mut m = ToyMachine::new(prog, 1);
        m.run_to(2).unwrap();
        assert_eq!(
            m.run_to(1),
            Err(SubjectError::TargetBehind {
                target: 1,
                current: 2
            })
        );
    }

    #[test]
    fn halt_exactly_at_target_reports_halted() {
        let prog = vec![asm::loadi(0, 1), asm::halt()];
        let mut m = ToyMachine::new(prog, 1);
        assert_eq!(m.run_to(2).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 2);
        // Halted machine: forward run_to is a no-op returning Halted.
        assert_eq!(m.run_to(50).unwrap(), RunOutcome::Halted);
        assert_eq!(m.work(), 2);
    }

    #[test]
    fn state_hash_is_pure_and_state_sensitive() {
        let prog = vec![asm::loadi(0, 1), asm::out(0), asm::halt()];
        let mut m = ToyMachine::new(prog, 9);
        let h0 = m.state_hash();
        assert_eq!(h0, m.state_hash(), "hashing must not change state");
        m.run_to(1).unwrap();
        let h1 = m.state_hash();
        assert_ne!(h0, h1);
        m.run_to(2).unwrap();
        assert_ne!(h1, m.state_hash());
    }

    #[test]
    fn generated_programs_run_at_least_min_work() {
        for gen_seed in [0u64, 1, 2, 0xFFFF_FFFF_FFFF_FFFF] {
            for min_work in [1u64, 7, 100, 5000] {
                let p = generate_program(gen_seed, min_work);
                assert!(p.work_to_halt > min_work);
                let mut m = ToyMachine::new(p.instrs.clone(), 42);
                assert_eq!(m.run_to(min_work).unwrap(), RunOutcome::ReachedTarget);
                // Halts at exactly work_to_halt, for any machine seed.
                assert_eq!(m.run_to(p.work_to_halt + 10).unwrap(), RunOutcome::Halted);
                assert_eq!(m.work(), p.work_to_halt);
            }
        }
    }

    #[test]
    fn generator_is_deterministic() {
        assert_eq!(generate_program(99, 1000), generate_program(99, 1000));
    }

    #[test]
    fn observable_digest_excludes_latent_prng_state() {
        // A pure program (no RAND): its observable output is seed-independent,
        // yet state_hash differs because the latent PRNG is seeded from `seed`.
        // This is exactly why O3 must use observable_digest, not state_hash.
        let pure = vec![asm::loadi(0, 0xABCD), asm::out(0), asm::halt()];
        let a = run(pure.clone(), 7, 100);
        let b = run(pure, 8, 100);
        assert_eq!(
            a.observable_digest(),
            b.observable_digest(),
            "pure output must not depend on the seed"
        );
        assert_ne!(
            a.state_hash(),
            b.state_hash(),
            "state_hash still differs via the latent seeded PRNG"
        );
        // And the observable digest is genuinely distinct from state_hash.
        assert_ne!(a.observable_digest(), a.state_hash());
    }

    #[test]
    fn observable_digest_tracks_rng_output() {
        // A RAND-consuming, control-flow-stable program: observable output
        // varies with the seed (the seed reached emitted bytes).
        let rng = vec![asm::rand(0), asm::out(0), asm::halt()];
        let a = run(rng.clone(), 7, 100);
        let b = run(rng, 8, 100);
        assert_ne!(
            a.observable_digest(),
            b.observable_digest(),
            "RNG output must depend on the seed"
        );
        // Same seed ⇒ identical observable output (purity of the accessor).
        let rng2 = vec![asm::rand(0), asm::out(0), asm::halt()];
        let c = run(rng2, 7, 100);
        assert_eq!(a.observable_digest(), c.observable_digest());
    }

    #[test]
    fn observable_digest_is_pure() {
        let prog = vec![asm::loadi(0, 5), asm::out(0), asm::halt()];
        let mut m = ToyMachine::new(prog, 3);
        let d0 = m.observable_digest();
        assert_eq!(d0, m.observable_digest(), "hashing must not change state");
        m.run_to(2).unwrap(); // executes the OUT
        assert_ne!(d0, m.observable_digest(), "output changed the digest");
    }
}
