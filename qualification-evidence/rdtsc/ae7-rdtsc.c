/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae7-rdtsc.c - does the deterministic backend get RDTSC and RDTSCP on SVM?
 *
 * Two questions, both answered on silicon rather than from the source:
 *
 *  1. Does the per-class opt-in tell the truth?  KVM_CHECK_EXTENSION must report
 *     the classes this vendor can actually cover, and KVM_ENABLE_CAP must refuse
 *     a request for a class it cannot.  On SVM that means the randomness class is
 *     absent and asking for it fails.
 *
 *  2. Are the two time-stamp instructions trapped?  A guest that executes RDTSC
 *     and RDTSCP must receive the values userspace supplies, not the host's.
 *     The control is the same guest in a VM that did not opt in, which must read
 *     the host counter and so must not see the sentinels.
 *
 * The guest installs a real-mode #UD handler, so an instruction that faulted and
 * an instruction that ran are distinguishable rather than one of them hanging.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <linux/kvm.h>

#define GUEST_PHYS 0x1000
#define UD_HANDLER 0x1500
#define STORE_GPA  0x2000
#define MEM_SIZE   0x10000

#define TSC_SENTINEL   0x1122334455667788ULL
#define TSCP_SENTINEL  0x99aabbccddeeff00ULL
#define AUX_SENTINEL   0x5a5a5a5aU

static void emit(uint8_t *mem)
{
    uint8_t code[] = {
        0xBF,0x00,0x20,        /* mov di,0x2000     */
        0x0F,0x31,             /* rdtsc             */
        0x66,0x89,0x05,        /* mov [di],eax      */
        0x66,0x89,0x55,0x04,   /* mov [di+4],edx    */
        0x0F,0x01,0xF9,        /* rdtscp            */
        0x66,0x89,0x45,0x08,   /* mov [di+8],eax    */
        0x66,0x89,0x55,0x0C,   /* mov [di+12],edx   */
        0x66,0x89,0x4D,0x10,   /* mov [di+16],ecx   */
        0xF4,                  /* hlt               */
    };
    uint8_t ud[] = {
        0xBF,0x00,0x20,                  /* mov di,0x2000            */
        0xC7,0x45,0x14,0xAD,0xDE,        /* mov word [di+20],0xDEAD  */
        0xF4,                            /* hlt                      */
    };
    memcpy(mem + GUEST_PHYS, code, sizeof(code));
    memcpy(mem + UD_HANDLER, ud, sizeof(ud));
    /* real-mode interrupt vector 6 (#UD) -> 0000:1500 */
    uint16_t off = UD_HANDLER, seg = 0;
    memcpy(mem + 6 * 4,     &off, 2);
    memcpy(mem + 6 * 4 + 2, &seg, 2);
}

struct result {
    uint32_t tsc_lo, tsc_hi, tscp_lo, tscp_hi, tscp_ecx;
    uint16_t ud_mark;
    int determinism_exits;
    int hlt;
    int other_exit;
};

/* Run the payload once.  `opt_in` is the class mask to enable, 0 for none. */
static int run_guest(int kvm, uint64_t opt_in, struct result *out, char *err, size_t errlen)
{
    memset(out, 0, sizeof(*out));

    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    if (vmfd < 0) { snprintf(err, errlen, "CREATE_VM: %s", strerror(errno)); return -1; }

    if (opt_in) {
        struct kvm_enable_cap cap = { .cap = KVM_CAP_X86_DETERMINISTIC_INTERCEPTS };
        cap.args[0] = opt_in;
        if (ioctl(vmfd, KVM_ENABLE_CAP, &cap) < 0) {
            snprintf(err, errlen, "ENABLE_CAP(0x%llx): %s",
                     (unsigned long long)opt_in, strerror(errno));
            close(vmfd);
            return -1;
        }
    }

    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0,
        .memory_size = MEM_SIZE, .userspace_addr = (uint64_t)mem };
    if (ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) {
        snprintf(err, errlen, "SET_MEM: %s", strerror(errno)); return -1;
    }

    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    if (vcpu < 0) { snprintf(err, errlen, "CREATE_VCPU: %s", strerror(errno)); return -1; }
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, vcpu, 0);

    emit(mem);
    struct kvm_sregs s;
    ioctl(vcpu, KVM_GET_SREGS, &s);
    s.cs.base = 0; s.cs.selector = 0;
    ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r;
    memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS; r.rflags = 0x2;
    ioctl(vcpu, KVM_SET_REGS, &r);

    for (int guard = 0; guard < 64; guard++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) {
            if (errno == EINTR) continue;
            snprintf(err, errlen, "KVM_RUN: %s", strerror(errno));
            return -1;
        }
        if (run->exit_reason == KVM_EXIT_HLT) { out->hlt = 1; break; }
        if (run->exit_reason == KVM_EXIT_DETERMINISM) {
            out->determinism_exits++;
            if (run->determinism.insn == KVM_DETERMINISM_RDTSC) {
                run->determinism.value = TSC_SENTINEL;
            } else if (run->determinism.insn == KVM_DETERMINISM_RDTSCP) {
                run->determinism.value = TSCP_SENTINEL;
                run->determinism.aux = AUX_SENTINEL;
            } else {
                snprintf(err, errlen, "unexpected determinism insn %u",
                         run->determinism.insn);
                return -1;
            }
            continue;
        }
        out->other_exit = run->exit_reason;
        break;
    }

    memcpy(&out->tsc_lo,   mem + STORE_GPA,      4);
    memcpy(&out->tsc_hi,   mem + STORE_GPA + 4,  4);
    memcpy(&out->tscp_lo,  mem + STORE_GPA + 8,  4);
    memcpy(&out->tscp_hi,  mem + STORE_GPA + 12, 4);
    memcpy(&out->tscp_ecx, mem + STORE_GPA + 16, 4);
    memcpy(&out->ud_mark,  mem + STORE_GPA + 20, 2);

    munmap(run, msize);
    munmap(mem, MEM_SIZE);
    close(vcpu);
    close(vmfd);
    return 0;
}

/* Try to enable `mask` on a fresh VM and report the errno, 0 on success. */
static int try_enable(int kvm, uint64_t mask)
{
    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    if (vmfd < 0) return -errno;
    struct kvm_enable_cap cap = { .cap = KVM_CAP_X86_DETERMINISTIC_INTERCEPTS };
    cap.args[0] = mask;
    int rc = ioctl(vmfd, KVM_ENABLE_CAP, &cap);
    int e = rc < 0 ? errno : 0;
    close(vmfd);
    return e;
}

int main(int argc, char **argv)
{
    const char *out_path = 0;
    for (int i = 1; i < argc; i++)
        if (!strcmp(argv[i], "--out") && i + 1 < argc) out_path = argv[++i];

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }

    long supported = ioctl(kvm, KVM_CHECK_EXTENSION,
                           KVM_CAP_X86_DETERMINISTIC_INTERCEPTS);

    int err_rng   = try_enable(kvm, KVM_DETERMINISTIC_INTERCEPT_RNG);
    int err_all   = try_enable(kvm, KVM_DETERMINISTIC_INTERCEPT_TSC |
                                    KVM_DETERMINISTIC_INTERCEPT_RNG |
                                    KVM_DETERMINISTIC_INTERCEPT_PREEMPT);
    int err_zero  = try_enable(kvm, 0);
    int err_tsc   = try_enable(kvm, KVM_DETERMINISTIC_INTERCEPT_TSC);
    int err_unk   = try_enable(kvm, 1ULL << 40);

    char err[256] = {0};
    struct result opted, control;
    if (run_guest(kvm, KVM_DETERMINISTIC_INTERCEPT_TSC |
                       KVM_DETERMINISTIC_INTERCEPT_PREEMPT,
                  &opted, err, sizeof(err)) < 0) {
        fprintf(stderr, "opted-in run failed: %s\n", err);
        return 2;
    }
    char err2[256] = {0};
    if (run_guest(kvm, 0, &control, err2, sizeof(err2)) < 0) {
        fprintf(stderr, "control run failed: %s\n", err2);
        return 2;
    }

    uint64_t opted_tsc  = ((uint64_t)opted.tsc_hi  << 32) | opted.tsc_lo;
    uint64_t opted_tscp = ((uint64_t)opted.tscp_hi << 32) | opted.tscp_lo;
    uint64_t ctl_tsc    = ((uint64_t)control.tsc_hi  << 32) | control.tsc_lo;
    uint64_t ctl_tscp   = ((uint64_t)control.tscp_hi << 32) | control.tscp_lo;

    int pass_advert   = (supported == (KVM_DETERMINISTIC_INTERCEPT_TSC |
                                       KVM_DETERMINISTIC_INTERCEPT_PREEMPT));
    int pass_rng_ref  = (err_rng == EINVAL) && (err_all == EINVAL) &&
                        (err_unk == EINVAL) && (err_zero == EINVAL);
    int pass_tsc_ok   = (err_tsc == 0);
    int pass_trapped  = (opted.determinism_exits == 2) &&
                        (opted_tsc == TSC_SENTINEL) &&
                        (opted_tscp == TSCP_SENTINEL) &&
                        (opted.tscp_ecx == AUX_SENTINEL) &&
                        (opted.ud_mark == 0) && opted.hlt;
    int pass_control  = (control.determinism_exits == 0) &&
                        (ctl_tsc != TSC_SENTINEL) &&
                        (ctl_tscp != TSCP_SENTINEL) &&
                        (ctl_tsc != 0) && control.hlt;

    int all = pass_advert && pass_rng_ref && pass_tsc_ok && pass_trapped && pass_control;

    char buf[2048];
    int n = snprintf(buf, sizeof(buf),
        "{\n"
        "  \"supported_mask\": %ld,\n"
        "  \"enable_errno\": { \"rng\": %d, \"all_three\": %d, \"zero\": %d,"
                             " \"tsc\": %d, \"unknown_bit\": %d },\n"
        "  \"opted_in\": { \"determinism_exits\": %d, \"rdtsc\": \"0x%016llx\","
                        " \"rdtscp\": \"0x%016llx\", \"rdtscp_ecx\": \"0x%08x\","
                        " \"ud\": %u, \"hlt\": %d, \"other_exit\": %d },\n"
        "  \"control\": { \"determinism_exits\": %d, \"rdtsc\": \"0x%016llx\","
                       " \"rdtscp\": \"0x%016llx\", \"rdtscp_ecx\": \"0x%08x\","
                       " \"ud\": %u, \"hlt\": %d, \"other_exit\": %d },\n"
        "  \"checks\": { \"advertises_tsc_and_preempt_only\": %d,"
                      " \"refuses_rng_and_unknown\": %d, \"accepts_tsc\": %d,\n"
        "                \"tsc_and_tscp_come_from_userspace\": %d,"
                      " \"control_reads_host_counter\": %d },\n"
        "  \"pass\": %d\n"
        "}\n",
        supported,
        err_rng, err_all, err_zero, err_tsc, err_unk,
        opted.determinism_exits, (unsigned long long)opted_tsc,
        (unsigned long long)opted_tscp, opted.tscp_ecx,
        opted.ud_mark, opted.hlt, opted.other_exit,
        control.determinism_exits, (unsigned long long)ctl_tsc,
        (unsigned long long)ctl_tscp, control.tscp_ecx,
        control.ud_mark, control.hlt, control.other_exit,
        pass_advert, pass_rng_ref, pass_tsc_ok, pass_trapped, pass_control,
        all);

    fwrite(buf, 1, n, stdout);
    if (out_path) {
        FILE *f = fopen(out_path, "w");
        if (f) { fwrite(buf, 1, n, f); fclose(f); }
    }
    return all ? 0 : 1;
}
