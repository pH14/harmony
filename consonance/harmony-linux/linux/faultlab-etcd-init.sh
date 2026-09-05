#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# rdinit for the etcd bundle (bugs/historical/etcd-3.5-inconsistency).
set -u
. /faultlab-common.sh

faultlab_mount_base
# etcd's data dir lives on tmpfs so that a SIGKILL at a Moment loses exactly
# what the guest page cache had not yet handed to the backend, which is the
# window the consistent-index bug lives in.
mkdir -p /run/etcd

FAULTLAB_NODE=/w/etcd-node.sh
FAULTLAB_READY=/w/etcd-ready.sh
export FAULTLAB_NODE FAULTLAB_READY

hooks=$(faultlab_arg faultlab.hooks "")
if [ -n "$hooks" ]; then
    # shellcheck disable=SC2046  # the hook list is a deliberate word split
    faultlab_control /w/etcd-hooks.sh $(echo "$hooks" | tr ',' ' ')
fi
faultlab_agent /bundle/etcd
