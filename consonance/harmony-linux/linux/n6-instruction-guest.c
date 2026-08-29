// SPDX-License-Identifier: AGPL-3.0-or-later
/* Table-generated N6 hostile instruction sweep, run as the guest's PID 1. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__x86_64__)
#include <cpuid.h>
#endif

#ifndef N6_ARCH
#error "N6_ARCH must be an architecture JSON string"
#endif
#ifndef N6_AUDIT_REJECTED
#error "the build must run the planted entropy-opcode audit before compiling"
#endif

struct n6_operation {
	const char *name;
	const unsigned char *start;
	const unsigned char *end;
};

struct n6_row {
	const char *identifier;
	const char *claim;
	size_t first;
	size_t count;
};

#include "n6-generated.h"

#define PAGE_BYTES 4096
#define PR_SET_TSC 26
#define PR_TSC_SIGSEGV 2

struct shared_result {
	uint64_t value;
	unsigned char data[PAGE_BYTES] __attribute__((aligned(64)));
};

static int output_fd = 1;

static void write_marker(const char *text)
{
	if (dprintf(output_fd, "%s\n", text) >= 0)
		return;
	if (output_fd == 1) {
		output_fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
		if (output_fd >= 0)
			(void)dprintf(output_fd, "%s\n", text);
	}
}

static __attribute__((noreturn)) void fail(const char *reason)
{
	char line[256];
	(void)snprintf(line, sizeof(line), "N6_GUEST_FAIL %s errno=%d", reason, errno);
	write_marker(line);
	for (;;)
		pause();
}

static uint64_t fnv1a(const unsigned char *data, size_t length)
{
	uint64_t hash = UINT64_C(1469598103934665603);
	size_t index;

	for (index = 0; index < length; index++) {
		hash ^= data[index];
		hash *= UINT64_C(1099511628211);
	}
	return hash;
}

static void run_operation(const struct n6_operation *operation, char result[64])
{
	struct shared_result *shared;
	unsigned char *jit;
	size_t length;
	pid_t child;
	int status;

	if (operation->start == NULL || operation->end == NULL)
		fail("execute-operation-has-no-code");
	length = (size_t)(operation->end - operation->start);
	if (length == 0 || length > PAGE_BYTES)
		fail("invalid-jit-length");
	shared = mmap(NULL, sizeof(*shared), PROT_READ | PROT_WRITE,
			MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	jit = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE,
		   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (shared == MAP_FAILED || jit == MAP_FAILED)
		fail("mmap");
	memset(shared, 0, sizeof(*shared));
	memcpy(jit, operation->start, length);
	__builtin___clear_cache((char *)jit, (char *)jit + length);
	if (mprotect(jit, PAGE_BYTES, PROT_READ | PROT_EXEC) != 0)
		fail("mprotect-rx");

	child = fork();
	if (child < 0)
		fail("fork");
	if (child == 0) {
		uint64_t (*function)(void *) = (uint64_t (*)(void *))(void *)jit;
		shared->value = function(shared->data);
		_exit(0);
	}
	if (waitpid(child, &status, 0) != child)
		fail("waitpid");
	if (WIFSIGNALED(status)) {
		(void)snprintf(result, 64, "signal:%d", WTERMSIG(status));
	} else if (WIFEXITED(status) && WEXITSTATUS(status) == 0) {
		(void)snprintf(result, 64, "value:%016llx:mem:%016llx",
			       (unsigned long long)shared->value,
			       (unsigned long long)fnv1a(shared->data, PAGE_BYTES));
	} else {
		(void)snprintf(result, 64, "exit:%d", WEXITSTATUS(status));
	}
	(void)munmap(jit, PAGE_BYTES);
	(void)munmap(shared, sizeof(*shared));
}

static int entropy_hidden(void)
{
#if defined(__aarch64__)
	/* Linux uapi: HWCAP2_RNG is bit 16. */
	return (getauxval(AT_HWCAP2) & (1UL << 16)) == 0;
#elif defined(__x86_64__)
	unsigned int eax, ebx, ecx, edx;
	int rdrand_hidden;
	int rdseed_hidden;

	if (!__get_cpuid(1, &eax, &ebx, &ecx, &edx))
		return 0;
	rdrand_hidden = (ecx & (1U << 30)) == 0;
	if (!__get_cpuid_count(7, 0, &eax, &ebx, &ecx, &edx))
		return 0;
	rdseed_hidden = (ebx & (1U << 18)) == 0;
	return rdrand_hidden && rdseed_hidden;
#else
#error "unsupported N6 guest architecture"
#endif
}

static int trap_policy_on(void)
{
#ifdef N6_TRAPS_OFF
	return 0;
#elif defined(__x86_64__)
	/* Linux confines this hostile, unaudited process with CR4.TSD. */
	if (prctl(PR_SET_TSC, PR_TSC_SIGSEGV, 0, 0, 0) != 0)
		fail("prctl-PR_SET_TSC");
	return 1;
#else
	/* CONFIG_HARMONY_ARM_PVCLOCK globally clears CNTKCTL_EL1 EL0 access. */
	return 1;
#endif
}

int main(void)
{
	const int traps_on = trap_policy_on();
	const int hidden = entropy_hidden();
	size_t row_index;
	char line[32768];

	if (N6_TABLE_ROW_COUNT == 0 || N6_TABLE_OPERATION_COUNT == 0)
		fail("empty-generated-table");
	write_marker("N6_GUEST_BEGIN arch=" N6_ARCH);
	for (row_index = 0; row_index < N6_TABLE_ROW_COUNT; row_index++) {
		const struct n6_row *row = &n6_rows[row_index];
		size_t used;
		size_t operation_index;

		used = (size_t)snprintf(line, sizeof(line),
			"N6_ROW {\"arch\":\"%s\",\"id\":\"%s\",\"claim\":\"%s\","
			"\"operation_count\":%zu", N6_ARCH, row->identifier,
			row->claim, row->count);
		if (strcmp(row->claim, "mask-and-audit") == 0) {
			used += (size_t)snprintf(line + used, sizeof(line) - used,
				",\"feature_hidden\":%s,\"audit_rejected\":%s}",
				hidden ? "true" : "false",
				N6_AUDIT_REJECTED ? "true" : "false");
		} else {
			used += (size_t)snprintf(line + used, sizeof(line) - used,
				",\"results\":[");
			for (operation_index = 0; operation_index < row->count;
			     operation_index++) {
				char result[64];
				const struct n6_operation *operation =
					&n6_operations[row->first + operation_index];

				run_operation(operation, result);
				used += (size_t)snprintf(line + used, sizeof(line) - used,
					"%s\"%s\"", operation_index == 0 ? "" : ",", result);
			}
			used += (size_t)snprintf(line + used, sizeof(line) - used,
				"],\"traps_on\":%s}", traps_on ? "true" : "false");
		}
		if (used >= sizeof(line))
			fail("report-line-overflow");
		write_marker(line);
	}
	(void)snprintf(line, sizeof(line),
		"N6_GUEST_OK arch=%s table_rows=%d exercised_rows=%zu operations=%d",
		N6_ARCH, N6_TABLE_ROW_COUNT, row_index, N6_TABLE_OPERATION_COUNT);
	write_marker(line);
	for (;;)
		pause();
}
