// SPDX-License-Identifier: AGPL-3.0-or-later
// Probe: does a 10 ms nanosleep wake up when harmony-pvclock is the active
// clocksource? The fault agent's tick loop depends on the answer; a guest
// whose timer never fires must self-clock off the pvclock page instead.
// Every line carries the "clocksource" substring so the box gate that filters
// guest serial by that word surfaces the probe output.
#include <stdio.h>
#include <time.h>
#include <unistd.h>
#include <sys/reboot.h>

int main(void) {
    struct timespec a, b, req = {0, 10000000};
    printf("clocksource-probe: start\n");
    fflush(stdout);
    for (int i = 0; i < 5; i++) {
        clock_gettime(CLOCK_MONOTONIC, &a);
        int r = nanosleep(&req, NULL);
        clock_gettime(CLOCK_MONOTONIC, &b);
        long long ns = (b.tv_sec - a.tv_sec) * 1000000000LL + (b.tv_nsec - a.tv_nsec);
        printf("clocksource-probe: iter=%d rc=%d elapsed_ns=%lld\n", i, r, ns);
        fflush(stdout);
    }
    printf("clocksource-probe: done GUEST_READY\n");
    fflush(stdout);
    sync();
    reboot(RB_POWER_OFF);
    return 0;
}
