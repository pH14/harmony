# SPDX-License-Identifier: AGPL-3.0-or-later
CC ?= cc
CFLAGS ?= -O2
BUILD_DIR ?= build
WARNINGS := -Wall -Wextra -Werror -Wconversion -Wpedantic
LIB := $(BUILD_DIR)/libvoidstar.so
ABI_TEST := $(BUILD_DIR)/abi-test
EVENT_KILL_TEST := $(BUILD_DIR)/event-kill-test
EVENT_GATE_TEST := $(BUILD_DIR)/event-gate-test
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

$(EVENT_KILL_TEST): tests/event_kill_test.c voidstar.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/event_kill_test.c

$(EVENT_GATE_TEST): tests/event_gate_test.c voidstar.c | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/event_gate_test.c

check: $(LIB) $(ABI_TEST) $(EVENT_KILL_TEST) $(EVENT_GATE_TEST)
	$(ABI_TEST)
	$(EVENT_KILL_TEST)
	$(EVENT_GATE_TEST)
	./check-symbols.sh $(LIB)

clean:
	rm -rf $(BUILD_DIR)
