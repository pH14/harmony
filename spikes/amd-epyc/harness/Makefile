# SPDX-License-Identifier: AGPL-3.0-or-later
# Built ON the box (native x86_64 Linux). See spikes/amd-epyc/README.md.
CC      ?= cc
CFLAGS  ?= -O2 -g -Wall -Wextra -std=gnu11
BINS     = amd-hammer ae0-probe kvm-guest-hammer singlestep-driver

all: $(BINS)

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
