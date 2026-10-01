// SPDX-License-Identifier: AGPL-3.0-or-later
#include <net/if.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>

int main(void) {
    struct ifreq request;
    memset(&request, 0, sizeof request);
    strcpy(request.ifr_name, "lo");
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0 || ioctl(fd, SIOCGIFFLAGS, &request) < 0) {
        return 1;
    }
    request.ifr_flags |= IFF_UP;
    return ioctl(fd, SIOCSIFFLAGS, &request) < 0;
}
