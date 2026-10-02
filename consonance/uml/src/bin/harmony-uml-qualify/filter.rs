// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

use serde_json::{Value, json};

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_ALU_AND_K: u16 = 0x54;
const BPF_RET_K: u16 = 0x06;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const RET_ERRNO: u32 = 0x0005_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const OFFSET_NR: u32 = 0;
const OFFSET_ARCH: u32 = 4;
const OFFSET_ARG0: u32 = 16;
const OFFSET_ARG1: u32 = 24;
const KVM_IOCTL_TYPE: u32 = 0xae00;
const INVALID_PTRACE_REQUEST: libc::c_long = 0x7fff_ffff;
const PERSONALITY_QUERY: u32 = 0xffff_ffff;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

fn jump(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: BPF_JMP_JEQ_K,
        jt,
        jf,
        k,
    }
}

fn number(syscall: libc::c_long) -> u32 {
    u32::try_from(syscall).unwrap_or(u32::MAX)
}

fn host_program() -> Vec<libc::sock_filter> {
    vec![
        statement(BPF_LD_W_ABS, OFFSET_ARCH),
        jump(AUDIT_ARCH, 1, 0),
        statement(BPF_RET_K, RET_KILL_PROCESS),
        statement(BPF_LD_W_ABS, OFFSET_NR),
        jump(number(libc::SYS_ptrace), 0, 1),
        statement(BPF_RET_K, RET_ERRNO | libc::EPERM as u32),
        jump(number(libc::SYS_ioctl), 0, 4),
        statement(BPF_LD_W_ABS, OFFSET_ARG1),
        statement(BPF_ALU_AND_K, 0xff00),
        jump(KVM_IOCTL_TYPE, 0, 1),
        statement(BPF_RET_K, RET_ERRNO | libc::EPERM as u32),
        statement(BPF_RET_K, RET_ALLOW),
    ]
}

fn seccomp_install_program() -> Vec<libc::sock_filter> {
    vec![
        statement(BPF_LD_W_ABS, OFFSET_ARCH),
        jump(AUDIT_ARCH, 1, 0),
        statement(BPF_RET_K, RET_KILL_PROCESS),
        statement(BPF_LD_W_ABS, OFFSET_NR),
        jump(number(libc::SYS_seccomp), 0, 1),
        statement(BPF_RET_K, RET_ERRNO | libc::EPERM as u32),
        jump(number(libc::SYS_prctl), 0, 3),
        statement(BPF_LD_W_ABS, OFFSET_ARG0),
        jump(libc::PR_SET_SECCOMP as u32, 0, 1),
        statement(BPF_RET_K, RET_ERRNO | libc::EPERM as u32),
        statement(BPF_RET_K, RET_ALLOW),
    ]
}

fn personality_program() -> Vec<libc::sock_filter> {
    vec![
        statement(BPF_LD_W_ABS, OFFSET_ARCH),
        jump(AUDIT_ARCH, 1, 0),
        statement(BPF_RET_K, RET_KILL_PROCESS),
        statement(BPF_LD_W_ABS, OFFSET_NR),
        jump(number(libc::SYS_personality), 0, 3),
        statement(BPF_LD_W_ABS, OFFSET_ARG0),
        jump(PERSONALITY_QUERY, 1, 0),
        statement(BPF_RET_K, RET_ERRNO | libc::EPERM as u32),
        statement(BPF_RET_K, RET_ALLOW),
    ]
}

fn install(program: &mut [libc::sock_filter]) -> io::Result<()> {
    let program = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_mut_ptr(),
    };
    // SAFETY: prctl(2) with PR_SET_NO_NEW_PRIVS takes only integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `program` points to `len` initialized sock_filter entries that
    // outlive the call; the kernel copies the program before returning.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn errno_of(result: libc::c_long) -> Option<i32> {
    (result == -1).then(|| io::Error::last_os_error().raw_os_error().unwrap_or(0))
}

pub fn deny_host_virtualization() -> Result<Value, String> {
    install(&mut host_program())
        .map_err(|error| format!("cannot install the denial filter: {error}"))?;
    // SAFETY: an invalid ptrace request with null pointers dereferences no memory.
    let ptrace = errno_of(unsafe {
        libc::syscall(
            libc::SYS_ptrace,
            INVALID_PTRACE_REQUEST,
            0,
            std::ptr::null_mut::<libc::c_void>(),
            std::ptr::null_mut::<libc::c_void>(),
        )
    });
    // SAFETY: ioctl on fd -1 with a null argument dereferences no memory.
    let kvm = errno_of(libc::c_long::from(unsafe {
        libc::ioctl(-1, 0xae00, std::ptr::null_mut::<libc::c_void>())
    }));
    if ptrace != Some(libc::EPERM) || kvm != Some(libc::EPERM) {
        return Err(format!(
            "denial filter is not effective: ptrace errno {ptrace:?}, KVM ioctl errno {kvm:?}"
        ));
    }
    Ok(json!({"ptrace": "EPERM", "kvm_ioctl": "EPERM"}))
}

pub fn deny_seccomp_install(command: &mut Command) {
    let mut program = seccomp_install_program();
    // SAFETY: the closure runs in the forked child before exec. It installs a
    // program allocated before the fork and calls only prctl(2), which is
    // async-signal-safe; it performs no allocation or locking.
    unsafe {
        command.pre_exec(move || install(&mut program));
    }
}

pub fn deny_personality_change(command: &mut Command) {
    let mut program = personality_program();
    // SAFETY: the closure runs in the forked child before exec. It installs a
    // program allocated before the fork and calls only prctl(2), which is
    // async-signal-safe; it performs no allocation or locking.
    unsafe {
        command.pre_exec(move || install(&mut program));
    }
}
