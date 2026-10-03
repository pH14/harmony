// SPDX-License-Identifier: AGPL-3.0-or-later
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "debug") == 0) {
        int fd = open("/tmp/debug", O_CREAT | O_WRONLY, 0644);
        if (fd < 0) return 1;
        close(fd);
        return 0;
    }
    for (;;) {
        if (access("/tmp/debug", F_OK) == 0) {
            puts("application debug logging enabled");
            fflush(stdout);
        }
        usleep(1000);
    }
}
