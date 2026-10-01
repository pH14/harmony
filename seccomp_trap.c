// SPDX-License-Identifier: AGPL-3.0-or-later
#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/personality.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#if defined(__x86_64__)
#define ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define ARCH AUDIT_ARCH_AARCH64
#endif

static volatile int sigsys_count, sigsys_nr = -1;

static void on_sigsys(int sig, siginfo_t *si, void *ctx)
{
	ucontext_t *uc = ctx;

	(void)sig;
	sigsys_count++;
	sigsys_nr = si->si_syscall;
#if defined(__x86_64__)
	uc->uc_mcontext.gregs[REG_RAX] = 4242;
#elif defined(__aarch64__)
	uc->uc_mcontext.regs[0] = 4242;
#endif
}

static int child_fn(void *arg)
{
	*(volatile int *)arg = 77;
	return 0;
}

static const char *es(int e)
{
	return e ? strerror(e) : "";
}

int main(void)
{
	struct sigaction sa = { .sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO };
	struct sock_filter f[] = {
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, ARCH, 1, 0),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_getppid, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_TRAP),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
	};
	struct sock_fprog prog = { .len = sizeof(f) / sizeof(f[0]), .filter = f };

	printf("{\"uid\":%d,", getuid());

	int pers = personality(ADDR_NO_RANDOMIZE), pers_errno = pers == -1 ? errno : 0;
	int pers_now = personality(0xffffffff);
	printf("\"personality_ret\":%d,\"personality_errno\":\"%s\",\"personality_now\":\"%#x\",", pers, es(pers_errno), pers_now);

	int mfd = memfd_create("probe", 0), mfd_errno = mfd < 0 ? errno : 0;
	int shared_c2p = -1, shared_p2c = -1, map_errno = 0;
	void *want = (void *)0x500000000000UL;
	if (mfd >= 0 && ftruncate(mfd, 4096) == 0) {
		volatile int *p = mmap(want, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, mfd, 0);
		if (p == MAP_FAILED) {
			map_errno = errno;
		} else {
			p[0] = 0;
			p[1] = 0;
			pid_t c = fork();
			if (!c) {
				munmap((void *)p, 4096);
				volatile int *q = mmap(want, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED, mfd, 0);
				if (q == MAP_FAILED)
					_exit(2);
				q[0] = 1234;
				for (int i = 0; i < 2000000 && q[1] != 5678; i++)
					usleep(1);
				_exit(q[1] == 5678 ? 0 : 1);
			}
			for (int i = 0; i < 2000000 && p[0] != 1234; i++)
				usleep(1);
			shared_c2p = p[0] == 1234;
			p[1] = 5678;
			int st;
			waitpid(c, &st, 0);
			shared_p2c = WIFEXITED(st) && WEXITSTATUS(st) == 0;
		}
	}
	printf("\"memfd_create\":%d,\"memfd_errno\":\"%s\",\"mmap_fixed_errno\":\"%s\",\"shared_child_to_parent\":%d,\"shared_parent_to_child\":%d,",
	       mfd >= 0, es(mfd_errno), es(map_errno), shared_c2p, shared_p2c);

	size_t stsz = 1 << 16;
	char *stack = mmap(NULL, stsz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	static volatile int clone_flag;
	clone_flag = 0;
	int cpid = clone(child_fn, stack + stsz, CLONE_VM | CLONE_FILES | SIGCHLD, (void *)&clone_flag);
	int clone_errno = cpid < 0 ? errno : 0, cst = -1;
	if (cpid > 0)
		waitpid(cpid, &cst, 0);
	printf("\"clone_vm_files\":%d,\"clone_errno\":\"%s\",\"clone_child_ran\":%d,", cpid > 0, es(clone_errno), clone_flag == 77);

	sigaction(SIGSYS, &sa, NULL);
	int nnp = prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0), nnp_errno = nnp ? errno : 0;
	long sc = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog), sc_errno = sc ? errno : 0;
	printf("\"no_new_privs\":%d,\"no_new_privs_errno\":\"%s\",\"seccomp_filter\":%ld,\"seccomp_errno\":\"%s\",",
	       nnp, es(nnp_errno), sc, es((int)sc_errno));
	long r = -1;
	int child_sigsys = -1;
	if (sc == 0) {
		r = syscall(SYS_getppid);
		pid_t c = fork();
		if (!c) {
			sigsys_count = 0;
			long cr = syscall(SYS_getppid);
			_exit(sigsys_count == 1 && sigsys_nr == __NR_getppid && cr == 4242 ? 0 : 1);
		}
		int st;
		waitpid(c, &st, 0);
		child_sigsys = WIFEXITED(st) && WEXITSTATUS(st) == 0;
	}
	printf("\"sigsys_arrived\":%d,\"si_syscall\":%d,\"si_syscall_matches\":%d,\"getppid_return\":%ld,\"child_inherits_filter\":%d}\n",
	       sigsys_count > 0, sigsys_nr, sigsys_nr == __NR_getppid, r, child_sigsys);
	return 0;
}
