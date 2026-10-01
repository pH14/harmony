// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Fixture /init for User-mode Linux qualification. The kernel passes the
 * harmony_fixture=<mode> boot parameter as an environment variable. The
 * launcher modes report on the console; the replay modes report every value
 * the guest can observe as /dev/harmony events, so two runs from one seed
 * must produce the same event sequence.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <stdarg.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/random.h>
#include <sys/reboot.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/timerfd.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define CHILDREN 16
#define WORKERS 4
#define TEXT_CHUNK 512
#define REWRITTEN_PAGES 12288

static void say(const char *line)
{
	size_t length = strlen(line);

	while (length > 0) {
		ssize_t written = write(1, line, length);

		if (written < 0 && errno == EINTR)
			continue;
		if (written <= 0)
			return;
		line += written;
		length -= (size_t)written;
	}
}

static void power_off(void)
{
	sync();
	reboot(RB_POWER_OFF);
	for (;;)
		pause();
}

static void fail(const char *reason)
{
	char line[160];

	snprintf(line, sizeof(line), "HARMONY_UML FAIL %s errno=%d\n", reason,
		 errno);
	say(line);
	power_off();
}

static void spin(void)
{
	volatile unsigned long counter = 0;

	for (;;)
		counter++;
}

static void boot(void)
{
	pid_t children[CHILDREN];
	struct utsname name;
	char line[200];

	for (int index = 0; index < CHILDREN; index++) {
		children[index] = fork();
		if (children[index] < 0)
			fail("fork");
		if (children[index] == 0) {
			for (int round = 0; round < 1000; round++)
				getppid();
			_exit(index);
		}
	}
	for (int index = 0; index < CHILDREN; index++) {
		int status;

		if (waitpid(children[index], &status, 0) != children[index])
			fail("waitpid");
		if (!WIFEXITED(status) || WEXITSTATUS(status) != index)
			fail("child-status");
	}
	if (uname(&name) != 0)
		fail("uname");
	snprintf(line, sizeof(line), "HARMONY_UML PASS release=%s machine=%s\n",
		 name.release, name.machine);
	say(line);
	power_off();
}

static int harmony = -1;

static void emit(const char *json)
{
	size_t length = strlen(json);

	if (write(harmony, json, length) != (ssize_t)length)
		fail("emit");
}

static void emitf(const char *format, ...) __attribute__((format(printf, 1, 2)));

static void emitf(const char *format, ...)
{
	char json[1024];
	va_list arguments;
	int length;

	va_start(arguments, format);
	length = vsnprintf(json, sizeof(json), format, arguments);
	va_end(arguments);
	if (length < 0 || (size_t)length >= sizeof(json))
		fail("emit-length");
	emit(json);
}

static void emit_bytes(const char *name, const unsigned char *bytes, size_t length)
{
	static const char hex[] = "0123456789abcdef";
	char json[TEXT_CHUNK * 2 + 128];
	size_t at = (size_t)snprintf(json, sizeof(json), "{\"uml\":\"%s\",\"hex\":\"", name);

	if (length > TEXT_CHUNK)
		fail("bytes-length");
	for (size_t index = 0; index < length; index++) {
		json[at++] = hex[bytes[index] >> 4];
		json[at++] = hex[bytes[index] & 15];
	}
	snprintf(json + at, sizeof(json) - at, "\"}");
	emit(json);
}

static void emit_text(const char *name, const char *text, size_t length)
{
	char json[TEXT_CHUNK * 6 + 128];
	size_t chunk = 0;

	for (size_t offset = 0; offset < length || chunk == 0; chunk++) {
		size_t end = offset + TEXT_CHUNK < length ? offset + TEXT_CHUNK : length;
		size_t at = (size_t)snprintf(json, sizeof(json),
					     "{\"uml\":\"%s\",\"chunk\":%zu,\"text\":\"",
					     name, chunk);

		for (; offset < end; offset++) {
			unsigned char byte = (unsigned char)text[offset];

			if (byte == '"' || byte == '\\') {
				json[at++] = '\\';
				json[at++] = (char)byte;
			} else if (byte < 0x20 || byte >= 0x7f) {
				at += (size_t)snprintf(json + at, sizeof(json) - at,
						       "\\u%04x", byte);
			} else {
				json[at++] = (char)byte;
			}
		}
		snprintf(json + at, sizeof(json) - at, "\"}");
		emit(json);
	}
}

static void emit_file(const char *path)
{
	static char text[65536];
	size_t length = 0;
	int fd = open(path, O_RDONLY | O_CLOEXEC);

	if (fd < 0)
		fail(path);
	for (;;) {
		ssize_t got = read(fd, text + length, sizeof(text) - length);

		if (got < 0 && errno == EINTR)
			continue;
		if (got < 0)
			fail(path);
		if (got == 0)
			break;
		length += (size_t)got;
		if (length == sizeof(text))
			fail(path);
	}
	close(fd);
	emit_text(path, text, length);
}

static uint64_t counter(void)
{
#if defined(__x86_64__)
	uint32_t low, high;

	__asm__ volatile("rdtsc" : "=a"(low), "=d"(high));
	return ((uint64_t)high << 32) | low;
#elif defined(__aarch64__)
	uint64_t value;

	__asm__ volatile("isb\n\tmrs %0, cntvct_el0" : "=r"(value));
	return value;
#else
#error "no counter read for this architecture"
#endif
}

static void emit_clocks(const char *label)
{
	static const struct {
		const char *name;
		clockid_t id;
	} clocks[] = {
		{ "realtime", CLOCK_REALTIME },
		{ "monotonic", CLOCK_MONOTONIC },
		{ "boottime", CLOCK_BOOTTIME },
		{ "monotonic_raw", CLOCK_MONOTONIC_RAW },
		{ "process", CLOCK_PROCESS_CPUTIME_ID },
		{ "thread", CLOCK_THREAD_CPUTIME_ID },
	};
	struct timeval wall;

	for (size_t index = 0; index < sizeof(clocks) / sizeof(clocks[0]); index++) {
		struct timespec now;

		if (clock_gettime(clocks[index].id, &now) != 0)
			fail("clock_gettime");
		emitf("{\"uml\":\"clock\",\"at\":\"%s\",\"clock\":\"%s\",\"ns\":%lld}",
		      label, clocks[index].name,
		      (long long)now.tv_sec * 1000000000LL + now.tv_nsec);
	}
	if (gettimeofday(&wall, NULL) != 0)
		fail("gettimeofday");
	emitf("{\"uml\":\"gettimeofday\",\"at\":\"%s\",\"us\":%lld,\"time\":%lld}",
	      label, (long long)wall.tv_sec * 1000000LL + wall.tv_usec,
	      (long long)time(NULL));
}

static void open_harmony(void)
{
	if (mount("devtmpfs", "/dev", "devtmpfs", 0, NULL) != 0)
		fail("mount-dev");
	harmony = open("/dev/harmony", O_RDWR | O_CLOEXEC);
	if (harmony < 0)
		fail("open-harmony");
}

static void finish(const char *mode)
{
	emitf("{\"uml\":\"done\",\"mode\":\"%s\"}", mode);
	say("HARMONY_UML DONE\n");
	power_off();
}

static void wait_child(pid_t child, const char *name)
{
	int status;

	if (waitpid(child, &status, 0) != child)
		fail("waitpid");
	emitf("{\"uml\":\"exit\",\"child\":\"%s\",\"status\":%d}", name, status);
}

static void counters(void)
{
#if defined(__aarch64__)
	uint64_t frequency;

	__asm__ volatile("mrs %0, cntfrq_el0" : "=r"(frequency));
	emitf("{\"uml\":\"counter_frequency\",\"value\":%llu}",
	      (unsigned long long)frequency);
#endif
	for (int round = 0; round < 4; round++) {
		unsigned long long first = counter();
		unsigned long long second;

		getppid();
		second = counter();
		emitf("{\"uml\":\"counter\",\"round\":%d,\"first\":%llu,\"second\":%llu}",
		      round, first, second);
	}
	finish("counter");
}

static volatile unsigned char *rewritten;

static void rewrite(int pass)
{
	long size = sysconf(_SC_PAGESIZE);
	struct timespec now;

	for (long page = 0; page < REWRITTEN_PAGES; page++)
		rewritten[page * size] = (unsigned char)pass;
	if (clock_gettime(CLOCK_THREAD_CPUTIME_ID, &now) != 0)
		fail("clock_gettime");
	emitf("{\"uml\":\"rewrite\",\"pass\":%d,\"thread_ns\":%lld}", pass,
	      (long long)now.tv_sec * 1000000000LL + now.tv_nsec);
}

static void values(void)
{
	unsigned char bytes[32];
	unsigned char drawn[8];
	struct utsname name;
	void *mapping;
	pid_t child;

	mapping = mmap(NULL, (size_t)REWRITTEN_PAGES * sysconf(_SC_PAGESIZE),
		       PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (mapping == MAP_FAILED)
		fail("mmap");
	rewritten = mapping;
	rewrite(0);
	emit_clocks("start");
	if (uname(&name) != 0)
		fail("uname");
	emitf("{\"uml\":\"uname\",\"sysname\":\"%s\",\"nodename\":\"%s\",\"release\":\"%s\",\"machine\":\"%s\"}",
	      name.sysname, name.nodename, name.release, name.machine);
	emit_text("version", name.version, strlen(name.version));
	if (getrandom(bytes, sizeof(bytes), 0) != (ssize_t)sizeof(bytes))
		fail("getrandom");
	emit_bytes("getrandom", bytes, sizeof(bytes));
	emit_bytes("at_random", (const unsigned char *)getauxval(AT_RANDOM), 16);
	if (write(harmony, "", 1) != 1 || read(harmony, drawn, sizeof(drawn)) != (ssize_t)sizeof(drawn))
		fail("harmony-entropy");
	emit_bytes("harmony_entropy", drawn, sizeof(drawn));
	rewrite(1);
	mapping = mmap(NULL, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (mapping == MAP_FAILED)
		fail("mmap");
	emitf("{\"uml\":\"addresses\",\"mmap\":\"%p\",\"heap\":\"%p\",\"stack\":\"%p\"}",
	      mapping, malloc(64), (void *)&mapping);
	emitf("{\"uml\":\"ids\",\"pid\":%d,\"ppid\":%d,\"uid\":%d,\"pgid\":%d,\"sid\":%d}",
	      getpid(), getppid(), getuid(), getpgrp(), getsid(0));
	rewrite(2);
	emit_file("/proc/self/auxv");
	emit_file("/proc/self/maps");
	emit_file("/proc/self/stat");
	emit_file("/proc/cmdline");
	emit_file("/proc/cpuinfo");
	emit_file("/proc/meminfo");
	emit_file("/proc/stat");
	rewrite(3);
	emit_file("/proc/uptime");
	emit_file("/proc/interrupts");
	emit_file("/proc/loadavg");
	emit_file("/proc/sys/kernel/random/boot_id");
	rewrite(4);
	child = fork();
	if (child < 0)
		fail("fork");
	if (child == 0) {
		execl("/registers", "registers", (char *)NULL);
		_exit(127);
	}
	wait_child(child, "registers");
	rewrite(5);
	emit_clocks("end");
	finish("values");
}

static void worker(int index, int pipe_in, int pipe_out)
{
	struct timespec now;
	char token;

	for (int round = 0; round < 40; round++) {
		for (int call = 0; call < index * 7 + round % 5; call++)
			getppid();
		if (round % 10 == index)
			sched_yield();
		if (index < 2 && round % 4 == 0) {
			if (write(pipe_out, "x", 1) != 1 || read(pipe_in, &token, 1) != 1)
				fail("pipe");
		}
		if (clock_gettime(CLOCK_MONOTONIC, &now) != 0)
			fail("clock_gettime");
		emitf("{\"uml\":\"worker\",\"index\":%d,\"round\":%d,\"ns\":%lld}",
		      index, round, (long long)now.tv_sec * 1000000000LL + now.tv_nsec);
	}
	_exit(index);
}

static void schedule(void)
{
	int ping[2], pong[2];
	pid_t children[WORKERS];

	if (pipe(ping) != 0 || pipe(pong) != 0)
		fail("pipe");
	for (int index = 0; index < WORKERS; index++) {
		children[index] = fork();
		if (children[index] < 0)
			fail("fork");
		if (children[index] == 0)
			worker(index, index == 0 ? pong[0] : ping[0],
			       index == 0 ? ping[1] : pong[1]);
	}
	for (int index = 0; index < WORKERS; index++)
		wait_child(children[index], "worker");
	emit_clocks("end");
	finish("schedule");
}

static volatile sig_atomic_t alarms;

static void on_alarm(int signal)
{
	(void)signal;
	alarms++;
}

static void timers(void)
{
	struct itimerval interval = { { 0, 3000 }, { 0, 3000 } };
	struct itimerspec deadline = { { 0, 0 }, { 0, 2500000 } };
	struct pollfd none = { -1, 0, 0 };
	struct sigaction action = { 0 };
	uint64_t expirations;
	int timer;

	emit_clocks("start");
	for (long nanoseconds = 1; nanoseconds <= 100000000; nanoseconds *= 10) {
		struct timespec pause = { nanoseconds / 1000000000, nanoseconds % 1000000000 };

		if (nanosleep(&pause, NULL) != 0)
			fail("nanosleep");
		emit_clocks("nanosleep");
	}
	action.sa_handler = on_alarm;
	if (sigaction(SIGALRM, &action, NULL) != 0 || setitimer(ITIMER_REAL, &interval, NULL) != 0)
		fail("itimer");
	while (alarms < 5)
		getppid();
	interval = (struct itimerval){ { 0, 0 }, { 0, 0 } };
	setitimer(ITIMER_REAL, &interval, NULL);
	emitf("{\"uml\":\"alarms\",\"count\":%d}", (int)alarms);
	emit_clocks("alarms");
	timer = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC);
	if (timer < 0 || timerfd_settime(timer, 0, &deadline, NULL) != 0 ||
	    read(timer, &expirations, sizeof(expirations)) != (ssize_t)sizeof(expirations))
		fail("timerfd");
	emitf("{\"uml\":\"timerfd\",\"expirations\":%llu}", (unsigned long long)expirations);
	emit_clocks("timerfd");
	if (poll(&none, 1, 7) != 0)
		fail("poll");
	emit_clocks("poll");
	{
		struct timeval wait = { 0, 1300 };

		if (select(0, NULL, NULL, NULL, &wait) != 0)
			fail("select");
	}
	emit_clocks("select");
	finish("timers");
}

static void flood(void)
{
	static const char line[] =
		"HARMONY_UML FLOOD 0123456789abcdef0123456789abcdef0123456789abcdef\n";

	for (;;)
		say(line);
}

static void orphans(void)
{
	char line[80];

	for (int index = 0; index < CHILDREN; index++) {
		pid_t child = fork();

		if (child < 0)
			fail("fork");
		if (child == 0)
			for (;;)
				pause();
	}
	snprintf(line, sizeof(line), "HARMONY_UML ORPHANS %d\n", CHILDREN);
	say(line);
	spin();
}

int main(void)
{
	const char *mode = getenv("harmony_fixture");

	if (mount("proc", "/proc", "proc", 0, NULL) != 0)
		fail("mount-proc");
	say("HARMONY_UML READY\n");
	if (mode == NULL || strcmp(mode, "boot") == 0)
		boot();
	else if (strcmp(mode, "hang") == 0)
		spin();
	else if (strcmp(mode, "flood") == 0)
		flood();
	else if (strcmp(mode, "orphans") == 0)
		orphans();
	else if (strcmp(mode, "exit") == 0)
		return 3;
	open_harmony();
	if (strcmp(mode, "values") == 0)
		values();
	else if (strcmp(mode, "schedule") == 0)
		schedule();
	else if (strcmp(mode, "timers") == 0)
		timers();
	else if (strcmp(mode, "counter") == 0)
		counters();
	fail("unknown-mode");
	return 1;
}
