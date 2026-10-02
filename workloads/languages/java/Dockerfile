# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
ARG BOOT_JDK_IMAGE=eclipse-temurin:21-jdk-jammy@sha256:6adefddd4a20bceef702cedb0b03952fcd7691f9c7ccffe27014992abc0b46bd
FROM ${BOOT_JDK_IMAGE} AS boot-jdk
FROM ${DEBIAN_IMAGE} AS compiled
ENV SOURCE_DATE_EPOCH=1778371200
ARG JDK_TAG=jdk-21.0.12.1-ga
ARG JDK_SHA256=9c719670e7be6080a46ebeb1223a375bd53818ba7c5ae11f11a02907b593775f
RUN apt-get update && apt-get install --yes --no-install-recommends \
    clang make autoconf file zip unzip patch ca-certificates curl binutils libc6-dev \
    libffi-dev libasound2-dev libcups2-dev libfontconfig1-dev \
    libx11-dev libxext-dev libxrender-dev libxrandr-dev libxtst-dev libxt-dev \
    && rm -rf /var/lib/apt/lists/*
COPY --from=boot-jdk /opt/java/openjdk /opt/boot-jdk
COPY workloads/languages/vendor /build/vendor
COPY workloads/languages/c/shim.c /build/c/shim.c
COPY workloads/languages/java/zero-code-cache.patch /build/zero-code-cache.patch
RUN curl --fail --silent --show-error --location "https://codeload.github.com/openjdk/jdk21u/tar.gz/refs/tags/${JDK_TAG}" -o /build/jdk.tar.gz \
    && echo "${JDK_SHA256}  /build/jdk.tar.gz" | sha256sum --check --strict \
    && mkdir -p /build/jdk \
    && tar -xzf /build/jdk.tar.gz -C /build/jdk --strip-components=1 \
    && rm /build/jdk.tar.gz \
    && patch --batch --forward --fuzz=0 -d /build/jdk -p1 < /build/zero-code-cache.patch \
    && clang -O1 -g -fPIC -c /build/c/shim.c -o /build/shim.o \
    && ar rcs /build/libharmony-forwarding.a /build/shim.o
WORKDIR /build/jdk
RUN bash configure --with-boot-jdk=/opt/boot-jdk --with-build-jdk=/opt/boot-jdk \
        --with-jvm-variants=zero --with-toolchain-type=clang \
        --with-debug-level=release --with-native-debug-symbols=internal --enable-headless-only \
        --with-zlib=bundled --disable-warnings-as-errors --with-source-date=${SOURCE_DATE_EPOCH} \
        --with-version-opt=harmony \
        --with-extra-cflags='-fsanitize-coverage=trace-pc-guard' \
        --with-extra-cxxflags='-fsanitize-coverage=trace-pc-guard' \
        --with-extra-ldflags='-fno-sanitize-link-runtime -Wl,--undefined=__sanitizer_cov_trace_pc_guard_init /build/libharmony-forwarding.a -ldl' \
    && make java.base-jmod \
    && /opt/boot-jdk/bin/jlink --module-path build/linux-*/images/jmods --add-modules java.base --output /opt/java \
        --no-header-files --no-man-pages --strip-java-debug-attributes \
    && /opt/java/bin/java -version 2>&1 | grep -q 'Zero VM'
COPY workloads/languages/java/Fixture.java /build/fixture/Fixture.java
RUN mkdir -p /out/fixture /out/symbols/native /out/licenses \
    && /opt/boot-jdk/bin/javac --release 21 -d /out/fixture /build/fixture/Fixture.java \
    && printf '#!/bin/sh\nexec /opt/java/bin/java -cp /opt/harmony/java Fixture "$@"\n' > /out/fixture-launcher \
    && chmod +x /out/fixture-launcher \
    && cp /build/jdk/LICENSE /out/licenses/openjdk.txt \
    && cp /build/jdk/ASSEMBLY_EXCEPTION /out/licenses/openjdk-assembly-exception.txt \
    && find /opt/java -type f -print | LC_ALL=C sort | while IFS= read -r file; do \
        if [ "$(head -c 4 "$file" | od -An -c | tr -d ' ')" = '177ELF' ]; then \
            destination="/out/symbols/native${file}"; mkdir -p "$(dirname "$destination")"; cp "$file" "$destination"; \
            nm --defined-only "$file" | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > "$destination.sym.tsv"; \
            strip --strip-debug "$file"; \
            sha256sum "$file" >> /out/symbols/harmony-instrumented-events; \
        fi; \
       done
FROM ${DEBIAN_IMAGE} AS language-base
RUN apt-get update && apt-get install --yes --no-install-recommends libffi8 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=compiled /opt/java /opt/java
COPY --from=compiled /out/fixture /opt/harmony/java
COPY --from=compiled /out/fixture-launcher /opt/harmony/fixture
COPY --from=compiled /out/symbols /symbols
COPY --from=compiled /out/licenses /licenses
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt
CMD ["/opt/harmony/fixture"]
