/* SPDX-License-Identifier: AGPL-3.0-or-later */

/*
 * A deliberately small Linux LD_PRELOAD interposer for the durable-journal
 * diagnostic.  It has one job: stop the process at the exact hard-link which
 * publishes the transaction named by HARMONY_JOURNAL_FINAL.  All other link
 * calls are delegated to libc unchanged.
 */

#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

typedef int (*link_fn)(const char *, const char *);
typedef int (*linkat_fn)(int, const char *, int, const char *, int);

/*
 * Resolve per call instead of caching an unsynchronized function pointer.
 * This diagnostic is deliberately not on a hot path, and a call-local lookup
 * keeps concurrent link/linkat calls free of a data race in the interposer.
 */
static link_fn resolve_link(void) {
    return (link_fn)dlsym(RTLD_NEXT, "link");
}

static linkat_fn resolve_linkat(void) {
    return (linkat_fn)dlsym(RTLD_NEXT, "linkat");
}

static int append_path(char *out, size_t capacity, const char *directory,
                       const char *path) {
    size_t directory_length = strlen(directory);
    const char *separator = directory_length > 0 && directory[directory_length - 1] == '/'
                                ? ""
                                : "/";
    int written = snprintf(out, capacity, "%s%s%s", directory, separator, path);
    return written >= 0 && (size_t)written < capacity;
}

/*
 * Compare the destination by its complete path.  Absolute paths are compared
 * directly.  Relative linkat paths are resolved against their exact directory
 * fd without canonicalizing or prefix-matching them; a path we cannot resolve
 * is simply left unintercepted, which the Python harness treats as a failure.
 */
static int target_matches(const char *path, int directory_fd) {
    const char *wanted = getenv("HARMONY_JOURNAL_FINAL");
    if (wanted == NULL || wanted[0] != '/' || path == NULL) {
        return 0;
    }
    if (path[0] == '/') {
        return strcmp(path, wanted) == 0;
    }

    char directory[PATH_MAX];
    ssize_t length;
    if (directory_fd == AT_FDCWD) {
        if (getcwd(directory, sizeof(directory)) == NULL) {
            return 0;
        }
    } else {
        char proc_path[64];
        int written = snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d",
                               directory_fd);
        if (written < 0 || (size_t)written >= sizeof(proc_path)) {
            return 0;
        }
        length = readlink(proc_path, directory, sizeof(directory) - 1);
        if (length < 0 || (size_t)length >= sizeof(directory)) {
            return 0;
        }
        directory[length] = '\0';
    }
    char candidate[PATH_MAX];
    return append_path(candidate, sizeof(candidate), directory, path) &&
           strcmp(candidate, wanted) == 0;
}

static int acknowledgement_fd(void) {
    const char *text = getenv("HARMONY_JOURNAL_ACK_FD");
    if (text == NULL || *text == '\0') {
        return -1;
    }
    errno = 0;
    char *end = NULL;
    long value = strtol(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || value < 0 || value > INT_MAX) {
        return -1;
    }
    return (int)value;
}

static int write_all(int fd, const char *bytes, size_t length) {
    while (length > 0) {
        ssize_t written = write(fd, bytes, length);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return 0;
        }
        bytes += written;
        length -= (size_t)written;
    }
    return 1;
}

/* The caller requires both an acknowledgement and an uncatchable kill. */
static void acknowledge_then_kill(const char *phase) {
    static const char prefix[] = "HARMONY_JOURNAL_INTERCEPT:";
    int fd = acknowledgement_fd();
    if (fd < 0 || !write_all(fd, prefix, sizeof(prefix) - 1) ||
        !write_all(fd, phase, strlen(phase)) || !write_all(fd, "\n", 1)) {
        _exit(125);
    }
    if (kill(getpid(), SIGKILL) != 0) {
        _exit(125);
    }
    _exit(125);
}

static const char *configured_phase(void) {
    const char *phase = getenv("HARMONY_JOURNAL_PHASE");
    if (phase == NULL || (strcmp(phase, "before") != 0 && strcmp(phase, "after") != 0)) {
        return NULL;
    }
    return phase;
}

int link(const char *oldpath, const char *newpath) {
    const char *phase = configured_phase();
    if (phase != NULL && target_matches(newpath, AT_FDCWD) && strcmp(phase, "before") == 0) {
        acknowledge_then_kill(phase);
    }
    link_fn delegate = resolve_link();
    if (delegate == NULL) {
        errno = ENOSYS;
        return -1;
    }
    int result = delegate(oldpath, newpath);
    if (result == 0 && phase != NULL && target_matches(newpath, AT_FDCWD) &&
        strcmp(phase, "after") == 0) {
        acknowledge_then_kill(phase);
    }
    return result;
}

int linkat(int olddirfd, const char *oldpath, int newdirfd, const char *newpath,
          int flags) {
    const char *phase = configured_phase();
    if (phase != NULL && target_matches(newpath, newdirfd) && strcmp(phase, "before") == 0) {
        acknowledge_then_kill(phase);
    }
    linkat_fn delegate = resolve_linkat();
    if (delegate == NULL) {
        errno = ENOSYS;
        return -1;
    }
    int result = delegate(olddirfd, oldpath, newdirfd, newpath, flags);
    if (result == 0 && phase != NULL && target_matches(newpath, newdirfd) &&
        strcmp(phase, "after") == 0) {
        acknowledge_then_kill(phase);
    }
    return result;
}
