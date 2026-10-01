// SPDX-License-Identifier: AGPL-3.0-or-later
#define _GNU_SOURCE
#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static sigjmp_buf jump;
static volatile int caught;

static void handler(int sig)
{
	caught = sig;
	siglongjmp(jump, 1);
}

static void status_line(const char *key)
{
	char line[256];
	FILE *f = fopen("/proc/self/status", "r");

	while (f && fgets(line, sizeof(line), f))
		if (!strncmp(line, key, strlen(key)) && line[strlen(key)] == ':') {
			line[strcspn(line, "\n")] = 0;
			printf("\"%s\":\"%s\",", key, line + strlen(key) + 2);
		}
	if (f)
		fclose(f);
}

static int __attribute__((unused)) cpuinfo_has(const char *flag)
{
	char line[8192];
	FILE *f = fopen("/proc/cpuinfo", "r");
	int found = 0;

	while (f && fgets(line, sizeof(line), f)) {
		if (strncmp(line, "flags", 5) && strncmp(line, "Features", 8))
			continue;
		char *p = strchr(line, ':');
		for (char *t = strtok(p ? p + 1 : line, " \n"); t; t = strtok(NULL, " \n"))
			if (!strcmp(t, flag))
				found = 1;
		break;
	}
	if (f)
		fclose(f);
	return found;
}

static void cpuinfo_field(const char *key)
{
	char line[512];
	FILE *f = fopen("/proc/cpuinfo", "r");

	while (f && fgets(line, sizeof(line), f))
		if (!strncmp(line, key, strlen(key))) {
			char *p = strchr(line, ':');
			line[strcspn(line, "\n")] = 0;
			printf("\"%s\":\"%s\",", key, p ? p + 2 : "");
			break;
		}
	if (f)
		fclose(f);
}

#if defined(__x86_64__)
#include <asm/prctl.h>
#include <cpuid.h>

static int trapped(void (*op)(void))
{
	struct sigaction sa = { .sa_handler = handler };

	sigaction(SIGSEGV, &sa, NULL);
	sigaction(SIGILL, &sa, NULL);
	caught = 0;
	if (!sigsetjmp(jump, 1))
		op();
	return caught;
}

static void do_cpuid(void)
{
	unsigned a, b, c, d;

	__cpuid(0, a, b, c, d);
}

static void do_rdtsc(void)
{
	unsigned lo, hi;

	__asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
}

static void do_rdrand(void)
{
	unsigned long v;
	unsigned char ok;

	__asm__ volatile("rdrand %0; setc %1" : "=r"(v), "=qm"(ok));
}

static void do_rdseed(void)
{
	unsigned long v;
	unsigned char ok;

	__asm__ volatile("rdseed %0; setc %1" : "=r"(v), "=qm"(ok));
}

int main(int argc, char **argv)
{
	unsigned a, b, c, d, rdrand, rdseed, hyper;
	char vendor[13] = { 0 }, hvendor[13] = { 0 };
	long set, get;
	int cpuid_sig = -1, after_fork = -1, after_exec = -1, tsc_sig = -1;

	if (argc > 1 && !strcmp(argv[1], "--get")) {
		printf("%ld\n", syscall(SYS_arch_prctl, ARCH_GET_CPUID, 0));
		return 0;
	}
	__cpuid(0, a, b, c, d);
	memcpy(vendor, &b, 4);
	memcpy(vendor + 4, &d, 4);
	memcpy(vendor + 8, &c, 4);
	__cpuid(1, a, b, c, d);
	rdrand = c >> 30 & 1;
	hyper = c >> 31 & 1;
	unsigned family = (a >> 8 & 15) + (a >> 20 & 255), model = (a >> 4 & 15) | (a >> 12 & 0xf0);
	__cpuid_count(7, 0, a, b, c, d);
	rdseed = b >> 18 & 1;
	if (hyper) {
		__cpuid(0x40000000, a, b, c, d);
		memcpy(hvendor, &b, 4);
		memcpy(hvendor + 4, &c, 4);
		memcpy(hvendor + 8, &d, 4);
	}

	printf("{\"arch\":\"x86_64\",\"uid\":%d,", getuid());
	status_line("CapEff");
	status_line("NoNewPrivs");
	status_line("Seccomp");
	cpuinfo_field("model name");
	printf("\"vendor\":\"%s\",\"family\":%u,\"model\":%u,\"hypervisor\":\"%s\",", vendor, family, model, hvendor);
	printf("\"cpuinfo_cpuid_fault\":%d,\"rdrand\":%u,\"rdseed\":%u,", cpuinfo_has("cpuid_fault"), rdrand, rdseed);

	get = syscall(SYS_arch_prctl, ARCH_GET_CPUID, 0);
	set = syscall(SYS_arch_prctl, ARCH_SET_CPUID, 0);
	int set_errno = set ? errno : 0;
	if (set == 0) {
		cpuid_sig = trapped(do_cpuid);
		pid_t child = fork();
		if (!child)
			_exit(trapped(do_cpuid));
		int st;
		waitpid(child, &st, 0);
		after_fork = WEXITSTATUS(st);
		int pipefd[2];
		pipe(pipefd);
		child = fork();
		if (!child) {
			dup2(pipefd[1], 1);
			execl("/proc/self/exe", argv[0], "--get", (char *)NULL);
			_exit(99);
		}
		close(pipefd[1]);
		char buf[32] = { 0 };
		read(pipefd[0], buf, sizeof(buf) - 1);
		waitpid(child, &st, 0);
		after_exec = atoi(buf);
		syscall(SYS_arch_prctl, ARCH_SET_CPUID, 1);
	}
	printf("\"arch_get_cpuid\":%ld,\"arch_set_cpuid\":%ld,\"arch_set_cpuid_errno\":\"%s\",", get, set, set ? strerror(set_errno) : "");
	printf("\"cpuid_trap_signal\":%d,\"cpuid_trap_after_fork\":%d,\"cpuid_enabled_after_exec\":%d,", cpuid_sig, after_fork, after_exec);
	if (prctl(PR_SET_TSC, PR_TSC_SIGSEGV) == 0) {
		tsc_sig = trapped(do_rdtsc);
		prctl(PR_SET_TSC, PR_TSC_ENABLE);
	}
	printf("\"rdtsc_trap_signal\":%d,", tsc_sig);
	printf("\"rdrand_runs\":%d,\"rdseed_runs\":%d}\n",
	       rdrand ? trapped(do_rdrand) == 0 : -1, rdseed ? trapped(do_rdseed) == 0 : -1);
	return 0;
}

#elif defined(__aarch64__)
#ifndef HWCAP2_RNG
#define HWCAP2_RNG (1 << 16)
#endif
#ifndef HWCAP_CPUID
#define HWCAP_CPUID (1 << 11)
#endif

int main(void)
{
	unsigned long hwcap = getauxval(AT_HWCAP), hwcap2 = getauxval(AT_HWCAP2);
	unsigned long midr = 0, isar0 = 0, cnt = 0, cntkctl_ok;
	struct sigaction sa = { .sa_handler = handler };

	printf("{\"arch\":\"aarch64\",\"uid\":%d,", getuid());
	status_line("CapEff");
	status_line("NoNewPrivs");
	status_line("Seccomp");
	cpuinfo_field("CPU implementer");
	cpuinfo_field("CPU part");
	sigaction(SIGILL, &sa, NULL);
	if (hwcap & HWCAP_CPUID) {
		__asm__ volatile("mrs %0, midr_el1" : "=r"(midr));
		__asm__ volatile("mrs %0, id_aa64isar0_el1" : "=r"(isar0));
	}
	sigaction(SIGSEGV, &sa, NULL);
	caught = 0;
	if (!sigsetjmp(jump, 1))
		__asm__ volatile("mrs %0, cntvct_el0" : "=r"(cnt));
	cntkctl_ok = caught == 0;
	printf("\"hwcap_cpuid\":%d,\"midr\":\"%#lx\",\"isar0_rndr_field\":%lu,\"hwcap2_rng\":%d,",
	       !!(hwcap & HWCAP_CPUID), midr, isar0 >> 60 & 15, !!(hwcap2 & HWCAP2_RNG));
	int set = prctl(PR_SET_TSC, PR_TSC_SIGSEGV), set_errno = set ? errno : 0, trap = -1, fork_trap = -1;
	if (set == 0) {
		caught = 0;
		if (!sigsetjmp(jump, 1))
			__asm__ volatile("mrs %0, cntvct_el0" : "=r"(cnt));
		trap = caught;
		pid_t child = fork();
		if (!child) {
			caught = 0;
			if (!sigsetjmp(jump, 1))
				__asm__ volatile("mrs %0, cntvct_el0" : "=r"(cnt));
			_exit(caught);
		}
		int st;
		waitpid(child, &st, 0);
		fork_trap = WEXITSTATUS(st);
		prctl(PR_SET_TSC, PR_TSC_ENABLE);
	}
	printf("\"cntvct_readable\":%lu,\"pr_set_tsc\":%d,\"pr_set_tsc_errno\":\"%s\",\"cntvct_trap_signal\":%d,\"cntvct_trap_after_fork\":%d}\n",
	       cntkctl_ok, set, set ? strerror(set_errno) : "", trap, fork_trap);
	return 0;
}
#endif
