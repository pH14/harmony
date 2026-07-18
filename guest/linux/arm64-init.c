// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * AA-5(c) owned init: no libc, runtime, shell, or outline-atomic library.
 * The image build scans this exact ELF for reservation-monitor instructions.
 */

#define AT_FDCWD (-100)
#define O_RDONLY 0

#define NR_MOUNT 40
#define NR_OPENAT 56
#define NR_CLOSE 57
#define NR_READ 63
#define NR_WRITE 64

static long syscall1(long number, long arg0)
{
	register long x0 __asm__("x0") = arg0;
	register long x8 __asm__("x8") = number;

	__asm__ volatile("svc #0" : "+r" (x0) : "r" (x8) : "memory");
	return x0;
}

static long syscall3(long number, long arg0, long arg1, long arg2)
{
	register long x0 __asm__("x0") = arg0;
	register long x1 __asm__("x1") = arg1;
	register long x2 __asm__("x2") = arg2;
	register long x8 __asm__("x8") = number;

	__asm__ volatile("svc #0"
			 : "+r" (x0)
			 : "r" (x1), "r" (x2), "r" (x8)
			 : "memory");
	return x0;
}

static long syscall5(long number, long arg0, long arg1, long arg2,
		     long arg3, long arg4)
{
	register long x0 __asm__("x0") = arg0;
	register long x1 __asm__("x1") = arg1;
	register long x2 __asm__("x2") = arg2;
	register long x3 __asm__("x3") = arg3;
	register long x4 __asm__("x4") = arg4;
	register long x8 __asm__("x8") = number;

	__asm__ volatile("svc #0"
			 : "+r" (x0)
			 : "r" (x1), "r" (x2), "r" (x3), "r" (x4), "r" (x8)
			 : "memory");
	return x0;
}

static unsigned long string_length(const char *string)
{
	unsigned long length = 0;

	while (string[length] != '\0')
		length++;
	return length;
}

static void write_all(const char *string)
{
	unsigned long length = string_length(string);

	while (length != 0) {
		long written = syscall3(NR_WRITE, 1, (long)string, (long)length);

		if (written <= 0)
			break;
		string += written;
		length -= (unsigned long)written;
	}
}

static int bytes_equal(const char *left, const char *right, unsigned long length)
{
	unsigned long index;

	for (index = 0; index < length; index++) {
		if (left[index] != right[index])
			return 0;
	}
	return 1;
}

static __attribute__((noreturn)) void stop(void)
{
	for (;;)
		__asm__ volatile("b .+4");
}

static __attribute__((noreturn)) void fail(void)
{
	write_all("HARMONY_AA5_FAIL\n");
	stop();
}

void __attribute__((noreturn)) _start(void)
{
	static const char source[] = "sysfs";
	static const char target[] = "/sys";
	static const char clocksource_path[] =
		"/sys/devices/system/clocksource/clocksource0/current_clocksource";
	static const char expected[] = "harmony-arm-pvclock\n";
	char value[sizeof(expected)];
	long fd;
	long count;

	if (syscall5(NR_MOUNT, (long)source, (long)target, (long)source, 0, 0) != 0)
		fail();

	fd = syscall3(NR_OPENAT, AT_FDCWD, (long)clocksource_path, O_RDONLY);
	if (fd < 0)
		fail();
	count = syscall3(NR_READ, fd, (long)value, sizeof(value));
	(void)syscall1(NR_CLOSE, fd);
	if (count != (long)(sizeof(expected) - 1) ||
	    !bytes_equal(value, expected, sizeof(expected) - 1))
		fail();

	write_all("HARMONY_AA5_CLOCKSOURCE_OK\n");
	write_all("HARMONY_AA5_READY\n");
	stop();
}
