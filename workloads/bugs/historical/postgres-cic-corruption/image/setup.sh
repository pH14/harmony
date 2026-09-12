#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs once inside the container rootfs before any node starts. The host has
# already mounted and bound /proc, /sys and /dev; everything below is this
# workload's own.
set -eu

# Node and hook output goes to files under /run, and PostgreSQL puts its unix
# socket in /tmp. Both are tmpfs so nothing a hook writes lands on the cluster's
# storage and perturbs what the search is exploring.
mkdir -p /tmp /run /dev/shm
mount -t tmpfs none /tmp
mount -t tmpfs none /run
# PostgreSQL allocates its main shared memory as a POSIX segment, which needs a
# real /dev/shm; the bound-in /dev does not provide one.
mount -t tmpfs none /dev/shm
# A fresh tmpfs is mode 755, which would leave the non-root node unable to
# create its socket.
chmod 1777 /tmp /run /dev/shm

# The PostgreSQL 14 statistics collector opens a UDP socket on loopback and
# logs a failure when it cannot; bringing lo up keeps that line off the serial.
ip link set lo up
