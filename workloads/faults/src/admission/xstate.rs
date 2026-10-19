// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};

use iced_x86::{Instruction, Mnemonic as M, OpKind, Register as R};

pub(super) fn name(i: &Instruction) -> Option<&'static str> {
    Some(match i.mnemonic() {
        M::Fxsave => "FXSAVE",
        M::Fxsave64 => "FXSAVE64",
        M::Xsave => "XSAVE",
        M::Xsave64 => "XSAVE64",
        M::Xsavec => "XSAVEC",
        M::Xsavec64 => "XSAVEC64",
        M::Xsaveopt => "XSAVEOPT",
        M::Xsaveopt64 => "XSAVEOPT64",
        M::Xsaves => "XSAVES",
        M::Xsaves64 => "XSAVES64",
        M::Xgetbv => "XGETBV",
        _ => return None,
    })
}

pub(super) fn is_site(name: &str) -> bool {
    matches!(
        name,
        "FXSAVE"
            | "FXSAVE64"
            | "XSAVE"
            | "XSAVE64"
            | "XSAVEC"
            | "XSAVEC64"
            | "XSAVEOPT"
            | "XSAVEOPT64"
            | "XSAVES"
            | "XSAVES64"
            | "XGETBV"
    )
}

pub(super) fn validate(
    decoded: BTreeMap<u64, Instruction>,
    targets: &BTreeSet<u64>,
) -> Result<(), String> {
    let instructions: Vec<_> = decoded.into_values().collect();
    let mut predecessors: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut overlapping = BTreeSet::new();
    let mut active = Vec::new();
    for (index, i) in instructions.iter().enumerate() {
        predecessors.entry(i.next_ip()).or_default().push(index);
        active.retain(|&prior: &usize| instructions[prior].next_ip() > i.ip());
        if !active.is_empty() {
            overlapping.insert(index);
            overlapping.extend(active.iter().copied());
        }
        active.push(index);
    }
    let edge = |index: usize| {
        !targets.contains(&instructions[index].ip())
            && !overlapping.contains(&index)
            && predecessors
                .get(&instructions[index].ip())
                .is_some_and(|p| p.len() == 1)
    };
    for (index, i) in instructions.iter().enumerate() {
        let Some(name) = name(i) else { continue };
        if i.mnemonic() == M::Xgetbv {
            let mut cursor = index;
            let mut zero = false;
            let mut wanted = R::ECX;
            for _ in 0..32 {
                if !edge(cursor) {
                    break;
                }
                let before = predecessors[&instructions[cursor].ip()][0];
                if overlapping.contains(&before) {
                    break;
                }
                let instruction = &instructions[before];
                let destination = instruction.op0_register();
                let writes_wanted = instruction.op0_kind() == OpKind::Register
                    && destination.full_register32() == wanted;
                if writes_wanted && destination.size() >= 4 {
                    if regs(instruction, M::Xor, destination, destination)
                        || imm(instruction, M::Mov, destination) == Some(0)
                    {
                        zero = true;
                        break;
                    }
                    let source = instruction.op1_register();
                    if instruction.mnemonic() == M::Mov
                        && instruction.op1_kind() == OpKind::Register
                        && source.size() >= 4
                        && (source.is_gpr32() || source.is_gpr64())
                    {
                        wanted = source.full_register32();
                        cursor = before;
                        continue;
                    }
                }
                let preserved = match instruction.mnemonic() {
                    M::Cpuid => !matches!(wanted, R::EAX | R::EBX | R::ECX | R::EDX),
                    M::Push => wanted != R::ESP,
                    _ if instruction.is_jcc_short_or_near() => true,
                    M::Cmp | M::Test => true,
                    M::Mov | M::Lea | M::And | M::Or | M::Xor | M::Add | M::Sub => {
                        !writes_wanted
                            && (instruction.op0_kind() != OpKind::Memory
                                || instruction.memory_base().full_register32() != wanted
                                    && instruction.memory_index().full_register32() != wanted)
                    }
                    _ => false,
                };
                if !preserved {
                    break;
                }
                cursor = before;
            }
            if !zero {
                return Err(format!("unproved XGETBV selector at 0x{:x}", i.ip()));
            }
        } else {
            let span = trampoline(&instructions, index);
            let safe = span.is_some_and(|(first, last)| {
                (first..=last).all(|n| {
                    let i = &instructions[n];
                    !overlapping.contains(&n)
                        && i.segment_prefix() == R::None
                        && !i.has_lock_prefix()
                        && (!i.has_repe_prefix() || i.mnemonic() == M::Endbr64)
                        && (!i.has_repne_prefix() || i.mnemonic() == M::Jmp)
                }) && (first + 1..=last)
                    .all(|n| edge(n) && instructions[n - 1].next_ip() == instructions[n].ip())
            });
            if !safe {
                return Err(format!(
                    "save instruction outside a loader trampoline: 0x{:x} {name}",
                    i.ip()
                ));
            }
        }
    }
    Ok(())
}

fn regs(i: &Instruction, m: M, a: R, b: R) -> bool {
    i.mnemonic() == m
        && i.op0_kind() == OpKind::Register
        && i.op1_kind() == OpKind::Register
        && i.op0_register() == a
        && i.op1_register() == b
}
fn imm(i: &Instruction, m: M, r: R) -> Option<u64> {
    (i.mnemonic() == m && i.op0_kind() == OpKind::Register && i.op0_register() == r)
        .then(|| match i.op1_kind() {
            OpKind::Immediate8 => Some(u64::from(i.immediate8())),
            OpKind::Immediate32 => Some(u64::from(i.immediate32())),
            OpKind::Immediate64 => Some(i.immediate64()),
            OpKind::Immediate8to64 => Some(i.immediate8to64() as u64),
            OpKind::Immediate32to64 => Some(i.immediate32to64() as u64),
            _ => None,
        })
        .flatten()
}
fn memory(i: &Instruction, op: u32, base: R, displacement: u64) -> bool {
    i.segment_prefix() == R::None
        && i.op_kind(op) == OpKind::Memory
        && i.memory_base() == base
        && i.memory_index() == R::None
        && i.memory_displacement64() == displacement
}
fn load(i: &Instruction, r: R, base: R, displacement: u64) -> bool {
    i.mnemonic() == M::Mov && i.op0_register() == r && memory(i, 1, base, displacement)
}
fn store(i: &Instruction, base: R, displacement: u64, r: R) -> bool {
    i.mnemonic() == M::Mov && memory(i, 0, base, displacement) && i.op1_register() == r
}
fn spill_register(r: R) -> bool {
    matches!(
        r,
        R::RAX | R::RCX | R::RDX | R::RSI | R::RDI | R::R8 | R::R9 | R::R10 | R::R11
    )
}
fn spill(i: &Instruction) -> bool {
    i.segment_prefix() == R::None
        && i.mnemonic() == M::Mov
        && i.op0_kind() == OpKind::Memory
        && i.memory_base() == R::RSP
        && i.memory_index() == R::None
        && i.memory_displacement64() <= i32::MAX as u64
        && i.op1_kind() == OpKind::Register
        && spill_register(i.op1_register())
}
fn reload(i: &Instruction) -> bool {
    i.segment_prefix() == R::None
        && i.mnemonic() == M::Mov
        && i.op0_kind() == OpKind::Register
        && spill_register(i.op0_register())
        && i.op1_kind() == OpKind::Memory
        && i.memory_base() == R::RSP
        && i.memory_index() == R::None
        && i.memory_displacement64() <= i32::MAX as u64
}
fn near(i: &Instruction, mnemonic: M) -> bool {
    i.mnemonic() == mnemonic
        && matches!(
            i.op0_kind(),
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64
        )
}
fn take(
    instructions: &[Instruction],
    cursor: &mut usize,
    predicate: impl FnOnce(&Instruction) -> bool,
) -> bool {
    if instructions.get(*cursor).is_some_and(predicate) {
        *cursor += 1;
        true
    } else {
        false
    }
}

fn trampoline(v: &[Instruction], index: usize) -> Option<(usize, usize)> {
    let save = v.get(index)?;
    let legacy = matches!(save.mnemonic(), M::Fxsave | M::Fxsave64);
    if !(legacy
        || matches!(
            save.mnemonic(),
            M::Xsave | M::Xsave64 | M::Xsavec | M::Xsavec64
        ))
        || !memory(save, 0, R::RSP, 0x40)
    {
        return None;
    }
    let mut first = index;
    let mut count = 0;
    let mask = if legacy {
        None
    } else {
        while first > 0
            && count < 16
            && spill(&v[first - 1])
            && v[first - 1].op1_register() == R::RDX
            && v[first - 1].memory_displacement64() != 0
        {
            first -= 1;
            count += 1;
        }
        first = first.checked_sub(1)?;
        if !regs(&v[first], M::Xor, R::EDX, R::EDX) {
            return None;
        }
        first = first.checked_sub(1)?;
        Some(imm(&v[first], M::Mov, R::EAX)?)
    };
    count = 0;
    while first > 0 && count < 8 && spill(&v[first - 1]) {
        first -= 1;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    first = first.checked_sub(3)?;
    if !regs(&v[first], M::Mov, R::RBX, R::RSP)
        || imm(&v[first + 1], M::And, R::RSP)
            != Some(if legacy {
                (-16_i64) as u64
            } else {
                (-64_i64) as u64
            })
        || v[first + 2].mnemonic() != M::Sub
        || v[first + 2].op0_register() != R::RSP
        || legacy && imm(&v[first + 2], M::Sub, R::RSP) != Some(0x240)
    {
        return None;
    }
    first = first.checked_sub(1)?;
    let resolver = v[first].mnemonic() == M::Push && v[first].op0_register() == R::RBX;
    if resolver {
        if first > 0 && v[first - 1].mnemonic() == M::Endbr64 {
            first -= 1;
        }
    } else if !store(&v[first], R::RSP, (-24_i64) as u64, R::RBX) {
        return None;
    }
    let mut next = index + 1;
    if resolver
        && (!take(v, &mut next, |i| load(i, R::RSI, R::RBX, 16))
            || !take(v, &mut next, |i| load(i, R::RDI, R::RBX, 8)))
    {
        return None;
    }
    if !take(v, &mut next, |i| near(i, M::Call))
        || !take(v, &mut next, |i| {
            regs(i, M::Mov, if resolver { R::R11 } else { R::RCX }, R::RAX)
        })
    {
        return None;
    }
    if let Some(mask) = mask {
        if !take(v, &mut next, |i| imm(i, M::Mov, R::EAX) == Some(mask))
            || !take(v, &mut next, |i| regs(i, M::Xor, R::EDX, R::EDX))
            || !take(v, &mut next, |i| {
                i.mnemonic() == M::Xrstor && memory(i, 0, R::RSP, 0x40)
            })
        {
            return None;
        }
    } else if !take(v, &mut next, |i| {
        i.mnemonic()
            == if save.mnemonic() == M::Fxsave64 {
                M::Fxrstor64
            } else {
                M::Fxrstor
            }
            && memory(i, 0, R::RSP, 0x40)
    }) {
        return None;
    }
    if !resolver && !take(v, &mut next, |i| regs(i, M::Mov, R::RAX, R::RCX)) {
        return None;
    }
    count = 0;
    while count < 8 && take(v, &mut next, reload) {
        count += 1;
    }
    if count == 0
        || !take(v, &mut next, |i| regs(i, M::Mov, R::RSP, R::RBX))
        || !take(v, &mut next, |i| {
            load(
                i,
                R::RBX,
                R::RSP,
                if resolver { 0 } else { (-24_i64) as u64 },
            )
        })
    {
        return None;
    }
    if resolver {
        if !take(v, &mut next, |i| imm(i, M::Add, R::RSP) == Some(24))
            || !take(v, &mut next, |i| {
                i.mnemonic() == M::Jmp
                    && i.op0_kind() == OpKind::Register
                    && i.op0_register() == R::R11
            })
        {
            return None;
        }
    } else if !take(v, &mut next, |i| near(i, M::Jmp)) {
        return None;
    }
    Some((first, next - 1))
}
