#!/usr/bin/env python3
import pathlib, sys

p = pathlib.Path("/root/spike/spikes/amd-epyc/harness/ae3-forceexit.c")
t = p.read_text()

old_a = """#ifndef KVM_ARM_PREEMPT_EXIT
#define KVM_ARM_PREEMPT_EXIT _IO(KVMIO, 0xe4)
"""
new_a = """#ifndef KVM_ARM_PREEMPT_EXIT
#define KVM_ARM_PREEMPT_EXIT _IO(KVMIO, 0xe4)
#endif
#ifndef KVM_DETERMINISTIC_INTERCEPT_PREEMPT
#define KVM_DETERMINISTIC_INTERCEPT_PREEMPT (1ULL << 2)
"""

old_b = '''    cap.args[0] = 1;                  /* enable (the handler stores args[0]&1) */
    if (ioctl(v->vmfd, KVM_ENABLE_CAP, &cap) < 0) {
'''
new_b = '''    /* The opt-in is a mask of instruction classes. KVM_CHECK_EXTENSION reports the
     * set this host can cover; ask for that set, and stop if the preemption exit the
     * landing mechanism needs is not in it. */
    long supported = ioctl(v->kvm, KVM_CHECK_EXTENSION,
                           KVM_CAP_X86_DETERMINISTIC_INTERCEPTS);
    if (supported <= 0 || !(supported & KVM_DETERMINISTIC_INTERCEPT_PREEMPT)) {
        fprintf(stderr, "deterministic intercepts: host reports 0x%lx, no preemption "
                        "class (stock/unpatched kvm_amd?)\\n", supported > 0 ? supported : 0);
        return -2;
    }
    cap.args[0] = (uint64_t)supported;
    if (ioctl(v->vmfd, KVM_ENABLE_CAP, &cap) < 0) {
'''

for old, new, name in ((old_a, new_a, "A"), (old_b, new_b, "B")):
    if t.count(old) != 1:
        print(f"anchor {name}: found {t.count(old)}, expected 1", file=sys.stderr)
        sys.exit(1)
    t = t.replace(old, new)

p.write_text(t)
print("patched")
