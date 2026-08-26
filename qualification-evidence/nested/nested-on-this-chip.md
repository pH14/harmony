# Counting a nested guest on this chip

The determinism machinery can run inside a virtual machine on this chip, with one
change to the host's hypervisor. Without that change the count it depends on is wrong,
quietly and by a wide margin. The gap was in software, not in the silicon.

Measurements are in this directory. `verify/` holds the diagnosis, on the stock
arrangement. `fix/` holds the change and the result, including the same battery run
against both host module sets an hour apart.

## What a nested guest needs, and what it got

The machinery needs four things from whatever it runs on. Three of them survive nesting
untouched, measured with `ae9-l1probe.c` inside the virtual machine:

- **Counting its own execution.** A loop retiring 10,000,000 conditional branches counts
  10,000,002, the value bare metal gives, with exact differences across loop sizes.
- **The speculative lock map.** The control MSR `0xC0011020` cannot be read from a guest;
  KVM refuses it. rr's behavioural probe works instead, reporting zero speculative
  lock-map commits against 1,000,004 branches on the same run, identical to metal. The
  workaround itself is host state and is inherited, not reapplied.
- **Overflow delivery.** A counter armed to overflow every 100,000 branches over 4,000,000
  branches of work delivered 40 records, none lost and none suppressed.

The fourth is counting **the subject**, and it did not survive. The machinery opens a
counter filtered to guest execution on the thread that enters the guest, and reads it from
outside. Inside a virtual machine that filter did nothing.

## The cause

A guest asks for a filtered counter by setting GuestOnly or HostOnly in the event select,
bits 40 and 41. Two things happened to those bits. KVM's AMD counter emulation listed both
as reserved and stripped them before storing the guest's write, and the event it created to
back the counter was hardcoded to count all guest execution. So the request was discarded
and the counter it produced counted everything the virtual CPU ran: the subject, the kernel
running it, and that kernel's own user space.

Hardware cannot make the distinction on its own. GuestOnly is applied relative to `VMRUN`,
and a guest and any guest it runs in turn are inside the same `VMRUN`.

The deciding measurement retires 1,000,000 branches in the measuring process's own user
space, after the counter is enabled and before the guest is ever entered, then runs a
10,000-branch guest payload. On metal the counter reads 10,000. Before the change, inside a
virtual machine, it read about 1,023,400 — the user loop, the payload, and the usual
surplus, all added together (`verify/14-metal-hostwork.json`,
`verify/25-l1-hostwork.json`).

## The change

Patch `0009` in `consonance/vmm-backend/kvm-patches/patches/`. Two halves: keep the guest's
filter bits instead of stripping them, and honour them by stopping and starting the backing
event at the transitions into and out of the guest's own guest, which is the one place the
hypervisor can tell the two apart. The bits are still never handed to hardware — the config
passed to perf is masked as before — so they are information the hypervisor acts on rather
than a control the guest reaches through.

## The result

Same box, same guest image, same binaries, an hour apart, with only the two host modules
differing. Every figure is for a payload retiring exactly 10,000 conditional branches.

| measurement | before | after | bare metal |
|---|---|---|---|
| 20 repetitions, three payload sizes | offset 13,270, no two alike | exact, offset 0 | exact, offset 0 |
| empty payload, 50 repetitions | about 13,074 | 0, fifty times | 0, fifty times |
| 10,000 branches, 50 repetitions | varies, about 23,400 | 10000, fifty times | 10000 |
| 0 / 10 / 100 / 1000 guest exits | 13,029 / 17,120 / 54,331 / 429,765 over | exact at every exit count | exact |
| 1,000,000 host branches before the guest | counted in full | not counted | not counted |
| 1,000,000 host branches after the guest | counted in full | not counted | not counted |
| guest-only, host-only and unfiltered counters together | all three equal | complementary, sum exact | complementary |
| sampled instruction pointers | 128 of 322 in the hypervisor's own kernel | 1 of 59 | 2 of 188 |

The first row's before figure comes from the paired run; the rest come from the diagnosis
pass on an earlier boot, which is why their magnitudes differ by a few hundred. The paired
run is the one that holds everything else constant.

The complementary reading is the strongest single check. With the change, the guest-only
counter reads 10,000 while the host-only counter on the same run reads 1,013,792 and the
unfiltered counter reads 1,023,769, which is their sum to within the few counts it takes to
enable three counters in sequence. Before, all three returned the same number.

Records: `fix/ab/prepatch/` and `fix/ab/patched/` are the paired run; `fix/21-` through
`fix/26-` are the full battery inside the virtual machine; `fix/10-` through `fix/14-` are
the bare-metal control on the same modules, unchanged by the patch; `fix/repeat/` is a
second boot.

## What it costs

Stopping and starting the counter is work at every transition into and out of the guest's
guest. Priced by the paired run at 20 repetitions of a payload taking 1,000 guest exits
(`fix/30-transition-cost.txt`): 419 ms before, 499 ms after, against a 301 ms and 308 ms
baseline at zero exits. That is about 5.9 microseconds per round trip before and 9.5 after,
so the change adds roughly 3.6 microseconds to each one. A guest doing no exits pays
nothing measurable.

This matters for landing, which single-steps the last stretch to an exact count and takes
one transition per instruction. At the margin this chip's baseline seals, that is thousands
of transitions per landing.

## Not settled

- The deep tail of the skid distribution inside a virtual machine. The landing procedure
  itself has since been measured there and lands exactly on the same states as metal, with
  the same skid distribution through the 99.9th percentile; see
  `landing-in-a-virtual-machine.md`. The tail beyond a few thousand landings is not reached
  by those runs.
- The outer kernel is the patched build in every run. An unpatched outer kernel cannot be
  tested without a host reboot.
- The change alters what any guest sees from a filtered counter, not only this backend's.
  A guest asking to count only its own guest now gets that, which for a guest running no
  guest of its own is zero where it used to be everything. That matches hardware, and it is
  a change in what such a guest reads.
