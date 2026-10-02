# syntax=docker/dockerfile:1
# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:trixie-slim@sha256:1caf1c703c8f7e15dcf2e7769b35000c764e6f50e4d7401c355fb0248f3ddfdb
FROM ${DEBIAN_IMAGE} AS build
SHELL ["/bin/sh", "-euxc"]
RUN apt-get update \
    && apt-get install --yes --no-install-recommends clang make binutils \
    && rm -rf /var/lib/apt/lists/*
COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
COPY workloads/faults/runtime/Makefile \
     workloads/faults/runtime/fault_runtime.c \
     workloads/faults/runtime/fault_runtime.h \
     workloads/faults/runtime/fault_runtime_shim.c /build/fault-runtime/
RUN make -C /build/fault-runtime CORE_DIR=/build/libvoidstar BUILD_DIR=/build/lib all \
    && clang -c -O2 -g -std=gnu11 -Wall -Wextra -Werror -D_GNU_SOURCE -pthread \
       /build/libvoidstar/voidstar.c -o /build/voidstar.o \
    && clang -c -O2 -g -std=gnu11 -Wall -Wextra -Werror -D_GNU_SOURCE -pthread \
       /build/fault-runtime/fault_runtime.c -o /build/fault_runtime.o \
    && clang -c -O2 -g -std=gnu11 -Wall -Wextra -Werror -D_GNU_SOURCE -pthread \
       /build/fault-runtime/fault_runtime_shim.c -o /build/fault_runtime_shim.o \
    && ar rcs /build/lib/libvoidstar.a /build/voidstar.o /build/fault_runtime.o /build/fault_runtime_shim.o
COPY workloads/bugs/interleaving.h /build/interleaving.h
COPY workloads/bugs/category/stale-lease/stale_lease.c /build/stale_lease.c
RUN clang -c -O1 -g -std=gnu11 -Wall -Wextra -Werror -fsanitize-coverage=trace-pc-guard \
        /build/stale_lease.c -o /build/workload.o \
    && clang -static -g -pthread /build/workload.o -o /build/stale-lease \
        -L/build/lib -Wl,--whole-archive -lvoidstar -Wl,--no-whole-archive -Wl,--build-id \
    && mkdir -p /out/symbols /out/data \
    && nm --defined-only /build/stale-lease \
       | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/stale-lease.sym.tsv \
    && test -s /out/symbols/stale-lease.sym.tsv \
    && sha256sum /build/stale-lease \
       | awk '{print $1 "  /usr/local/bin/stale-lease"}' > /out/symbols/harmony-instrumented-events
FROM scratch
COPY --from=build /build/stale-lease /usr/local/bin/stale-lease
COPY --from=build /build/lib/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=build /out/symbols /symbols
COPY --from=build /out/data /data
COPY workloads/bugs/category/stale-lease/image/bundle /etc/harmony/bundle
CMD ["/usr/local/bin/stale-lease"]
