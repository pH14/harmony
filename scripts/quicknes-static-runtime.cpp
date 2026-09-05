/* SPDX-License-Identifier: AGPL-3.0-or-later */

#include <stddef.h>
#include <stdlib.h>

static void *quicknes_alloc(size_t size)
{
	void *ptr;

	if (size == 0)
		size = 1;
	ptr = malloc(size);
	if (ptr == NULL)
		abort();
	return ptr;
}

void *operator new(size_t size) { return quicknes_alloc(size); }
void *operator new[](size_t size) { return quicknes_alloc(size); }
void operator delete(void *ptr) noexcept { free(ptr); }
void operator delete[](void *ptr) noexcept { free(ptr); }
void operator delete(void *ptr, size_t) noexcept { free(ptr); }
void operator delete[](void *ptr, size_t) noexcept { free(ptr); }

extern "C" void __cxa_pure_virtual(void) { abort(); }
