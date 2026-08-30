// SPDX-License-Identifier: AGPL-3.0-or-later
/* Table-generated N6 hostile instruction sweep, run as the guest's PID 1. */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
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
	int signal_only;
};

#include "n6-generated.h"

#define PAGE_BYTES 4096
struct shared_result {
	uint64_t value;
	unsigned char data[PAGE_BYTES] __attribute__((aligned(64)));
};

static int output_fd = 1;
static sigjmp_buf operation_jmp;
static volatile sig_atomic_t operation_signal;

struct operation_runner {
	struct shared_result *shared;
	unsigned char *jit;
	size_t jit_bytes;
};

static void operation_fault(int signo)
{
	operation_signal = signo;
	siglongjmp(operation_jmp, 1);
}

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

static uint64_t fnv1a_update(uint64_t hash, const unsigned char *data,
			     size_t length)
{
	size_t index;

	for (index = 0; index < length; index++) {
		hash ^= data[index];
		hash *= UINT64_C(1099511628211);
	}
	return hash;
}

static uint64_t fnv1a(const unsigned char *data, size_t length)
{
	return fnv1a_update(UINT64_C(1469598103934665603), data, length);
}

static uint64_t digest_text(uint64_t hash, const char *text)
{
	hash = fnv1a_update(hash, (const unsigned char *)text, strlen(text));
	return fnv1a_update(hash, (const unsigned char *)"", 1);
}

static void setup_operation_runner(struct operation_runner *runner)
{
	struct sigaction action;
	size_t operation_index;

	if ((size_t)N6_TABLE_OPERATION_COUNT > SIZE_MAX / PAGE_BYTES)
		fail("jit-size-overflow");
	runner->jit_bytes = (size_t)N6_TABLE_OPERATION_COUNT * PAGE_BYTES;

	runner->shared = mmap(NULL, sizeof(*runner->shared), PROT_READ | PROT_WRITE,
			MAP_SHARED | MAP_ANONYMOUS, -1, 0);
	runner->jit = mmap(NULL, runner->jit_bytes, PROT_READ | PROT_WRITE,
			   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (runner->shared == MAP_FAILED || runner->jit == MAP_FAILED)
		fail("mmap");
	for (operation_index = 0; operation_index < N6_TABLE_OPERATION_COUNT;
	     operation_index++) {
		const struct n6_operation *operation =
			&n6_operations[operation_index];
		unsigned char *page = runner->jit + operation_index * PAGE_BYTES;
		size_t length;

		if (operation->start == NULL && operation->end == NULL)
			continue;
		if (operation->start == NULL || operation->end == NULL)
			fail("execute-operation-has-no-code");
		length = (size_t)(operation->end - operation->start);
		if (length == 0 || length > PAGE_BYTES)
			fail("invalid-jit-length");
		memcpy(page, operation->start, length);
		__builtin___clear_cache((char *)page, (char *)page + length);
	}
	if (mprotect(runner->jit, runner->jit_bytes, PROT_READ | PROT_EXEC) != 0)
		fail("mprotect-rx");

	memset(&action, 0, sizeof(action));
	action.sa_handler = operation_fault;
	if (sigemptyset(&action.sa_mask) != 0 ||
	    sigaction(SIGILL, &action, NULL) != 0 ||
	    sigaction(SIGSEGV, &action, NULL) != 0 ||
	    sigaction(SIGTRAP, &action, NULL) != 0)
		fail("sigaction-install");
}

static void run_operation(struct operation_runner *runner,
			  size_t operation_index, char result[64])
{
	const struct n6_operation *operation = &n6_operations[operation_index];

	if (operation->start == NULL || operation->end == NULL)
		fail("execute-operation-has-no-code");
	memset(runner->shared, 0, sizeof(*runner->shared));
	operation_signal = 0;
	if (sigsetjmp(operation_jmp, 1) == 0) {
		uint64_t (*function)(void *) =
			(uint64_t (*)(void *))(void *)(runner->jit +
				operation_index * PAGE_BYTES);
		runner->shared->value = function(runner->shared->data);
		(void)snprintf(result, 64, "value:%016llx:mem:%016llx",
			       (unsigned long long)runner->shared->value,
			       (unsigned long long)fnv1a(runner->shared->data,
						 PAGE_BYTES));
	} else {
		(void)snprintf(result, 64, "signal:%d", operation_signal);
	}
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
#else
	/* The owned kernel must establish confinement before this process runs. */
	return 1;
#endif
}

#if defined(__aarch64__)
static __attribute__((noreturn)) void run_arm64_sweep(
	struct operation_runner *runner, int traps_on, int hidden)
{
	uint64_t row_digests[N6_TABLE_ROW_COUNT];
	size_t accounted_operations = 0;
	size_t signal_rows = 0;
	size_t signal_rows_ok = 0;
	size_t mask_rows = 0;
	size_t mask_rows_ok = 0;
	size_t row_index;
	char line[1024];

	for (row_index = 0; row_index < N6_TABLE_ROW_COUNT; row_index++) {
		const struct n6_row *row = &n6_rows[row_index];
		uint64_t digest = UINT64_C(1469598103934665603);
		size_t operation_index;

		digest = digest_text(digest, row->identifier);
		digest = digest_text(digest, row->claim);
		if (strcmp(row->claim, "mask-and-audit") == 0) {
			mask_rows++;
			for (operation_index = 0; operation_index < row->count;
			     operation_index++) {
				const struct n6_operation *operation =
					&n6_operations[row->first + operation_index];

				digest = digest_text(digest, operation->name);
			}
			digest = digest_text(digest, hidden ? "hidden" : "visible");
			digest = digest_text(digest,
				N6_AUDIT_REJECTED ? "audit-rejected" : "audit-accepted");
			accounted_operations += row->count;
			if (hidden && N6_AUDIT_REJECTED)
				mask_rows_ok++;
		} else {
			int all_signals = 1;

#ifdef N6_TRAPS_OFF
			int exposed = 0;
#endif

			for (operation_index = 0; operation_index < row->count;
			     operation_index++) {
				const struct n6_operation *operation =
					&n6_operations[row->first + operation_index];
				char result[64];

				run_operation(runner, row->first + operation_index, result);
				digest = digest_text(digest, operation->name);
				digest = digest_text(digest, result);
				accounted_operations++;
				if (strncmp(result, "signal:", 7) != 0) {
					all_signals = 0;
#ifdef N6_TRAPS_OFF
					exposed = 1;
#endif
				}
			}
			if (row->signal_only) {
				signal_rows++;
				if (all_signals)
					signal_rows_ok++;
			}
#ifdef N6_TRAPS_OFF
			if (strcmp(row->identifier, "arm64-virtual-counter") == 0) {
				int bytes = snprintf(line, sizeof(line),
					"N6_TRAPS_OFF arch=%s row=%s operations=%zu "
					"traps_on=0 exposed=%d digest=%016llx",
					N6_ARCH, row->identifier, row->count, exposed,
					(unsigned long long)digest);

				if (bytes < 0 || (size_t)bytes >= sizeof(line))
					fail("traps-off-report-overflow");
				write_marker(line);
			}
#endif
		}
		row_digests[row_index] = digest;
	}

	{
		int bytes = snprintf(line, sizeof(line),
			"N6_GUEST_OK arch=%s rows=%d/%zu operations=%zu traps_on=%d "
			"signal_rows=%zu/%zu mask_rows=%zu/%zu digests=",
			N6_ARCH, N6_TABLE_ROW_COUNT, row_index, accounted_operations,
			traps_on, signal_rows_ok, signal_rows, mask_rows_ok, mask_rows);
		size_t used;

		if (bytes < 0 || (size_t)bytes >= sizeof(line))
			fail("summary-report-overflow");
		used = (size_t)bytes;
		for (row_index = 0; row_index < N6_TABLE_ROW_COUNT; row_index++) {
			bytes = snprintf(line + used, sizeof(line) - used, "%s%016llx",
				row_index == 0 ? "" : ",",
				(unsigned long long)row_digests[row_index]);
			if (bytes < 0 || (size_t)bytes >= sizeof(line) - used)
				fail("summary-report-overflow");
			used += (size_t)bytes;
		}
	}
	write_marker(line);
	for (;;)
		pause();
}
#endif

int main(void)
{
	const int traps_on = trap_policy_on();
	const int hidden = entropy_hidden();
	struct operation_runner runner;
#if !defined(__aarch64__)
	size_t row_index;
	char line[32768];
#endif

	if (N6_TABLE_ROW_COUNT == 0 || N6_TABLE_OPERATION_COUNT == 0)
		fail("empty-generated-table");
	setup_operation_runner(&runner);
#if defined(__aarch64__)
	run_arm64_sweep(&runner, traps_on, hidden);
#else
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
			for (operation_index = 0; operation_index < row->count;
			     operation_index++) {
				const struct n6_operation *operation =
					&n6_operations[row->first + operation_index];
				char result[64];
				char progress[256];
				int progress_bytes;
				run_operation(&runner, row->first + operation_index,
					      result);
				progress_bytes = snprintf(progress, sizeof(progress),
					"N6_OPERATION arch=%s row=%s operation=%zu/%zu "
					"name=%s result=%s", N6_ARCH, row->identifier,
					operation_index + 1, row->count, operation->name,
					result);
				if (progress_bytes < 0 ||
				    (size_t)progress_bytes >= sizeof(progress))
					fail("operation-report-overflow");
				write_marker(progress);
			}
			used += (size_t)snprintf(line + used, sizeof(line) - used,
				",\"traps_on\":%s}", traps_on ? "true" : "false");
		}
		if (used >= sizeof(line))
			fail("report-line-overflow");
		write_marker(line);
	}
	(void)snprintf(line, sizeof(line),
		"N6_GUEST_OK arch=%s table_rows=%d exercised_rows=%zu operations=%d",
		N6_ARCH, N6_TABLE_ROW_COUNT, row_index, N6_TABLE_OPERATION_COUNT);
	write_marker(line);
#endif
	for (;;)
		pause();
}
