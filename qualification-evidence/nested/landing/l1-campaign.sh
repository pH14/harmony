#!/bin/sh
# The landing campaign inside a virtual machine, paired with the metal control:
# same binary, same seeds, same arm counts, so targets and landed digests compare
# one to one.
O=/share/out-landing
mkdir -p $O
echo "kernel: $(uname -r)  cpus: $(nproc)"
date -u +%FT%TZ

echo "--- A: 5000 arms, margin 16192, replay ---"
s=$(date +%s%N)
/share/ae3-forceexit --core 0 --event 0x5100d1 --margin 16192 \
    --arms 5000 --seed 11 --replay --out $O/A-margin16192-replay.json
echo "A rc=$?"
e=$(date +%s%N); echo "A wall=$(( (e-s)/1000000 )) ms"

echo "--- B: 2000 arms, margin 3072, tail probe ---"
s=$(date +%s%N)
/share/ae3-forceexit --core 0 --event 0x5100d1 --margin 3072 \
    --arms 2000 --seed 11 --out $O/B-margin3072.json
echo "B rc=$?"
e=$(date +%s%N); echo "B wall=$(( (e-s)/1000000 )) ms"

date -u +%FT%TZ
echo "=== L1 campaign done ==="
