// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Reports the registers a new process starts with as one /dev/harmony event:
 * the flags, every general-purpose register and the FXSAVE image. Built
 * without a C library so nothing runs before _start saves them.
 */
#include <stddef.h>
#include <stdint.h>
#include <fcntl.h>
#include <sys/syscall.h>

#define SAVED 17

static long call3(long number, long first, long second, long third)
{
	long result;

	__asm__ volatile("syscall"
			 : "=a"(result)
			 : "a"(number), "D"(first), "S"(second), "d"(third)
			 : "rcx", "r11", "memory");
	return result;
}

static char json[2048];
static unsigned char image[512] __attribute__((aligned(64)));

static size_t put_text(size_t at, const char *text)
{
	while (*text)
		json[at++] = *text++;
	return at;
}

static size_t put_hex(size_t at, const unsigned char *bytes, size_t length)
{
	static const char hex[] = "0123456789abcdef";

	for (size_t index = length; index > 0; index--) {
		json[at++] = hex[bytes[index - 1] >> 4];
		json[at++] = hex[bytes[index - 1] & 15];
	}
	return at;
}

__attribute__((used, noreturn)) void report(const uint64_t *saved)
{
	size_t at;
	long fd;

	__asm__ volatile("fxsave64 %0" : "=m"(image));
	at = put_text(0, "{\"uml\":\"registers\",\"saved\":\"");
	for (int index = 0; index < SAVED; index++) {
		if (index)
			json[at++] = ' ';
		at = put_hex(at, (const unsigned char *)&saved[index], sizeof(saved[index]));
	}
	at = put_text(at, "\",\"fxsave\":\"");
	for (size_t index = 0; index < sizeof(image); index++)
		at = put_hex(at, &image[index], 1);
	at = put_text(at, "\"}");
	fd = call3(SYS_open, (long)"/dev/harmony", O_WRONLY | O_CLOEXEC, 0);
	if (fd < 0 || call3(SYS_write, fd, (long)json, (long)at) != (long)at)
		call3(SYS_exit_group, 1, 0, 0);
	call3(SYS_exit_group, 0, 0, 0);
	__builtin_unreachable();
}

__attribute__((naked, noreturn)) void _start(void)
{
	__asm__ volatile(
		"pushfq\n"
		"push %rax\n"
		"push %rbx\n"
		"push %rcx\n"
		"push %rdx\n"
		"push %rsi\n"
		"push %rdi\n"
		"push %rbp\n"
		"push %r8\n"
		"push %r9\n"
		"push %r10\n"
		"push %r11\n"
		"push %r12\n"
		"push %r13\n"
		"push %r14\n"
		"push %r15\n"
		"lea 128(%rsp), %rax\n"
		"push %rax\n"
		"mov %rsp, %rdi\n"
		"and $-16, %rsp\n"
		"call report\n"
		"ud2\n");
}
