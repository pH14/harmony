// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../voidstar.c"

static volatile sig_atomic_t active_child;

static void timeout(int signal_number)
{
    (void)signal_number;
    if (active_child > 0)
        (void)kill(-(pid_t)active_child, SIGKILL);
    _exit(124);
}

static void run_child(void)
{
    int start_fd = event_fd_from_environment("HARMONY_EVENT_TEST_START_FD");
    uint64_t edge = 1;
    unsigned char byte = 1;

    assert(start_fd >= 0);
    assert(write(start_fd, &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    assert(read(start_fd, &byte, sizeof(byte)) == (ssize_t)sizeof(byte));

    for (;;)
        (void)notify_coverage(edge++);
}

static void check_ordinal(const char *executable, uint64_t ordinal)
{
    int channel[2];
    int report_channel[2];
    int start_channel[2];
    pid_t child;
    uint64_t acknowledgement = 0;
    uint64_t report[2] = {0, 0};
    unsigned char byte = 0;
    int status;

    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channel) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report_channel) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, start_channel) == 0);
    {
        char fd[32];

        assert(snprintf(fd, sizeof(fd), "%d", channel[1]) > 0);
        assert(setenv("HARMONY_EVENT_KILL_FD", fd, 1) == 0);
        assert(snprintf(fd, sizeof(fd), "%d", report_channel[1]) > 0);
        assert(setenv("HARMONY_EVENT_REPORT_FD", fd, 1) == 0);
        assert(snprintf(fd, sizeof(fd), "%d", start_channel[1]) > 0);
        assert(setenv("HARMONY_EVENT_TEST_START_FD", fd, 1) == 0);
    }
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        (void)close(channel[0]);
        (void)close(report_channel[0]);
        (void)close(start_channel[0]);
        assert(setpgid(0, 0) == 0);
        assert(execl(executable, executable, "--child", (char *)NULL) == 0);
        _exit(127);
    }
    active_child = (sig_atomic_t)child;
    (void)alarm(10);

    assert(unsetenv("HARMONY_EVENT_KILL_FD") == 0);
    assert(unsetenv("HARMONY_EVENT_REPORT_FD") == 0);
    assert(unsetenv("HARMONY_EVENT_TEST_START_FD") == 0);
    (void)close(channel[1]);
    (void)close(report_channel[1]);
    (void)close(start_channel[1]);
    assert(read(start_channel[0], &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    assert(write(channel[0], &ordinal, sizeof(ordinal)) == (ssize_t)sizeof(ordinal));
    assert(read(channel[0], &acknowledgement, sizeof(acknowledgement)) ==
           (ssize_t)sizeof(acknowledgement));
    assert(acknowledgement == ordinal);
    assert(write(start_channel[0], &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status));
    assert(WTERMSIG(status) == SIGKILL);
    assert(read(report_channel[0], report, sizeof(report)) == (ssize_t)sizeof(report));
    assert(report[0] == ordinal);
    assert(report[1] == ordinal);
    (void)close(channel[0]);
    (void)close(report_channel[0]);
    (void)close(start_channel[0]);
    (void)alarm(0);
    active_child = 0;
}

int main(int argc, char **argv)
{
    static const uint64_t ordinals[] = {1, 8, 32};
    size_t index;

    assert(signal(SIGALRM, timeout) != SIG_ERR);
    if (argc == 2)
        run_child();
    for (index = 0; index < sizeof(ordinals) / sizeof(ordinals[0]); ++index)
        check_ordinal(argv[0], ordinals[index]);
    return 0;
}
