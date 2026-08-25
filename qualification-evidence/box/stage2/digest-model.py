#!/usr/bin/env python3
"""Recover the work count a landing digest names, including the ones the record does not.

The ae3 payload is `mov ecx,300000 ; 1: dec ecx ; jnz 1b ; hlt` and the digest is
FNV-1a over RIP and RCX. Two stop points are reachable, and they say different things:

  RIP 0x1006, RCX = 300000 - work      the guest is at the `dec` having just retired the
                                       `jnz` that made the work count - a single-step landing
  RIP 0x1008, RCX = 300000 - work - 1  the guest is at the `jnz` having just retired the
                                       `dec` of the next iteration - an overflow stop

The model is fitted, not assumed: it is required to reproduce five digests whose work
count the records state outright before it is used on any digest whose work count they
do not.
"""
import struct, sys

N = 300000

def fnv(rip, rcx):
    h = 1469598103934665603
    for b in struct.pack("<QQ", rip, rcx & 0xFFFFFFFFFFFFFFFF):
        h ^= b
        h = (h * 1099511628211) & 0xFFFFFFFFFFFFFFFF
    return "%016x" % h

def digest_of(work, stop):
    return fnv(0x1006, N - work) if stop == "step" else fnv(0x1008, N - work - 1)

# Validation set, all read straight out of the campaign records.
KNOWN_STEP = {69772: "e786e00868728ddb", 66045: "6eed2b411ca5809a",
              67121: "670010d8e4dcfc6a", 85981: "cf13e37119f51f29",
              27325: "1a590d234b78c64d"}
KNOWN_OVERFLOW = {107384: "5e023e578a2029be", 38949: "e412b3211abf7bc3"}

bad = [w for w, d in KNOWN_STEP.items() if digest_of(w, "step") != d]
bad += [w for w, d in KNOWN_OVERFLOW.items() if digest_of(w, "overflow") != d]
if bad:
    print("model rejected at work counts", bad)
    sys.exit(1)
print(f"model reproduces all {len(KNOWN_STEP) + len(KNOWN_OVERFLOW)} digests whose work "
      f"count the records state")

table = {}
for w in range(0, N + 1):
    for stop in ("step", "overflow"):
        table.setdefault(digest_of(w, stop), []).append((w, stop))
dupes = sum(1 for v in table.values() if len(v) > 1)
print(f"dictionary: {len(table)} digests over {2*(N+1)} work-count/stop-point pairs, "
      f"{dupes} of them ambiguous")

for line in sys.stdin:
    line = line.split("#")[0].strip()
    if not line:
        continue
    label, target, period, digest = line.split()
    target, period = int(target), int(period)
    hits = table.get(digest, [])
    if not hits:
        print(f"{label}: digest {digest} is not any reachable landing")
        continue
    for w, stop in hits:
        print(f"{label}: target {target}, period {period} -> stopped at work {w} "
              f"({stop}), {w - target:+d} from the target, skid {w - period}")
