# SPDX-License-Identifier: AGPL-3.0-or-later
ARG DEBIAN_IMAGE=docker.io/library/debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171
FROM ${DEBIAN_IMAGE} AS compiled
ENV SOURCE_DATE_EPOCH=1778371200
ARG PYTHON_VERSION=3.14.5
ARG PYTHON_SHA256=7e32597b99e5d9a39abed35de4693fa169df3e5850d4c334337ffd6a19a36db6
ARG SDK_REVISION=41141de806c45dd76ab7febc1864e170a74050d8
ARG SDK_SHA256=4c2bd180905c0309f932d310aff36e24dd26f9859dae0bd2f66b27fc8dcf41b4
ARG SETUPTOOLS_SHA256=f36b47402ecde768dbfafc46e8e4207b4360c654f1f3bb84475f0a28628fb19c
ARG MARKUPSAFE_SHA256=722695808f4b6457b320fdc131280796bdceb04ab50fe1795cd540799ebe1698
RUN apt-get update && apt-get install --yes --no-install-recommends \
    clang make libc6-dev binutils ca-certificates curl xz-utils patch pkg-config libffi-dev zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
COPY workloads/languages/vendor /build/vendor
COPY workloads/languages/c/shim.c /build/c/shim.c
COPY workloads/languages/python/disable-codegen.patch /build/disable-codegen.patch
RUN curl --fail --silent --show-error --location "https://www.python.org/ftp/python/${PYTHON_VERSION}/Python-${PYTHON_VERSION}.tar.xz" -o /build/python.tar.xz \
    && echo "${PYTHON_SHA256}  /build/python.tar.xz" | sha256sum --check --strict \
    && mkdir -p /build/cpython \
    && tar -xf /build/python.tar.xz -C /build/cpython --strip-components=1 \
    && patch --batch --forward --fuzz=0 -d /build/cpython -p1 < /build/disable-codegen.patch \
    && clang -O1 -g -fPIC -c /build/c/shim.c -o /build/shim.o
WORKDIR /build/cpython
RUN CC=clang CFLAGS='-O1 -g -fsanitize-coverage=trace-pc-guard' LDFLAGS='-fno-sanitize-link-runtime /build/shim.o -ldl' \
    ./configure --prefix=/opt/python --enable-experimental-jit=no --disable-test-modules --with-ensurepip=install \
    || (cat config.log; exit 1)
RUN sed -i 's/^#define PY_HAVE_PERF_TRAMPOLINE 1$/\/\* #undef PY_HAVE_PERF_TRAMPOLINE \*\//' pyconfig.h \
    && make -j"$(nproc)" \
    && make install \
    && /opt/python/bin/python3.14 -c 'import sys; assert not sys._jit.is_available()'
COPY workloads/languages/python/fixture.py workloads/languages/python/coverage_edges.py workloads/languages/python/test_coverage_edges.py /build/recipe/
RUN apt-get update && apt-get install --yes --no-install-recommends python3-pip \
    && rm -rf /var/lib/apt/lists/*
RUN curl --fail --silent --show-error --location "https://codeload.github.com/antithesishq/antithesis-sdk-python/tar.gz/${SDK_REVISION}" -o /build/sdk.tar.gz \
    && echo "${SDK_SHA256}  /build/sdk.tar.gz" | sha256sum --check --strict \
    && mkdir -p /build/sdk /out/symbols /out/fixture /out/licenses \
    && tar -xf /build/sdk.tar.gz -C /build/sdk --strip-components=1 \
    && python3 -m pip --isolated install --no-index --no-deps --no-binary :all: --no-build-isolation \
        --target /opt/python/lib/python3.14/site-packages /build/sdk \
    && cmp /build/sdk/src/antithesis/_internal/coverage.py /opt/python/lib/python3.14/site-packages/antithesis/_internal/coverage.py \
    && cp /build/sdk/LICENSE /out/licenses/antithesis-sdk-python.txt \
    && cp /build/cpython/LICENSE /out/licenses/cpython.txt \
    && /opt/python/bin/python3.14 /build/recipe/test_coverage_edges.py \
    && cp /build/recipe/fixture.py /out/fixture/fixture.py \
    && /opt/python/bin/python3.14 /build/recipe/coverage_edges.py /out/fixture harmony-python /out/symbols/python.sym.tsv \
    && printf '#!/bin/sh\nexec /opt/python/bin/python3.14 /opt/harmony/python/fixture.py "$@"\n' > /out/fixture-launcher \
    && chmod +x /out/fixture-launcher
RUN curl --fail --silent --show-error --location https://files.pythonhosted.org/packages/18/5d/3bf57dcd21979b887f014ea83c24ae194cfcd12b9e0fda66b957c69d1fca/setuptools-80.9.0.tar.gz -o /build/setuptools.tar.gz \
    && echo "${SETUPTOOLS_SHA256}  /build/setuptools.tar.gz" | sha256sum --check --strict \
    && mkdir -p /build/setuptools \
    && tar -xf /build/setuptools.tar.gz -C /build/setuptools --strip-components=1 \
    && python3 -m pip --isolated install --no-index --no-deps --no-binary :all: --no-build-isolation \
        --target /opt/python/lib/python3.14/site-packages /build/setuptools \
    && curl --fail --silent --show-error --location https://files.pythonhosted.org/packages/7e/99/7690b6d4034fffd95959cbe0c02de8deb3098cc577c67bb6a24fe5d7caa7/markupsafe-3.0.3.tar.gz -o /build/markupsafe.tar.gz \
    && echo "${MARKUPSAFE_SHA256}  /build/markupsafe.tar.gz" | sha256sum --check --strict \
    && mkdir -p /build/markupsafe \
    && tar -xf /build/markupsafe.tar.gz -C /build/markupsafe --strip-components=1 \
    && CC=clang CFLAGS='-O1 -g -fsanitize-coverage=trace-pc-guard' LDFLAGS='-fno-sanitize-link-runtime -ldl' \
        /opt/python/bin/python3.14 -m pip --isolated install --verbose --no-index --no-deps --no-binary :all: --no-build-isolation /build/markupsafe \
    && /opt/python/bin/python3.14 -c 'from markupsafe import _speedups, escape; assert _speedups.__file__.endswith(".so"); assert escape("<&>") == "&lt;&amp;&gt;"' \
    && cp /build/markupsafe/LICENSE.txt /out/licenses/markupsafe.txt \
    && rm -rf /opt/python/lib/python3.14/test /opt/python/lib/python3.14/site-packages/pip /opt/python/lib/python3.14/ensurepip \
        /opt/python/lib/python3.14/site-packages/setuptools* /opt/python/lib/python3.14/site-packages/_distutils_hack \
        /opt/python/lib/python3.14/site-packages/distutils-precedence.pth \
    && find /opt/python -type d -name __pycache__ -prune -exec rm -rf {} + \
    && PYTHONDONTWRITEBYTECODE=1 /opt/python/bin/python3.14 -m compileall -q -f \
        --invalidation-mode checked-hash /opt/python/lib/python3.14
RUN mkdir -p /out/symbols/native \
    && find /opt/python -type f \( -name 'python3.14' -o -name '*.so' \) -print | LC_ALL=C sort > /out/native-paths \
    && while IFS= read -r file; do \
        destination="/out/symbols/native${file}"; mkdir -p "$(dirname "$destination")"; cp "$file" "$destination"; \
        nm --defined-only "$file" | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > "$destination.sym.tsv"; \
        strip --strip-debug "$file"; \
        sha256sum "$file" >> /out/symbols/harmony-instrumented-events; \
       done < /out/native-paths
FROM ${DEBIAN_IMAGE} AS language-base
RUN apt-get update && apt-get install --yes --no-install-recommends libffi8 zlib1g \
    && rm -rf /var/lib/apt/lists/*
COPY --from=compiled /opt/python /opt/python
RUN rm -rf /opt/python/include /opt/python/lib/libpython*.a /opt/python/lib/python3.14/config-*
COPY --from=compiled /out/fixture /opt/harmony/python
COPY --from=compiled /out/fixture-launcher /opt/harmony/fixture
COPY --from=compiled /out/symbols /symbols
COPY --from=compiled /out/licenses /licenses
COPY workloads/languages/vendor/ANTITHESIS-SDK-CPP-LICENSE /licenses/antithesis-sdk-cpp.txt
ENV PYTHONHASHSEED=0 PYTHON_JIT=0 PYTHONPERFSUPPORT=0 PYTHONDONTWRITEBYTECODE=1
CMD ["/opt/harmony/fixture"]
