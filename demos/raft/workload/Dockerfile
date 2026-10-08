# SPDX-License-Identifier: AGPL-3.0-or-later
ARG BASE=docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM ${BASE} AS build
RUN apt-get update && apt-get install -y --no-install-recommends clang gcc libc6-dev make binutils && rm -rf /var/lib/apt/lists/*
COPY consonance/harmony-linux/libvoidstar /build/libvoidstar
COPY workloads/faults/runtime /build/fault-runtime
COPY workloads/languages/vendor /build/vendor
COPY demos/raft/workload/raft.c /build/raft.c
COPY demos/raft/workload/tests /build/tests
RUN clang -O1 /build/tests/identity.c -o /tmp/identity-test && /tmp/identity-test
RUN make -C /build/fault-runtime CORE_DIR=/build/libvoidstar BUILD_DIR=/out all
RUN printf '#include "vendor/antithesis_instrumentation.h"\n' > /build/shim.c && clang -O1 -g -fsanitize-coverage=trace-pc-guard -c /build/raft.c -o /build/raft.o && clang -O1 -g -c /build/shim.c -o /build/shim.o && clang /build/raft.o /build/shim.o -pthread -ldl -Wl,--build-id -o /out/control && mkdir -p /out/symbols && cp /out/control /out/symbols/control && nm --defined-only /out/control | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > /out/symbols/raft.sym.tsv && sha256sum /out/control | awk '{print $1 "  /app/control"}' > /out/symbols/harmony-instrumented-events
FROM ${BASE} AS system
FROM scratch
COPY --from=system /lib/x86_64-linux-gnu/libc.so.6 /lib/x86_64-linux-gnu/libc.so.6
COPY --from=system /lib/x86_64-linux-gnu/libselinux.so.1 /lib/x86_64-linux-gnu/libselinux.so.1
COPY --from=system /lib/x86_64-linux-gnu/libpcre2-8.so.0 /lib/x86_64-linux-gnu/libpcre2-8.so.0
COPY --from=system /lib64/ld-linux-x86-64.so.2 /lib64/ld-linux-x86-64.so.2
COPY --from=system /bin/dash /bin/sh
COPY --from=system /usr/bin/cat /usr/bin/ls /usr/bin/touch /usr/bin/mkdir /usr/bin/rm /usr/bin/sleep /usr/bin/head /usr/bin/tail /usr/bin/
COPY --from=system /etc/passwd /etc/group /etc/
COPY --from=system /usr/share/doc/libc6/copyright /licenses/libc6.txt
COPY --from=system /usr/share/doc/coreutils/copyright /licenses/coreutils.txt
COPY --from=system /usr/share/doc/dash/copyright /licenses/dash.txt
ENV PATH=/usr/bin:/bin
WORKDIR /tmp
COPY --from=build /out/control /app/control
COPY --from=build /out/libvoidstar.so /usr/lib/libvoidstar.so
COPY --from=build /out/symbols /symbols
COPY demos/raft/workload/bundle /etc/harmony/bundle
COPY demos/raft/workload/raft.c /app/raft.c
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt
CMD ["/app/control","status"]
