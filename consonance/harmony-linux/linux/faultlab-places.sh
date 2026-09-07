#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Print the places a park may name in a workload binary: the address of every
# row of its DWARF line table, one hex address per line, sorted and unique.
# A row starts an instruction that begins a source line, so the list covers
# the program at source-line grain and follows from the binary alone; the
# searcher reads it with --places. The binary must carry its line table
# (built with -g); the copy the image ships may be stripped, since stripping
# moves no code.
#
# usage: faultlab-places.sh <elf> > <places>
set -euo pipefail

if [ $# -ne 1 ] || [ ! -r "$1" ]; then
    echo "usage: faultlab-places.sh <elf>" >&2
    exit 2
fi

# decodedline rows: "<file> <line> <address> [view] [flags]"; the address is
# the first 0x field. Rows whose address is 0 are for discarded code.
readelf --debug-dump=decodedline "$1" \
    | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^0x[0-9a-fA-F]+$/) { print tolower($i); break } }' \
    | grep -v '^0x0$' \
    | sort -u
