/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <errno.h>
#include <stdint.h>

void *__wrap_sbrk(intptr_t increment)
{
    if (increment == 0)
        return (void *)(uintptr_t)16777216;
    errno = ENOMEM;
    return (void *)(intptr_t)-1;
}
