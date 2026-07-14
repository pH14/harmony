#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Apply the harmony guest-kernel source additions to a linux source tree.

Usage: apply-guest-patches.py <kernel-source-dir>

The first (and so far only) guest-kernel source change: the task-110
CONFIG_HARMONY_PVCLOCK clocksource (docs/PARAVIRT-CLOCK.md §3.1). Follows the
kvm-patches string-anchor discipline (consonance/vmm-backend/kvm-patches/
scripts/apply_patch.py): every edit is anchored on a unique existing line and
aborts loudly if the anchor is missing or non-unique — a drifted kernel tree
never silently diverges. Unlike the kvm applier this one is fully idempotent
(re-running on an already-patched tree is a no-op), because build-kernel.sh
re-runs it on the persistent extracted tree on every build.

Uses tabs exactly as the kernel sources do.
"""
import os
import shutil
import sys

if len(sys.argv) != 2:
    sys.exit("usage: apply-guest-patches.py <kernel-source-dir>")
KSRC = sys.argv[1]
HERE = os.path.dirname(os.path.abspath(__file__))


def read(p):
    with open(p, "r") as f:
        return f.read()


def write(p, s):
    with open(p, "w") as f:
        f.write(s)


def unique_line_index(path, s, needle):
    lines = s.splitlines(keepends=True)
    idx = [i for i, l in enumerate(lines) if needle in l]
    if len(idx) != 1:
        sys.exit(
            "FAIL %s: anchor %r count=%d (need 1) — the kernel tree drifted; "
            "re-anchor patches/apply-guest-patches.py" % (path, needle, len(idx))
        )
    return lines, idx[0]


def insert_after_line(path, needle, addition):
    """Insert `addition` after the unique line containing `needle`; no-op if
    the addition is already present (idempotence)."""
    full = os.path.join(KSRC, path)
    s = read(full)
    if addition in s:
        print("ok already %s" % path)
        return
    lines, i = unique_line_index(path, s, needle)
    lines.insert(i + 1, addition)
    write(full, "".join(lines))
    print("ok insert  %s (after %r)" % (path, needle))


def insert_before_line(path, needle, addition):
    """Insert `addition` before the unique line containing `needle`; no-op if
    the addition is already present (idempotence)."""
    full = os.path.join(KSRC, path)
    s = read(full)
    if addition in s:
        print("ok already %s" % path)
        return
    lines, i = unique_line_index(path, s, needle)
    lines.insert(i, addition)
    write(full, "".join(lines))
    print("ok insert  %s (before %r)" % (path, needle))


def copy_file(name, dest):
    """Copy a patch payload file into the tree; overwrite so an edited payload
    always wins (the build is the arbiter of staleness)."""
    src = os.path.join(HERE, name)
    full = os.path.join(KSRC, dest)
    if os.path.exists(full) and read(full) == read(src):
        print("ok already %s" % dest)
        return
    shutil.copyfile(src, full)
    print("ok copy    %s -> %s" % (name, dest))


# ---- the clocksource file ---------------------------------------------------
copy_file("harmony_pvclock.c", "arch/x86/kernel/harmony_pvclock.c")

# ---- arch/x86/kernel/Makefile: build it under its config symbol -------------
# Anchor: the KVM_GUEST object line (the kvmclock line — the shape this
# clocksource borrows). NOT a bare "kvmclock.o": 6.18.35 also carries a
# `CFLAGS_REMOVE_kvmclock.o = -pg` line, so that substring is non-unique.
insert_after_line(
    "arch/x86/kernel/Makefile",
    "obj-$(CONFIG_KVM_GUEST)",
    "obj-$(CONFIG_HARMONY_PVCLOCK)\t+= harmony_pvclock.o\n",
)

# ---- arch/x86/Kconfig: the config symbol ------------------------------------
# Anchor: the (unique) KVM_GUEST config block, inside the HYPERVISOR_GUEST
# section; the harmony entry goes immediately before it. `depends on
# X86_64 && PARAVIRT` keeps it valid wherever the section moves.
KCONFIG_BLOCK = (
    "config HARMONY_PVCLOCK\n"
    '\tbool "Harmony paravirt work-derived clock"\n'
    "\tdepends on X86_64 && PARAVIRT\n"
    "\tdefault n\n"
    "\thelp\n"
    "\t  Clocksource and sched_clock backed by the harmony deterministic\n"
    "\t  hypervisor's materialized work-derived clock page (a seqlock page\n"
    "\t  the host re-stamps at every virtual-time advance; the read path is\n"
    "\t  a page load, never a raw counter read). Inert unless the\n"
    "\t  harmony_pvclock kernel parameter is present AND the host accepts\n"
    "\t  the page registration over the hypercall doorbell; when active the\n"
    "\t  TSC is marked unstable so kernel timekeeping can never fall back\n"
    "\t  to raw RDTSC. Say N unless building the harmony determinism guest.\n"
    "\n"
)
insert_before_line("arch/x86/Kconfig", "config KVM_GUEST", KCONFIG_BLOCK)

print("harmony guest patches applied.")
