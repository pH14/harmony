# SPDX-License-Identifier: AGPL-3.0-or-later
CC ?= cc
CFLAGS ?= -O2
BUILD_DIR ?= build
WARNINGS := -Wall -Wextra -Werror -Wconversion -Wpedantic
LIB := $(BUILD_DIR)/libvoidstar.so
ABI_TEST := $(BUILD_DIR)/abi-test
UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
EXPORT_FLAGS := -dynamiclib
else
EXPORT_FLAGS := -shared -Wl,--version-script=libvoidstar.map
endif

.PHONY: all check clean

all: $(LIB)

$(BUILD_DIR):
	mkdir -p $@

$(LIB): voidstar.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -fPIC $(EXPORT_FLAGS) -pthread -o $@ $<

$(ABI_TEST): tests/abi_test.c voidstar.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/abi_test.c

check: $(LIB) $(ABI_TEST)
	$(ABI_TEST)
	./check-symbols.sh $(LIB)

clean:
	rm -rf $(BUILD_DIR)
