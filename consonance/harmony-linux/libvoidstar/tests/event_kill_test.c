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

int main(int argc, char **argv)
{
    int channel[2];
    pid_t child;
    uint64_t ordinal = 1;
    uint64_t acknowledgement = 0;
    int status;

    if (argc == 2) {
        for (;;)
            (void)notify_coverage(1);
    }

    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, channel) == 0);
    {
        char fd[32];

        assert(snprintf(fd, sizeof(fd), "%d", channel[1]) > 0);
        assert(setenv("HARMONY_EVENT_KILL_FD", fd, 1) == 0);
    }
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        (void)close(channel[0]);
        assert(setpgid(0, 0) == 0);
        assert(execl(argv[0], argv[0], "--child", (char *)NULL) == 0);
        _exit(127);
    }

    assert(unsetenv("HARMONY_EVENT_KILL_FD") == 0);
    (void)close(channel[1]);
    assert(write(channel[0], &ordinal, sizeof(ordinal)) == (ssize_t)sizeof(ordinal));
    assert(read(channel[0], &acknowledgement, sizeof(acknowledgement)) ==
           (ssize_t)sizeof(acknowledgement));
    assert(acknowledgement == ordinal);
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status));
    assert(WTERMSIG(status) == SIGKILL);
    (void)close(channel[0]);
    return 0;
}
