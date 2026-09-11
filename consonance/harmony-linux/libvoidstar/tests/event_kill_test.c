// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

/*
 * Make one site hot and leave a second untouched before the arm arrives, then
 * visit both forever. Which of the two the kill lands on is the whole meaning
 * of a rarity: the hot site is the one a run reaches constantly, and the rare
 * one is the state a crash has to catch.
 */
enum {
    TEST_HOT_EDGE = 1,
    TEST_RARE_EDGE = 2,
    TEST_WARMUP_VISITS = 64
};

static void run_child(void)
{
    int start_fd = event_fd_from_environment("HARMONY_EVENT_TEST_START_FD");
    unsigned char byte = 1;
    size_t index;

    assert(start_fd >= 0);
    /* The automatic yield reaches for a device this test does not provide and
       kills the process group when it cannot. This test makes far more
       callbacks than the yield cadence allows, and the cadence is not what it
       measures. */
    atomic_store_explicit(
        &harmony_automatic_coverage_enabled, false, memory_order_release);
    (void)init_coverage_module(64, "event-kill-test.sym.tsv");
    for (index = 0; index < TEST_WARMUP_VISITS; ++index)
        (void)notify_coverage(TEST_HOT_EDGE);
    assert(write(start_fd, &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    assert(read(start_fd, &byte, sizeof(byte)) == (ssize_t)sizeof(byte));

    for (;;) {
        (void)notify_coverage(TEST_HOT_EDGE);
        (void)notify_coverage(TEST_RARE_EDGE);
    }
}

/*
 * Spawn a gated child on its own event control, report and start channels.
 * The child warms one site and blocks until `start_channel[0]` is written.
 */
static pid_t start_child(const char *executable, int *channel, int *report_channel,
                         int *start_channel)
{
    pid_t child;
    unsigned char byte = 0;

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
        assert(execl(executable, executable, "--launcher", (char *)NULL) == 0);
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
    return child;
}

/* Send one command and read back the whole echo the library owes it. */
static void exchange(int fd, const uint64_t *command, uint64_t *acknowledgement)
{
    size_t bytes = HARMONY_EVENT_CMD_WORDS * sizeof(uint64_t);

    assert(write(fd, command, bytes) == (ssize_t)bytes);
    assert(read(fd, acknowledgement, bytes) == (ssize_t)bytes);
}

static void check_rarity(const char *executable, uint64_t rarity, uint64_t fired_edge)
{
    int channel[2];
    int report_channel[2];
    int start_channel[2];
    pid_t child;
    uint64_t command[HARMONY_EVENT_CMD_WORDS] = {HARMONY_EVENT_CMD_KILL, rarity, 1};
    uint64_t acknowledgement[HARMONY_EVENT_CMD_WORDS] = {0, 0, 0};
    uint64_t report[2] = {0, 0};
    unsigned char byte = 0;
    int status;

    child = start_child(executable, channel, report_channel, start_channel);
    exchange(channel[0], command, acknowledgement);
    assert(acknowledgement[0] == HARMONY_EVENT_CMD_KILL);
    assert(acknowledgement[1] == rarity);
    assert(write(start_channel[0], &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status));
    assert(WTERMSIG(status) == SIGKILL);
    assert(read(report_channel[0], report, sizeof(report)) == (ssize_t)sizeof(report));
    assert(report[0] == rarity);
    assert(report[1] == fired_edge);
    (void)close(channel[0]);
    (void)close(report_channel[0]);
    (void)close(start_channel[0]);
    (void)alarm(0);
    active_child = 0;
}

/*
 * Rarity 0 is a real coordinate, so a disarm cannot be a zero rarity. The
 * third word carries the arm, and a zero there leaves the process running: the
 * child answers a later command, which it could not do if the disarm had armed
 * the rarest site instead.
 */
static void check_disarm(const char *executable)
{
    int channel[2];
    int report_channel[2];
    int start_channel[2];
    pid_t child;
    uint64_t arm[HARMONY_EVENT_CMD_WORDS] = {HARMONY_EVENT_CMD_KILL, 40, 1};
    uint64_t disarm[HARMONY_EVENT_CMD_WORDS] = {HARMONY_EVENT_CMD_KILL, 0, 0};
    uint64_t acknowledgement[HARMONY_EVENT_CMD_WORDS] = {0, 0, 0};
    uint64_t status_command[HARMONY_EVENT_CMD_WORDS] = {
        HARMONY_EVENT_CMD_PARK_STATUS, 0, 0};
    unsigned char byte = 0;
    int status;

    child = start_child(executable, channel, report_channel, start_channel);
    exchange(channel[0], arm, acknowledgement);
    exchange(channel[0], disarm, acknowledgement);
    assert(acknowledgement[0] == HARMONY_EVENT_CMD_KILL);
    assert(write(start_channel[0], &byte, sizeof(byte)) == (ssize_t)sizeof(byte));
    exchange(channel[0], status_command, acknowledgement);
    assert(acknowledgement[0] == HARMONY_EVENT_CMD_PARK_STATUS);
    assert(kill(-child, SIGKILL) == 0);
    assert(waitpid(child, &status, 0) == child);
    (void)close(channel[0]);
    (void)close(report_channel[0]);
    (void)close(start_channel[0]);
    (void)alarm(0);
    active_child = 0;
}

int main(int argc, char **argv)
{
    assert(signal(SIGALRM, timeout) != SIG_ERR);
    if (argc == 2) {
        if (strcmp(argv[1], "--child") == 0)
            run_child();
        if (strcmp(argv[1], "--launcher") == 0)
            assert(execl(argv[0], argv[0], "--child", (char *)NULL) == 0);
        abort();
    }
    /* Rarity 0 admits only a site no callback has reached, so it steps over
       the warmed site and lands on the other one. */
    check_rarity(argv[0], 0, TEST_RARE_EDGE);
    /* A rarity wider than the visit counter admits every site, so the kill
       lands on the first callback after the arm whatever its site. */
    check_rarity(argv[0], 40, TEST_HOT_EDGE);
    check_disarm(argv[0]);
    return 0;
}
