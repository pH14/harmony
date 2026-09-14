// SPDX-License-Identifier: AGPL-3.0-or-later

#include "fault_runtime.h"

void harmony_instrumentation_event(uint64_t site)
{
    harmony_fault_runtime_event(site);
}
