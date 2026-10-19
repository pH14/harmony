// SPDX-License-Identifier: AGPL-3.0-or-later

#ifndef TRANSACTION_KIND
#define TRANSACTION_KIND 0
#endif

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

void _start(void)
{
	long outcome = 0;
#if TRANSACTION_KIND == 1
	__asm__ volatile("xbegin 1f; mov $64, %%ecx; 2: pause; loop 2b; xend; "
			 "mov $1, %%eax; jmp 3f; 1: xor %%eax, %%eax; 3:"
			 : "=a"(outcome) : : "ecx", "memory");
#elif TRANSACTION_KIND == 2
	static int lock;
	int value = 1;
	__asm__ volatile("xacquire xchg %0, %1; xrelease movl $0, %1"
			 : "+r"(value), "+m"(lock) : : "memory");
	outcome = value;
#else
	__asm__ volatile("pause" : : : "memory");
#endif
	if (outcome)
		call(1, 1, (long)"TRANSACTION COMMIT\n", 19, 0, 0);
	else
		call(1, 1, (long)"UML TRANSACTION CONTROL PASS\n", 29, 0, 0);
	call(83, (long)"/proc", 0755, 0, 0, 0);
	if (call(165, (long)"proc", (long)"/proc", (long)"proc", 0, 0))
		for (;;) {}
	long fd = call(257, -100, (long)"/proc/exitcode", 1, 0, 0);
	if (fd < 0 || call(1, fd, (long)"0\n", 2, 0, 0) != 2)
		for (;;) {}
	call(169, 0xfee1dead, 672274793, 0x4321fedc, 0, 0);
	for (;;) {}
}
