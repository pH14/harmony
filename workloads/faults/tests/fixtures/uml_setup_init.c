// SPDX-License-Identifier: AGPL-3.0-or-later

#include <stdint.h>

#if defined(__x86_64__)
#define SYS_WRITE 1
#define SYS_IOCTL 16
#define SYS_MOUNT 165
#define SYS_OPENAT 257
static long call(long number, long a, long b, long c, long d, long e)
{
	register long r10 __asm__("r10") = d;
	register long r8 __asm__("r8") = e;
	long result;
	__asm__ volatile("syscall" : "=a"(result)
			 : "a"(number), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
			 : "rcx", "r11", "memory");
	return result;
}
#elif defined(__aarch64__)
#define SYS_WRITE 64
#define SYS_IOCTL 29
#define SYS_MOUNT 40
#define SYS_OPENAT 56
static long call(long number, long a, long b, long c, long d, long e)
{
	register long x8 __asm__("x8") = number;
	register long x0 __asm__("x0") = a;
	register long x1 __asm__("x1") = b;
	register long x2 __asm__("x2") = c;
	register long x3 __asm__("x3") = d;
	register long x4 __asm__("x4") = e;
	__asm__ volatile("svc 0" : "+r"(x0)
			 : "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x8) : "memory");
	return x0;
}
#else
#error unsupported architecture
#endif

struct header {
	uint32_t magic;
	uint16_t kind, service, opcode, status;
	uint32_t sequence, length, reserved;
} __attribute__((packed));

struct exchange {
	uint64_t request, response;
	uint32_t request_length, response_capacity, response_length, reserved;
};

__attribute__((used, noreturn)) static void probe_main(void)
{
	struct {
		struct header header;
		uint32_t event;
	} request = {
		.header = { .magic = 0x31504348, .kind = 1, .service = 4,
			.opcode = 1, .sequence = 1, .length = 4 },
		.event = 4U << 24,
	};
	unsigned char response[64] = { 0 };
	struct exchange exchange = {
		.request = (uint64_t)(uintptr_t)&request,
		.response = (uint64_t)(uintptr_t)response,
		.request_length = sizeof(request),
		.response_capacity = sizeof(response),
	};
	long fd;

	if (call(SYS_MOUNT, (long)"devtmpfs", (long)"/dev", (long)"devtmpfs", 0, 0))
		goto fail;
	fd = call(SYS_OPENAT, -100, (long)"/dev/harmony", 2, 0, 0);
	if (fd < 0 || call(SYS_IOCTL, fd, 0xc0204801UL, (long)&exchange, 0, 0))
		goto fail;
	for (;;) {
		static const char event[] = "{\"fixture\":\"faults-infrastructure\"}";
		if (call(SYS_WRITE, fd, (long)event, sizeof(event) - 1, 0, 0) < 0)
			goto fail;
	}
fail:
	call(SYS_WRITE, 1, (long)"FAULTS INFRASTRUCTURE FIXTURE FAILED\n",
	     sizeof("FAULTS INFRASTRUCTURE FIXTURE FAILED\n") - 1, 0, 0);
	for (;;)
		__asm__ volatile("" : : : "memory");
}

#if defined(__x86_64__)
__asm__(".global _start\n_start:\n xor %ebp,%ebp\n and $-16,%rsp\n call probe_main\n ud2\n");
#else
__asm__(".global _start\n_start:\n bl probe_main\n brk #0\n");
#endif
