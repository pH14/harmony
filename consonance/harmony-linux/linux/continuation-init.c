// SPDX-License-Identifier: AGPL-3.0-or-later

typedef unsigned long u64;

static long call3(long nr, long a, long b, long c)
{
	register long ax __asm__("rax") = nr;
	register long di __asm__("rdi") = a;
	register long si __asm__("rsi") = b;
	register long dx __asm__("rdx") = c;
	__asm__ volatile("syscall" : "+a"(ax) : "D"(di), "S"(si), "d"(dx)
			 : "rcx", "r11", "memory");
	return ax;
}

static void write_all(const char *s, u64 n)
{
	while (n) {
		long done = call3(1, 1, (long)s, n);
		if (done <= 0)
			for (;;) {}
		s += done;
		n -= done;
	}
}

static void hex(u64 value)
{
	char output[17];
	for (int i = 0; i != 16; i++)
		output[i] = "0123456789abcdef"[(value >> (60 - 4 * i)) & 15];
	output[16] = ' ';
	write_all(output, sizeof(output));
}

static volatile unsigned char memory[4096];
static const u64 vector[2] = { 0x3141592653589793, 0x2384626433832795 };
static const u64 detour_vector[2] = { 0xfedcba9876543210, 0x0123456789abcdef };
static const u64 floating = 0x400921fb54442d18;

void _start(void)
{
	char input[2];
	u64 observed[2], fp, random, time[2];
	const u64 delay[2] = { 0, 2000000 };
	u64 hash = 0xcbf29ce484222325;
	for (u64 i = 0; i != sizeof(memory); i++)
		memory[i] = (unsigned char)(i * 17 + 31);
	__asm__ volatile("movdqu %0, %%xmm15; fninit; fldl %1"
			 : : "m"(vector), "m"(floating) : "memory");
	write_all("X3_READY\n", 9);
	for (;;) {
		if (call3(0, 0, (long)input, sizeof(input)) != 2 || input[1] != '\n')
			for (;;) {}
		if (input[0] == 'A')
			break;
		if (input[0] != 'B')
			for (;;) {}
		for (u64 i = 0; i != sizeof(memory); i++)
			memory[i] ^= 0xa5;
		__asm__ volatile("movdqu %0, %%xmm15; fadd %%st(0), %%st(0)"
				 : : "m"(detour_vector) : "memory");
		write_all("X3_DETOUR\n", 10);
	}
	__asm__ volatile("movdqu %%xmm15, %0; fstpl %1"
			 : "=m"(observed), "=m"(fp) : : "memory");
	for (u64 i = 0; i != sizeof(memory); i++)
		hash = (hash ^ memory[i]) * 0x100000001b3;
	if (call3(35, (long)delay, 0, 0) || call3(228, 1, (long)time, 0) ||
	    call3(318, (long)&random, sizeof(random), 0) != sizeof(random))
		for (;;) {}
	write_all("X3_RESULT ", 10);
	hex(hash);
	hex(observed[0]);
	hex(observed[1]);
	hex(fp);
	hex(time[0]);
	hex(time[1]);
	hex(random);
	write_all("\nX3_DONE\n", 9);
	call3(169, 0xfee1dead, 672274793, 0x4321fedc);
	for (;;) {}
}
