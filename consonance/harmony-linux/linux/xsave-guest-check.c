// SPDX-License-Identifier: AGPL-3.0-or-later
#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>
#include <elf.h>

struct captures {
    _Alignas(64) unsigned char before[512];
    _Alignas(64) unsigned char after[512];
};
static int payload_equal(const unsigned char *a, const unsigned char *b)
{
    for (unsigned i = 0; i < 8; i++)
        if (memcmp(a + 32 + 16 * i, b + 32 + 16 * i, 10)) return 0;
    return 1;
}
static int payload_nonzero(const unsigned char *a)
{
    for (unsigned i = 0; i < 8; i++)
        for (unsigned j = 0; j < 10; j++)
            if (a[32 + 16 * i + j]) return 1;
    return 0;
}
static volatile sig_atomic_t signal_ok;
static void handler(int signo, siginfo_t *info, void *context)
{
    (void)signo;
    (void)info;
    ucontext_t *uc = context;
    unsigned char *fp = (void *)uc->uc_mcontext.fpregs;
    uint32_t mxcsr;
    uint64_t bv;
    memcpy(&mxcsr, fp + 24, 4);
    memcpy(&bv, fp + 512, 8);
    signal_ok = mxcsr == 0x3f80 && (bv & 3) == 3;
}

static void run_case(unsigned mode)
{
    struct captures *shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
                                  MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    assert(shared != MAP_FAILED);
    memset(shared, 0, sizeof(*shared));
    pid_t child = fork();
    assert(child >= 0);
    if (!child) {
        struct sigaction sa = {.sa_sigaction = handler, .sa_flags = SA_SIGINFO};
        assert(sigaction(SIGUSR1, &sa, NULL) == 0);
        assert(ptrace(PTRACE_TRACEME, 0, NULL, NULL) == 0);
        asm volatile("fninit");
        if (mode) asm volatile("fld1");
        if (mode == 2) asm volatile("fninit");
        asm volatile("fxsave64 %0" : "=m" (shared->before));
        raise(SIGSTOP);
        asm volatile("fxsave64 %0" : "=m" (shared->after));
        if (!mode) {
            uint32_t mxcsr;
            asm volatile("stmxcsr %0" : "=m" (mxcsr));
            assert(mxcsr == 0x3f80);
            raise(SIGUSR1);
            asm volatile("stmxcsr %0" : "=m" (mxcsr));
            assert(signal_ok && mxcsr == 0x3f80);
        }
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child && WIFSTOPPED(status));
    _Alignas(64) unsigned char image[832];
    memset(image, 0xa5, sizeof(image));
    struct iovec io = {.iov_base = image, .iov_len = sizeof(image)};
    assert(ptrace(PTRACE_GETREGSET, child, NT_X86_XSTATE, &io) == 0);
    assert(io.iov_len == sizeof(image));
    uint64_t bv;
    memcpy(&bv, image + 512, 8);
    if (mode) {
        assert(payload_nonzero(shared->before));
        int equal = payload_equal(image, shared->before);
        printf("G1_X87 mode=%u saved_bv=%llu payload_equal=%d\n", mode,
               (unsigned long long)bv, equal);
        fflush(stdout);
        assert(equal);
    } else {
        uint32_t mxcsr = 0x3f80;
        memset(image + 512, 0, 64);
        memcpy(image + 24, &mxcsr, 4);
    }
    assert(ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &io) == 0);
    if (!mode) {
        assert(ptrace(PTRACE_GETREGSET, child, NT_X86_XSTATE, &io) == 0);
        uint32_t mxcsr;
        memcpy(&mxcsr, image + 24, 4);
        assert(mxcsr == 0x3f80);
        mxcsr = 0x80001f80;
        memcpy(image + 24, &mxcsr, 4);
        errno = 0;
        assert(ptrace(PTRACE_SETREGSET, child, NT_X86_XSTATE, &io) == -1);
        assert(errno == EINVAL);
    }
    assert(ptrace(PTRACE_CONT, child, NULL, NULL) == 0);
    while (waitpid(child, &status, 0) == child && WIFSTOPPED(status)) {
        int sig = WSTOPSIG(status);
        assert(sig == SIGUSR1);
        assert(ptrace(PTRACE_CONT, child, NULL, (void *)(intptr_t)sig) == 0);
    }
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    if (mode) assert(payload_equal(shared->before, shared->after));
    munmap(shared, sizeof(*shared));
    printf("G1_GUEST mode=%u roundtrip=true\n", mode);
}
static unsigned char *fault_fp;
static struct captures *fault_capture;
static volatile sig_atomic_t *fault_handler_seen;
static const unsigned char fault_vector[32] = {
    1, 3, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31,
    2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32
};
static void fault_handler(int signo, siginfo_t *info, void *context)
{
    (void)signo;
    (void)info;
    *fault_handler_seen = 1;
    ucontext_t *uc = context;
    unsigned char *fp = (void *)uc->uc_mcontext.fpregs;
    uint32_t mxcsr;
    memcpy(&mxcsr, fp + 24, 4);
    assert(fp == fault_fp);
    assert(mxcsr == 0x3f80);
    assert(payload_equal(fp, fault_capture->before));
    assert(memcmp(fp + 160, fault_vector, 16) == 0);
    assert(memcmp(fp + 576, fault_vector + 16, 16) == 0);
}
static void run_signal_fault_case(int protected)
{
    fault_handler_seen = mmap(NULL, sizeof(*fault_handler_seen), PROT_READ | PROT_WRITE,
                              MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    assert(fault_handler_seen != MAP_FAILED);
    *fault_handler_seen = 0;
    pid_t child = fork();
    assert(child >= 0);
    if (!child) {
        struct rlimit core_limit = {0, 0};
        assert(setrlimit(RLIMIT_CORE, &core_limit) == 0);
        long page = sysconf(_SC_PAGESIZE);
        assert(page == 4096);
        size_t length = 16 * (size_t)page;
        unsigned char *stack = mmap(NULL, length, PROT_READ | PROT_WRITE,
                                   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        assert(stack != MAP_FAILED);
        unsigned char *boundary = stack + length - page;
        fault_fp = boundary - 512;
        struct captures capture;
        memset(&capture, 0, sizeof(capture));
        fault_capture = &capture;
        stack_t alternate = {.ss_sp = stack, .ss_size = length - page + 324};
        assert(sigaltstack(&alternate, NULL) == 0);
        struct sigaction sa = {.sa_sigaction = fault_handler,
                               .sa_flags = SA_SIGINFO | SA_ONSTACK};
        assert(sigaction(SIGUSR2, &sa, NULL) == 0);
        if (protected)
            assert(mprotect(boundary - page, (size_t)page, PROT_NONE) == 0);
        unsigned char resident[2] = {0xff, 0xff};
        assert(mincore(boundary - page, 2 * (size_t)page, resident) == 0);
        assert(!(resident[0] & 1) && !(resident[1] & 1));
        long pid = getpid(), tid = syscall(SYS_gettid), result;
        uint32_t mxcsr = 0x3f80, restored_mxcsr;
        unsigned char restored_vector[32];
        asm volatile("fninit\n\tfld1\n\tldmxcsr %[mx]\n\t"
                     "vmovdqu %[vector], %%ymm0\n\tfxsave64 %[before]\n\t"
                     "syscall\n\tfxsave64 %[after]\n\t"
                     "stmxcsr %[restored_mx]\n\tvmovdqu %%ymm0, %[restored_vector]"
                     : "=a"(result), [before] "=m"(capture.before),
                       [after] "=m"(capture.after), [restored_mx] "=m"(restored_mxcsr),
                       [restored_vector] "=m"(restored_vector)
                     : "0"((long)SYS_tgkill), "D"(pid), "S"(tid), "d"((long)SIGUSR2),
                       [mx] "m"(mxcsr), [vector] "m"(fault_vector)
                     : "rcx", "r11", "xmm0", "memory", "cc");
        assert(!protected && result == 0 && *fault_handler_seen);
        assert(restored_mxcsr == mxcsr);
        assert(payload_equal(capture.before, capture.after));
        assert(memcmp(restored_vector, fault_vector, 32) == 0);
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    if (protected) {
        assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);
        assert(!*fault_handler_seen);
    }
    else
        assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(munmap((void *)fault_handler_seen, sizeof(*fault_handler_seen)) == 0);
    printf("G1_SIGNAL_FAULT protected=%d expected_outcome=true\n", protected);
}
int main(int argc, char **argv)
{
    if (argc == 2) {
        unsigned mode = (unsigned)strtoul(argv[1], NULL, 10);
        assert(mode <= 4);
        if (mode < 3) run_case(mode);
        else run_signal_fault_case(mode == 4);
    } else {
        run_case(0);
        run_case(1);
        run_case(2);
        run_signal_fault_case(0);
        run_signal_fault_case(1);
    }
    puts("G1_GUEST_OK");
    return 0;
}
