// SPDX-License-Identifier: AGPL-3.0-or-later

#define _GNU_SOURCE
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#if defined(__x86_64__)
#define NATIVE_ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define NATIVE_ARCH AUDIT_ARCH_AARCH64
#else
#error unsupported test architecture
#endif

static int listener(void)
{
	struct sock_filter code[] = {
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, NATIVE_ARCH, 1, 0),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_KILL_PROCESS),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_mprotect, 0, 7),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 4096, 0, 5),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1]) + 4),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 0, 3),
		BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
		BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, PROT_READ | PROT_WRITE | PROT_EXEC, 0, 1),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
		BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
	};
	struct sock_fprog program = { .len = sizeof(code) / sizeof(code[0]), .filter = code };
	if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))
		return -1;
	return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER,
		       SECCOMP_FILTER_FLAG_NEW_LISTENER, &program);
}

static long seconds(void)
{
	struct timespec now;
	if (clock_gettime(CLOCK_MONOTONIC, &now))
		return -1;
	return now.tv_sec;
}

static int cleanup(void)
{
	int leftovers = 0;
	for (;;) {
		char path[80];
		snprintf(path, sizeof(path), "/proc/self/task/%ld/children", (long)getpid());
		FILE *children = fopen(path, "r");
		if (!children)
			return -1;
		int pid, found = 0;
		while (fscanf(children, "%d", &pid) == 1) {
			found = 1;
			leftovers = 1;
			if (getpgid(pid) == pid)
				kill(-pid, SIGKILL);
			kill(pid, SIGKILL);
		}
		fclose(children);
		if (!found)
			return leftovers;
		while (waitpid(-1, NULL, __WALL) < 0 && errno == EINTR) {}
	}
}

int main(int argc, char **argv)
{
	if (argc < 3)
		return 2;
	signal(SIGPIPE, SIG_IGN);
	int result = 1, status = 0, reaped = 0, armed = 0;
	unsigned int continued = 0, denied = 0;
	pid_t child = -1;
	struct sockaddr_un address = { .sun_family = AF_UNIX };
	if (strlen(argv[1]) >= sizeof(address.sun_path))
		return 2;
	strcpy(address.sun_path, argv[1]);
	int server = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
	if (server < 0 || bind(server, (void *)&address, sizeof(address)) || listen(server, 1) ||
	    prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0))
		goto done;
	int notifications = listener();
	if (notifications < 0)
		goto done;
	child = fork();
	if (child < 0)
		goto done;
	if (!child) {
		close(server);
		close(notifications);
		if (prctl(PR_SET_PDEATHSIG, SIGKILL) ||
		    setenv("HARMONY_UML_FAULT_CONTROL", argv[1], 1))
			_exit(2);
		execv(argv[2], &argv[2]);
		_exit(2);
	}
	struct pollfd fds[] = {
		{ .fd = notifications, .events = POLLIN },
		{ .fd = server, .events = POLLIN },
	};
	long started = seconds();
	if (started < 0)
		goto done;
	for (;;) {
		pid_t ended = waitpid(child, &status, WNOHANG);
		if (ended == child) {
			reaped = 1;
			result = !WIFEXITED(status) || WEXITSTATUS(status) ||
				 !armed || !continued || denied != 1;
			break;
		}
		if ((ended < 0 && errno != EINTR) || seconds() - started >= 90)
			break;
		int ready = poll(fds, 2, 100);
		if (ready < 0) {
			if (errno == EINTR)
				continue;
			break;
		}
		if (fds[1].revents & POLLIN) {
			if (fds[1].fd == server) {
				fds[1].fd = accept4(server, NULL, NULL, SOCK_CLOEXEC);
				if (fds[1].fd < 0)
					break;
			} else {
				char command;
				if (read(fds[1].fd, &command, 1) != 1 || command != 'A' || armed || !continued)
					break;
				armed = 1;
				if (write(fds[1].fd, "A", 1) != 1)
					break;
				close(fds[1].fd);
				fds[1].fd = -1;
			}
		}
		if (fds[0].revents & POLLIN) {
			struct seccomp_notif request = {0};
			if (ioctl(notifications, SECCOMP_IOCTL_NOTIF_RECV, &request)) {
				if (errno == EINTR || errno == ENOENT)
					continue;
				break;
			}
			struct seccomp_notif_resp response = { .id = request.id };
			if (armed) {
				response.error = -ENOMEM;
				denied++;
			} else {
				response.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
				continued++;
			}
			if (ioctl(notifications, SECCOMP_IOCTL_NOTIF_SEND, &response))
				break;
		}
	}
done:
	if (result)
		perror("checkpoint host-fault supervisor");
	if (child > 0 && !reaped) {
		kill(child, SIGKILL);
		while (waitpid(child, NULL, 0) < 0 && errno == EINTR) {}
	}
	int leftovers = cleanup();
	if (leftovers)
		result = 1;
	fprintf(stderr, "checkpoint host-fault supervisor: armed=%d continued=%u denied=%u status=%d leftovers=%d\n",
		armed, continued, denied, status, leftovers);
	unlink(argv[1]);
	return result;
}
