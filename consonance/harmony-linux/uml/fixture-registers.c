// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Reports the registers a new process starts with as one /dev/harmony event:
 * every general-purpose register, the flags and the floating-point and vector
 * state (the FXSAVE image on x86-64, v0-v31 on arm64). Built without a C
 * library so nothing runs before _start saves them.
 */
#include <stddef.h>
#include <stdint.h>
#include <fcntl.h>
#include <sys/syscall.h>

#if defined(__x86_64__)
#define SAVED 17
#define VECTOR_KEY "fxsave"

static long call3(long number, long first, long second, long third)
{
	long result;

	__asm__ volatile("syscall"
			 : "=a"(result)
			 : "a"(number), "D"(first), "S"(second), "d"(third)
			 : "rcx", "r11", "memory");
	return result;
}

static long open_harmony(void)
{
	return call3(SYS_open, (long)"/dev/harmony", O_WRONLY | O_CLOEXEC, 0);
}
#elif defined(__aarch64__)
#define SAVED 35
#define VECTOR_KEY "vectors"

static long call3(long number, long first, long second, long third)
{
	register long x8 __asm__("x8") = number;
	register long x0 __asm__("x0") = first;
	register long x1 __asm__("x1") = second;
	register long x2 __asm__("x2") = third;

	__asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory");
	return x0;
}

static long open_harmony(void)
{
	return call3(SYS_openat, AT_FDCWD, (long)"/dev/harmony", O_WRONLY | O_CLOEXEC);
}
#else
#error "no register fixture for this architecture"
#endif

static char json[2048];

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
#if defined(__x86_64__)
	static unsigned char image[512] __attribute__((aligned(64)));

	__asm__ volatile("fxsave64 %0" : "=m"(image));
#else
	const unsigned char *image = (const unsigned char *)&saved[SAVED + 1];
#endif
	at = put_text(0, "{\"uml\":\"registers\",\"saved\":\"");
	for (int index = 0; index < SAVED; index++) {
		if (index)
			json[at++] = ' ';
		at = put_hex(at, (const unsigned char *)&saved[index], sizeof(saved[index]));
	}
	at = put_text(at, "\",\"" VECTOR_KEY "\":\"");
	for (size_t index = 0; index < 512; index++)
		at = put_hex(at, &image[index], 1);
	at = put_text(at, "\"}");
	fd = open_harmony();
	if (fd < 0 || call3(SYS_write, fd, (long)json, (long)at) != (long)at)
		call3(SYS_exit_group, 1, 0, 0);
	call3(SYS_exit_group, 0, 0, 0);
	__builtin_unreachable();
}

#if defined(__x86_64__)
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
#else
/*
 * saved[0..30] are x0-x30, then sp, nzcv, fpcr and fpsr; v0-v31 follow at
 * byte 288. A plain sub leaves the flags alone.
 */
__asm__(
	".text\n"
	".global _start\n"
	".type _start, %function\n"
	"_start:\n"
	"sub sp, sp, #800\n"
	"stp x0, x1, [sp, #0]\n"
	"stp x2, x3, [sp, #16]\n"
	"stp x4, x5, [sp, #32]\n"
	"stp x6, x7, [sp, #48]\n"
	"stp x8, x9, [sp, #64]\n"
	"stp x10, x11, [sp, #80]\n"
	"stp x12, x13, [sp, #96]\n"
	"stp x14, x15, [sp, #112]\n"
	"stp x16, x17, [sp, #128]\n"
	"stp x18, x19, [sp, #144]\n"
	"stp x20, x21, [sp, #160]\n"
	"stp x22, x23, [sp, #176]\n"
	"stp x24, x25, [sp, #192]\n"
	"stp x26, x27, [sp, #208]\n"
	"stp x28, x29, [sp, #224]\n"
	"str x30, [sp, #240]\n"
	"add x0, sp, #800\n"
	"mrs x1, nzcv\n"
	"stp x0, x1, [sp, #248]\n"
	"mrs x0, fpcr\n"
	"mrs x1, fpsr\n"
	"stp x0, x1, [sp, #264]\n"
	"add x0, sp, #288\n"
	"st1 {v0.2d, v1.2d, v2.2d, v3.2d}, [x0], #64\n"
	"st1 {v4.2d, v5.2d, v6.2d, v7.2d}, [x0], #64\n"
	"st1 {v8.2d, v9.2d, v10.2d, v11.2d}, [x0], #64\n"
	"st1 {v12.2d, v13.2d, v14.2d, v15.2d}, [x0], #64\n"
	"st1 {v16.2d, v17.2d, v18.2d, v19.2d}, [x0], #64\n"
	"st1 {v20.2d, v21.2d, v22.2d, v23.2d}, [x0], #64\n"
	"st1 {v24.2d, v25.2d, v26.2d, v27.2d}, [x0], #64\n"
	"st1 {v28.2d, v29.2d, v30.2d, v31.2d}, [x0], #64\n"
	"mov x0, sp\n"
	"bl report\n"
	"brk #0\n"
	".size _start, .-_start\n");
#endif
