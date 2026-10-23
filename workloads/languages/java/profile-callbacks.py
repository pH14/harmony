#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Make a diagnostic libvoidstar source copy without changing callback batching."""
import sys
from pathlib import Path

source = Path(sys.argv[1]).read_text()
source = source.replace("#include <unistd.h>", "#include <unistd.h>\n#include <stdio.h>\n#include <sys/mman.h>", 1)
anchor = "static pthread_mutex_t harmony_device_lock = PTHREAD_MUTEX_INITIALIZER;"
profile = r'''
static pthread_once_t java_profile_once = PTHREAD_ONCE_INIT;
static uint64_t *java_profile;

static void java_profile_init(void)
{
    char path[96];
    int fd;
    void *mapping;
    if (getenv("HARMONY_JAVA_COVERAGE_PROFILE") == NULL)
        return;
    (void)snprintf(path, sizeof(path), "/tmp/java-coverage.%ld", (long)getpid());
    fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0)
        abort();
    if (ftruncate(fd, 4 * (off_t)sizeof(uint64_t)) != 0)
        abort();
    mapping = mmap(NULL, 4 * sizeof(uint64_t), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    (void)close(fd);
    if (mapping == MAP_FAILED)
        abort();
    java_profile = mapping;
}

static void java_profile_add(unsigned int index, uint64_t value)
{
    (void)pthread_once(&java_profile_once, java_profile_init);
    if (java_profile != NULL)
        (void)__atomic_fetch_add(&java_profile[index], value, __ATOMIC_RELAXED);
}
'''
assert source.count(anchor) == 1
source = source.replace(anchor, anchor + profile)
anchor = "    put_u32(coverage.request, harmony_coverage.thread);"
assert source.count(anchor) == 1
source = source.replace(anchor, "    java_profile_add(3, 1);\n" + anchor)
anchor = """uint64_t harmony_coverage_add(uint64_t hits)
{
    if (harmony_coverage.thread == 0)
        harmony_coverage.thread = harmony_thread_id();
    coverage_count(hits);"""
replacement = """uint64_t harmony_coverage_add(uint64_t hits)
{
    uint64_t before = harmony_coverage.threshold;
    java_profile_add(0, 1);
    java_profile_add(1, hits);
    if (harmony_coverage.thread == 0)
        harmony_coverage.thread = harmony_thread_id();
    coverage_count(hits);
    if (harmony_coverage.threshold != before)
        java_profile_add(2, 1);"""
assert source.count(anchor) == 1
Path(sys.argv[2]).write_text(source.replace(anchor, replacement))
