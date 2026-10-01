// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Fixture /init for User-mode Linux qualification. The kernel passes the
 * harmony_fixture=<mode> boot parameter as an environment variable; each mode
 * exercises one launcher property and reports it on the console.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHILDREN 16

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
	fail("unknown-mode");
	return 1;
}
