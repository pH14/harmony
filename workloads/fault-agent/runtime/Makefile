# SPDX-License-Identifier: AGPL-3.0-or-later
CC ?= cc
CFLAGS ?= -O2
BUILD_DIR ?= build
WARNINGS := -Wall -Wextra -Werror -Wconversion -Wpedantic
CORE_DIR ?= ../../../consonance/harmony-linux/libvoidstar
CORE := $(CORE_DIR)/voidstar.c
MAP := $(CORE_DIR)/libvoidstar.map
UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
EXPORT_FLAGS := -dynamiclib
else
EXPORT_FLAGS := -shared -Wl,--version-script=$(MAP)
endif

LIB := $(BUILD_DIR)/libvoidstar.so
EVENT_TEST := $(BUILD_DIR)/event-test
HOOK_TEST := $(BUILD_DIR)/hook-test
SITE_TEST := $(BUILD_DIR)/site-test

.PHONY: all check clean

all: $(LIB)

$(BUILD_DIR):
	mkdir -p $@

$(LIB): $(CORE) $(MAP) fault_runtime.c fault_runtime_shim.c fault_runtime.h | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -fPIC $(EXPORT_FLAGS) -pthread -o $@ \
		$(CORE) fault_runtime.c fault_runtime_shim.c

$(EVENT_TEST): tests/event_test.c fault_runtime.c fault_runtime_shim.c fault_runtime.h | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/event_test.c

$(HOOK_TEST): tests/hook_test.c $(LIB) fault_runtime.h | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/hook_test.c \
		-L$(BUILD_DIR) -lvoidstar -Wl,-rpath,$(abspath $(BUILD_DIR))

$(SITE_TEST): tests/site_test.c fault_runtime.c fault_runtime.h | $(BUILD_DIR)
	$(CC) $(CFLAGS) $(WARNINGS) -pthread -o $@ tests/site_test.c

check: $(LIB) $(EVENT_TEST) $(HOOK_TEST) $(SITE_TEST)
	$(EVENT_TEST)
	$(HOOK_TEST)
	$(SITE_TEST)

clean:
	rm -rf $(BUILD_DIR)
