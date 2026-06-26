#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Apply the deterministic-intercept KVM patch to a linux source tree.

Run from the root of the kernel tree.  Idempotent-ish: aborts loudly if any
anchor is missing or non-unique, so a partially-applied tree never silently
diverges.  Uses tabs exactly as the kernel sources do.
"""
import sys

def read(p):
    with open(p, "r") as f:
        return f.read()

def write(p, s):
    with open(p, "w") as f:
        f.write(s)

def exact(path, old, new):
    s = read(path)
    n = s.count(old)
    if n != 1:
        sys.exit("FAIL exact %s: anchor count=%d (need 1)\n--- anchor ---\n%s" % (path, n, old))
    write(path, s.replace(old, new, 1))
    print("ok exact  %s" % path)

def insert_after_line(path, needle, addition):
    s = read(path)
    lines = s.splitlines(keepends=True)
    idx = [i for i, l in enumerate(lines) if needle in l]
    if len(idx) != 1:
        sys.exit("FAIL after %s: needle %r count=%d (need 1)" % (path, needle, len(idx)))
    i = idx[0]
    lines.insert(i + 1, addition)
    write(path, "".join(lines))
    print("ok after  %s  (%r)" % (path, needle))

def insert_before_line(path, needle, addition):
    s = read(path)
    lines = s.splitlines(keepends=True)
    idx = [i for i, l in enumerate(lines) if needle in l]
    if len(idx) != 1:
        sys.exit("FAIL before %s: needle %r count=%d (need 1)" % (path, needle, len(idx)))
    i = idx[0]
    lines.insert(i, addition)
    write(path, "".join(lines))
    print("ok before %s  (%r)" % (path, needle))

UAPI = "include/uapi/linux/kvm.h"
HOST = "arch/x86/include/asm/kvm_host.h"
XH   = "arch/x86/kvm/x86.h"
XC   = "arch/x86/kvm/x86.c"
VMX  = "arch/x86/kvm/vmx/vmx.c"

# ---------------------------------------------------------------- patch 1: uapi
insert_after_line(UAPI, "#define KVM_EXIT_TDX",
    "#define KVM_EXIT_DETERMINISM      41\n")

DET_STRUCT = (
    "\t\t/* KVM_EXIT_DETERMINISM */\n"
    "\t\tstruct {\n"
    "#define KVM_DETERMINISM_RDTSC\t0\n"
    "#define KVM_DETERMINISM_RDTSCP\t1\n"
    "#define KVM_DETERMINISM_RDRAND\t2\n"
    "#define KVM_DETERMINISM_RDSEED\t3\n"
    "\t\t\t__u32 insn;     /* kernel -> user: KVM_DETERMINISM_* */\n"
    "\t\t\t__u32 width;    /* kernel -> user: result width in bytes */\n"
    "\t\t\t__u64 value;    /* user -> kernel: low `width` bytes -> dest / EDX:EAX */\n"
    "\t\t\t__u64 aux;      /* RDTSCP: user -> kernel IA32_TSC_AUX -> ECX */\n"
    "#define KVM_DETERMINISM_FLAG_CF\t(1 << 0) /* RNG: user requests CF=1 (success) */\n"
    "\t\t\t__u8 flags;     /* user -> kernel */\n"
    "\t\t\t__u8 dest_reg;  /* kernel -> user: RNG dest GPR (0=RAX..15=R15) */\n"
    "\t\t\t__u8 pad[6];\n"
    "\t\t} determinism;\n"
)
insert_before_line(UAPI, "/* Fix the size of the union. */", DET_STRUCT)

insert_after_line(UAPI, "#define KVM_CAP_GUEST_MEMFD_FLAGS 244",
    "#define KVM_CAP_X86_DETERMINISTIC_INTERCEPTS 245\n")

# ----------------------------------------------- patch 2: kvm core (cap + emul)
insert_after_line(HOST, "\tbool bus_lock_detection_enabled;",
    "\tbool deterministic_intercepts;\n")

insert_after_line(HOST, "int kvm_emulate_rdpmc(struct kvm_vcpu *vcpu);",
    "int kvm_emulate_rdtsc_intercept(struct kvm_vcpu *vcpu);\n"
    "int kvm_emulate_rdtscp_intercept(struct kvm_vcpu *vcpu);\n"
    "int kvm_emulate_rng_intercept(struct kvm_vcpu *vcpu, u32 insn,\n"
    "\t\t\t      unsigned int insn_len);\n")

insert_after_line(XH, "\tbool has_bus_lock_exit;",
    "\tbool has_deterministic_intercepts;\n")

EMUL = (
"static int complete_deterministic_tsc(struct kvm_vcpu *vcpu)\n"
"{\n"
"\tstruct kvm_run *run = vcpu->run;\n"
"\tu64 val = run->determinism.value;\n"
"\n"
"\tkvm_rax_write(vcpu, (u32)val);\n"
"\tkvm_rdx_write(vcpu, (u32)(val >> 32));\n"
"\tif (run->determinism.insn == KVM_DETERMINISM_RDTSCP)\n"
"\t\tkvm_rcx_write(vcpu, (u32)run->determinism.aux);\n"
"\n"
"\treturn kvm_skip_emulated_instruction(vcpu);\n"
"}\n"
"\n"
"static int complete_deterministic_rng(struct kvm_vcpu *vcpu)\n"
"{\n"
"\tstruct kvm_run *run = vcpu->run;\n"
"\tint reg = run->determinism.dest_reg;\n"
"\tu64 val = run->determinism.value;\n"
"\tunsigned long rflags;\n"
"\n"
"\tswitch (run->determinism.width) {\n"
"\tcase 2:\n"
"\t\tval = (kvm_register_read_raw(vcpu, reg) & ~0xffffULL) |\n"
"\t\t      (val & 0xffffULL);\n"
"\t\tbreak;\n"
"\tcase 4:\n"
"\t\tval = (u32)val;\n"
"\t\tbreak;\n"
"\tdefault:\n"
"\t\tbreak;\n"
"\t}\n"
"\tkvm_register_write_raw(vcpu, reg, val);\n"
"\n"
"\t/*\n"
"\t * RDRAND/RDSEED report success in CF and clear OF/SF/ZF/AF/PF.  The\n"
"\t * spike injects deterministic success (CF=1) via the userspace flag.\n"
"\t */\n"
"\trflags = kvm_get_rflags(vcpu);\n"
"\trflags &= ~(X86_EFLAGS_CF | X86_EFLAGS_PF | X86_EFLAGS_AF |\n"
"\t\t    X86_EFLAGS_ZF | X86_EFLAGS_SF | X86_EFLAGS_OF);\n"
"\tif (run->determinism.flags & KVM_DETERMINISM_FLAG_CF)\n"
"\t\trflags |= X86_EFLAGS_CF;\n"
"\tkvm_set_rflags(vcpu, rflags);\n"
"\n"
"\treturn kvm_skip_emulated_instruction(vcpu);\n"
"}\n"
"\n"
"static int kvm_emulate_deterministic_tsc(struct kvm_vcpu *vcpu, u32 insn)\n"
"{\n"
"\tstruct kvm_run *run = vcpu->run;\n"
"\n"
"\tif (WARN_ON_ONCE(!vcpu->kvm->arch.deterministic_intercepts))\n"
"\t\treturn kvm_skip_emulated_instruction(vcpu);\n"
"\n"
"\tmemset(&run->determinism, 0, sizeof(run->determinism));\n"
"\trun->exit_reason = KVM_EXIT_DETERMINISM;\n"
"\trun->determinism.insn = insn;\n"
"\trun->determinism.width = 8;\n"
"\tvcpu->arch.complete_userspace_io = complete_deterministic_tsc;\n"
"\treturn 0;\n"
"}\n"
"\n"
"int kvm_emulate_rdtsc_intercept(struct kvm_vcpu *vcpu)\n"
"{\n"
"\treturn kvm_emulate_deterministic_tsc(vcpu, KVM_DETERMINISM_RDTSC);\n"
"}\n"
"EXPORT_SYMBOL_FOR_KVM_INTERNAL(kvm_emulate_rdtsc_intercept);\n"
"\n"
"int kvm_emulate_rdtscp_intercept(struct kvm_vcpu *vcpu)\n"
"{\n"
"\treturn kvm_emulate_deterministic_tsc(vcpu, KVM_DETERMINISM_RDTSCP);\n"
"}\n"
"EXPORT_SYMBOL_FOR_KVM_INTERNAL(kvm_emulate_rdtscp_intercept);\n"
"\n"
"/*\n"
" * Surface RDRAND/RDSEED to userspace.  The VMX exit qualification carries no\n"
" * operand info, so decode the destination register and width from the trapped\n"
" * instruction bytes: [legacy prefixes] [REX] 0F C7 /6 (RDRAND) or /7 (RDSEED).\n"
" */\n"
"int kvm_emulate_rng_intercept(struct kvm_vcpu *vcpu, u32 insn,\n"
"\t\t\t      unsigned int insn_len)\n"
"{\n"
"\tstruct kvm_run *run = vcpu->run;\n"
"\tstruct x86_exception e;\n"
"\tu8 buf[15], modrm;\n"
"\tunsigned int i = 0;\n"
"\tint width = 4, reg;\n"
"\tbool rex_b = false;\n"
"\n"
"\t/* Not opted in: preserve stock behavior (#UD for an un-exposed feature). */\n"
"\tif (!vcpu->kvm->arch.deterministic_intercepts)\n"
"\t\treturn kvm_handle_invalid_op(vcpu);\n"
"\n"
"\tif (!insn_len || insn_len > sizeof(buf))\n"
"\t\treturn kvm_handle_invalid_op(vcpu);\n"
"\tif (kvm_read_guest_virt(vcpu, kvm_get_linear_rip(vcpu), buf, insn_len, &e))\n"
"\t\treturn kvm_handle_invalid_op(vcpu);\n"
"\n"
"\twhile (i < insn_len &&\n"
"\t       (buf[i] == 0x66 || buf[i] == 0x67 || buf[i] == 0xf0 ||\n"
"\t\tbuf[i] == 0xf2 || buf[i] == 0xf3 || buf[i] == 0x2e ||\n"
"\t\tbuf[i] == 0x36 || buf[i] == 0x3e || buf[i] == 0x26 ||\n"
"\t\tbuf[i] == 0x64 || buf[i] == 0x65)) {\n"
"\t\tif (buf[i] == 0x66)\n"
"\t\t\twidth = 2;\n"
"\t\ti++;\n"
"\t}\n"
"\tif (i < insn_len && (buf[i] & 0xf0) == 0x40) {\n"
"\t\tif (buf[i] & 0x08)\n"
"\t\t\twidth = 8;\n"
"\t\tif (buf[i] & 0x01)\n"
"\t\t\trex_b = true;\n"
"\t\ti++;\n"
"\t}\n"
"\tif (i + 2 >= insn_len || buf[i] != 0x0f || buf[i + 1] != 0xc7)\n"
"\t\treturn kvm_handle_invalid_op(vcpu);\n"
"\tmodrm = buf[i + 2];\n"
"\treg = (modrm & 0x07) | (rex_b ? 0x08 : 0x00);\n"
"\n"
"\tmemset(&run->determinism, 0, sizeof(run->determinism));\n"
"\trun->exit_reason = KVM_EXIT_DETERMINISM;\n"
"\trun->determinism.insn = insn;\n"
"\trun->determinism.width = width;\n"
"\trun->determinism.dest_reg = reg;\n"
"\tvcpu->arch.complete_userspace_io = complete_deterministic_rng;\n"
"\treturn 0;\n"
"}\n"
"EXPORT_SYMBOL_FOR_KVM_INTERNAL(kvm_emulate_rng_intercept);\n"
"\n"
)
insert_before_line(XC, "static u64 kvm_msr_reason(int r)", EMUL)

insert_before_line(XC, "\tcase KVM_CAP_PRE_FAULT_MEMORY:",
    "\tcase KVM_CAP_X86_DETERMINISTIC_INTERCEPTS:\n"
    "\t\tr = kvm_caps.has_deterministic_intercepts;\n"
    "\t\tbreak;\n")

exact(XC,
    "\tcase KVM_CAP_X86_USER_SPACE_MSR:\n"
    "\t\tr = -EINVAL;\n"
    "\t\tif (cap->args[0] & ~KVM_MSR_EXIT_REASON_VALID_MASK)\n"
    "\t\t\tbreak;\n"
    "\t\tkvm->arch.user_space_msr_mask = cap->args[0];\n"
    "\t\tr = 0;\n"
    "\t\tbreak;\n",
    "\tcase KVM_CAP_X86_USER_SPACE_MSR:\n"
    "\t\tr = -EINVAL;\n"
    "\t\tif (cap->args[0] & ~KVM_MSR_EXIT_REASON_VALID_MASK)\n"
    "\t\t\tbreak;\n"
    "\t\tkvm->arch.user_space_msr_mask = cap->args[0];\n"
    "\t\tr = 0;\n"
    "\t\tbreak;\n"
    "\tcase KVM_CAP_X86_DETERMINISTIC_INTERCEPTS:\n"
    "\t\tr = -EINVAL;\n"
    "\t\tif (!kvm_caps.has_deterministic_intercepts || (cap->args[0] & ~1ULL))\n"
    "\t\t\tbreak;\n"
    "\t\tmutex_lock(&kvm->lock);\n"
    "\t\tif (kvm->created_vcpus) {\n"
    "\t\t\tr = -EINVAL;\n"
    "\t\t} else {\n"
    "\t\t\tkvm->arch.deterministic_intercepts = cap->args[0] & 1;\n"
    "\t\t\tr = 0;\n"
    "\t\t}\n"
    "\t\tmutex_unlock(&kvm->lock);\n"
    "\t\tbreak;\n")

# ------------------------------------------------------- patch 3: VMX intercept
exact(VMX,
    "\tif (kvm_hlt_in_guest(vmx->vcpu.kvm))\n"
    "\t\texec_control &= ~CPU_BASED_HLT_EXITING;\n"
    "\treturn exec_control;\n",
    "\tif (kvm_hlt_in_guest(vmx->vcpu.kvm))\n"
    "\t\texec_control &= ~CPU_BASED_HLT_EXITING;\n"
    "\n"
    "\t/* Deterministic backend: trap RDTSC/RDTSCP to userspace (opt-in). */\n"
    "\tif (vmx->vcpu.kvm->arch.deterministic_intercepts)\n"
    "\t\texec_control |= CPU_BASED_RDTSC_EXITING;\n"
    "\n"
    "\treturn exec_control;\n")

exact(VMX,
    "\tvmx_adjust_sec_exec_exiting(vmx, &exec_control, rdrand, RDRAND);\n"
    "\tvmx_adjust_sec_exec_exiting(vmx, &exec_control, rdseed, RDSEED);\n",
    "\tvmx_adjust_sec_exec_exiting(vmx, &exec_control, rdrand, RDRAND);\n"
    "\tvmx_adjust_sec_exec_exiting(vmx, &exec_control, rdseed, RDSEED);\n"
    "\n"
    "\t/*\n"
    "\t * Deterministic backend: force RDRAND/RDSEED exiting on (opt-in) even\n"
    "\t * though the features are exposed to the guest, so userspace supplies\n"
    "\t * the value from a seeded stream.  Overrides the adjustments above.\n"
    "\t */\n"
    "\tif (vmx->vcpu.kvm->arch.deterministic_intercepts) {\n"
    "\t\tif (cpu_has_vmx_rdrand())\n"
    "\t\t\texec_control |= SECONDARY_EXEC_RDRAND_EXITING;\n"
    "\t\tif (cpu_has_vmx_rdseed())\n"
    "\t\t\texec_control |= SECONDARY_EXEC_RDSEED_EXITING;\n"
    "\t}\n")

RNG_HANDLERS = (
"static int handle_rdrand(struct kvm_vcpu *vcpu)\n"
"{\n"
"\treturn kvm_emulate_rng_intercept(vcpu, KVM_DETERMINISM_RDRAND,\n"
"\t\t\t\t\t vmcs_read32(VM_EXIT_INSTRUCTION_LEN));\n"
"}\n"
"\n"
"static int handle_rdseed(struct kvm_vcpu *vcpu)\n"
"{\n"
"\treturn kvm_emulate_rng_intercept(vcpu, KVM_DETERMINISM_RDSEED,\n"
"\t\t\t\t\t vmcs_read32(VM_EXIT_INSTRUCTION_LEN));\n"
"}\n"
"\n"
)
insert_before_line(VMX, "static int (*kvm_vmx_exit_handlers[])(struct kvm_vcpu *vcpu) = {", RNG_HANDLERS)

exact(VMX,
    "\t[EXIT_REASON_RDRAND]                  = kvm_handle_invalid_op,\n"
    "\t[EXIT_REASON_RDSEED]                  = kvm_handle_invalid_op,\n",
    "\t[EXIT_REASON_RDTSC]                   = kvm_emulate_rdtsc_intercept,\n"
    "\t[EXIT_REASON_RDTSCP]                  = kvm_emulate_rdtscp_intercept,\n"
    "\t[EXIT_REASON_RDRAND]                  = handle_rdrand,\n"
    "\t[EXIT_REASON_RDSEED]                  = handle_rdseed,\n")

exact(VMX,
    "\tkvm_caps.has_bus_lock_exit = cpu_has_vmx_bus_lock_detection();\n"
    "\tkvm_caps.has_notify_vmexit = cpu_has_notify_vmexit();\n",
    "\tkvm_caps.has_bus_lock_exit = cpu_has_vmx_bus_lock_detection();\n"
    "\tkvm_caps.has_notify_vmexit = cpu_has_notify_vmexit();\n"
    "\t/* RDTSC-exiting is architectural on all VMX CPUs; RNG gated at setup. */\n"
    "\tkvm_caps.has_deterministic_intercepts = true;\n")

print("ALL EDITS APPLIED")
