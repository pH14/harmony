# SPDX-License-Identifier: AGPL-3.0-or-later
# Built ON the box (native x86_64 Linux). See spikes/amd-epyc/README.md.
CC      ?= cc
CFLAGS  ?= -O2 -g -Wall -Wextra -std=gnu11
BINS     = amd-hammer ae0-probe kvm-guest-hammer singlestep-driver ae4-freeze ae4-msr \
           ae3-forceexit ae5-gate

all: $(BINS)

ae4-freeze: ae4-freeze.c
	$(CC) $(CFLAGS) -o $@ ae4-freeze.c

ae4-msr: ae4-msr.c
	$(CC) $(CFLAGS) -o $@ ae4-msr.c

# AE-3 in-kernel force-exit + exact landing (needs the patched 6.18.35 kernel to RUN,
# but must COMPILE from a fresh checkout — P1-4).
ae3-forceexit: ae3-forceexit.c
	$(CC) $(CFLAGS) -o $@ ae3-forceexit.c

# AE-5 mechanism-integrated same-seed gate (patched-kernel run; compile-checked here).
ae5-gate: ae5-gate.c
	$(CC) $(CFLAGS) -o $@ ae5-gate.c

amd-hammer: amd-hammer.c ../payloads/oracles.h
	$(CC) $(CFLAGS) -o $@ amd-hammer.c

ae0-probe: ae0-probe.c
	$(CC) $(CFLAGS) -o $@ ae0-probe.c

kvm-guest-hammer: kvm-guest-hammer.c ../payloads/oracles.h
	$(CC) $(CFLAGS) -o $@ kvm-guest-hammer.c

singlestep-driver: singlestep-driver.c
	$(CC) $(CFLAGS) -o $@ singlestep-driver.c

clean:
	rm -f $(BINS) *.o

.PHONY: all clean
