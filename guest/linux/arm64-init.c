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
#define NR_EXIT_GROUP 94
#define NR_CLONE 220
#define NR_WAIT4 260

#define SIGCHLD 17
#define SIGILL 4

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

#ifdef HARMONY_AA5_EL0_PROBE
/*
 * AA-5(b) live closure probe, built ONLY into the el0probe initramfs variant
 * (this planted CNTVCT_EL0 read must never ship in the scan-clean init).
 *
 * Under CNTKCTL_EL1 EL0 denial the raw read traps to EL1; the owned kernel's
 * trap emulation resolves through the pvclock page. 1000 consecutive EL0
 * reads therefore observe at most a handful of distinct values (the page
 * advances only at exact publications), while an unclosed live counter makes
 * essentially every read distinct. A SIGILL death is the stricter
 * deny-without-emulation posture and also proves closure.
 */
#define EL0_PROBE_READS 1000
#define EL0_PROBE_MAX_DISTINCT 16
#define EL0_PROBE_LIVE_EXIT 7

static __attribute__((noreturn)) void exit_group(long code)
{
	syscall1(NR_EXIT_GROUP, code);
	stop();
}

static __attribute__((noreturn)) void el0_probe_child(void)
{
	unsigned long previous = 0;
	unsigned long distinct = 0;
	unsigned long index;

	for (index = 0; index < EL0_PROBE_READS; index++) {
		unsigned long value;

		__asm__ volatile("mrs %0, cntvct_el0" : "=r"(value));
		if (index == 0 || value != previous)
			distinct++;
		previous = value;
	}
	exit_group(distinct > EL0_PROBE_MAX_DISTINCT ? EL0_PROBE_LIVE_EXIT : 0);
}

static long wait_for(long pid)
{
	int status = 0;
	long waited = syscall5(NR_WAIT4, pid, (long)&status, 0, 0, 0);

	if (waited != pid)
		fail();
	return status;
}

static void el0_counter_probe(void)
{
	long pid;
	int status;

	pid = (long)syscall5(NR_CLONE, SIGCHLD, 0, 0, 0, 0);
	if (pid < 0) {
		write_all("HARMONY_AA5_EL0_CLONE_FAIL\n");
		fail();
	}
	if (pid == 0)
		el0_probe_child();
	status = (int)wait_for(pid);
	if ((status & 0x7f) == SIGILL) {
		write_all("HARMONY_AA5_EL0_CNTVCT_UNDEF_OK\n");
	} else if ((status & 0x7f) == 0 && ((status >> 8) & 0xff) == 0) {
		write_all("HARMONY_AA5_EL0_CNTVCT_PAGE_OK\n");
	} else if ((status & 0x7f) == 0 && ((status >> 8) & 0xff) == EL0_PROBE_LIVE_EXIT) {
		/* The EL0 read observed a moving value: the raw counter is reachable. */
		write_all("HARMONY_AA5_EL0_LIVE_COUNTER_HOLE\n");
		fail();
	} else {
		write_all("HARMONY_AA5_EL0_WAIT_STATUS_FAIL\n");
		fail();
	}

	/* Discriminator control: a cleanly exiting nonzero child must be told apart. */
	pid = (long)syscall5(NR_CLONE, SIGCHLD, 0, 0, 0, 0);
	if (pid < 0)
		fail();
	if (pid == 0)
		exit_group(42);
	status = (int)wait_for(pid);
	if ((status & 0x7f) != 0 || ((status >> 8) & 0xff) != 42) {
		write_all("HARMONY_AA5_EL0_CONTROL_FAIL\n");
		fail();
	}
	write_all("HARMONY_AA5_EL0_PROBE_CONTROL_OK\n");
}
#endif

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
#ifdef HARMONY_AA5_EL0_PROBE
	/* Probe only after the pvclock clocksource is verified live: "page time"
	 * is only a meaningful verdict on a guest actually running the page.
	 */
	el0_counter_probe();
#endif
	write_all("HARMONY_AA5_READY\n");
	stop();
}
