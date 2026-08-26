#!/bin/bash
# Paired metal control for the nested landing campaign: same binary, same seeds,
# same arm counts, so targets and landed digests compare one to one.
set -uo pipefail
O=/root/qual-evidence/nested-landing/metal
mkdir -p "$O"
H=/root/spike/spikes/amd-epyc/harness
cd "$H"
{
  echo "=== metal landing campaign ==="
  date -u +%FT%TZ
  echo "--- A: 5000 arms, margin 16192, replay ---"
  s=$(date +%s%N)
  taskset -c 3 ./ae3-forceexit --core 3 --event 0x5100d1 --margin 16192 \
      --arms 5000 --seed 11 --replay --out "$O/A-margin16192-replay.json"
  echo "A rc=$?"
  e=$(date +%s%N); echo "A wall=$(( (e-s)/1000000 )) ms"
  echo "--- B: 2000 arms, margin 3072, tail probe ---"
  s=$(date +%s%N)
  taskset -c 3 ./ae3-forceexit --core 3 --event 0x5100d1 --margin 3072 \
      --arms 2000 --seed 11 --out "$O/B-margin3072.json"
  echo "B rc=$?"
  e=$(date +%s%N); echo "B wall=$(( (e-s)/1000000 )) ms"
  date -u +%FT%TZ
  echo "=== metal done ==="
} > "$O/metal.log" 2>&1
