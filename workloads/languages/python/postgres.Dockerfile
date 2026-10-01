# SPDX-License-Identifier: AGPL-3.0-or-later
ARG HARMONY_PYTHON_BUILD_IMAGE=harmony-python-reference-build:local
ARG HARMONY_PYTHON_IMAGE=harmony-language-python:local
FROM ${HARMONY_PYTHON_IMAGE} AS python
FROM ${HARMONY_PYTHON_BUILD_IMAGE} AS database
ARG PG_VERSION=14.3
ARG PG_SHA256=279057368bf59a919c05ada8f95c5e04abb43e74b9a2a69c3d46a20e07a9af38
RUN apt-get update && apt-get install --yes --no-install-recommends bison flex bzip2 \
    && rm -rf /var/lib/apt/lists/*
RUN curl --fail --silent --show-error --location "https://ftp.postgresql.org/pub/source/v${PG_VERSION}/postgresql-${PG_VERSION}.tar.bz2" -o /build/postgresql.tar.bz2 \
    && echo "${PG_SHA256}  /build/postgresql.tar.bz2" | sha256sum --check --strict \
    && mkdir -p /build/postgresql \
    && tar -xf /build/postgresql.tar.bz2 -C /build/postgresql --strip-components=1
WORKDIR /build/postgresql
RUN CC=clang CFLAGS='-O1 -g -fsanitize-coverage=trace-pc-guard' LDFLAGS='-fno-sanitize-link-runtime /build/shim.o -ldl' \
    ./configure --prefix=/usr/lib/postgresql --without-readline --without-zlib --disable-nls --without-icu \
    && make -j"$(nproc)" && make install \
    && make -C contrib/amcheck && make -C contrib/amcheck install
COPY workloads/bugs/historical/postgres-cic-corruption/image/postgresql.conf.append workloads/bugs/historical/postgres-cic-corruption/image/seed.sql workloads/bugs/historical/postgres-cic-corruption/image/loopback-up.c /build/seed/
RUN clang -O2 -Wall -Werror -o /out/loopback-up /build/seed/loopback-up.c
RUN groupadd --system --gid 70 postgres \
    && useradd --system --uid 70 --gid 70 --home-dir /var/lib/postgresql --shell /bin/sh postgres \
    && mkdir -p /var/lib/postgresql/data /build/seed/sock \
    && chown -R 70:70 /var/lib/postgresql /build/seed \
    && setpriv --reuid=70 --regid=70 --clear-groups env LC_ALL=C TZ=UTC HOME=/var/lib/postgresql \
        /usr/lib/postgresql/bin/initdb -D /var/lib/postgresql/data --locale=C --encoding=UTF8 -A trust -U postgres -N \
    && cat /build/seed/postgresql.conf.append >> /var/lib/postgresql/data/postgresql.conf \
    && setpriv --reuid=70 --regid=70 --clear-groups /usr/lib/postgresql/bin/pg_ctl -D /var/lib/postgresql/data -w -t 120 \
        -o "-k /build/seed/sock -c listen_addresses=''" -l /build/seed/log start \
    && setpriv --reuid=70 --regid=70 --clear-groups /usr/lib/postgresql/bin/psql -h /build/seed/sock -U postgres -d postgres \
        -v ON_ERROR_STOP=1 -qtAX -v seed_rows=20000 -v fillfactor=10 -f /build/seed/seed.sql \
    && setpriv --reuid=70 --regid=70 --clear-groups /usr/lib/postgresql/bin/pg_ctl -D /var/lib/postgresql/data -w -t 120 -m fast stop
RUN rm -rf /usr/lib/postgresql/include /usr/lib/postgresql/lib/bitcode /usr/lib/postgresql/lib/pgxs \
        /usr/lib/postgresql/share/doc /usr/lib/postgresql/share/man \
    && find /usr/lib/postgresql/lib -name '*.a' -delete \
    && for binary in /usr/lib/postgresql/bin/*; do \
        case "$(basename "$binary")" in postgres|psql|pg_isready|pg_amcheck|pg_ctl) ;; *) rm -f "$binary" ;; esac; \
       done \
    && mkdir -p /out/postgres-symbols /out/postgres-licenses \
    && cp COPYRIGHT /out/postgres-licenses/postgresql.txt \
    && find /usr/lib/postgresql -type f \( -perm -u+x -o -name '*.so*' \) | LC_ALL=C sort > /out/postgres-native-paths \
    && while IFS= read -r file; do \
        destination="/out/postgres-symbols${file}"; mkdir -p "$(dirname "$destination")"; cp "$file" "$destination"; \
        nm --defined-only "$file" | awk '$2 ~ /[tT]/ {print $1 "\t" $3}' > "$destination.sym.tsv"; \
        strip --strip-debug "$file"; \
        sha256sum "$file" >> /out/postgres-symbols/harmony-instrumented-events; \
       done < /out/postgres-native-paths
COPY workloads/languages/python/postgres_driver.py /out/postgres-driver/postgres_driver.py
COPY workloads/languages/python/test_postgres_driver.py /out/postgres-driver/test_postgres_driver.py
RUN /opt/python/bin/python3.14 /out/postgres-driver/test_postgres_driver.py \
    && rm /out/postgres-driver/test_postgres_driver.py \
    && /opt/python/bin/python3.14 /build/recipe/coverage_edges.py /out/postgres-driver harmony-python-postgres /out/postgres-symbols/python-postgres.sym.tsv
FROM python
COPY --from=database /out/loopback-up /opt/harmony/loopback-up
RUN groupadd --system --gid 70 postgres \
    && useradd --system --uid 70 --gid 70 --home-dir /var/lib/postgresql --shell /bin/sh postgres
COPY --from=database /usr/lib/postgresql /usr/lib/postgresql
COPY --from=database /var/lib/postgresql /var/lib/postgresql
COPY --from=database /out/postgres-symbols /symbols/postgres
COPY --from=database /out/postgres-driver /opt/harmony/python-postgres
COPY --from=database /out/postgres-licenses /licenses
COPY workloads/bugs/historical/postgres-cic-corruption/image/setup.sh workloads/bugs/historical/postgres-cic-corruption/image/node.sh workloads/bugs/historical/postgres-cic-corruption/image/ready.sh /opt/harmony/
COPY workloads/languages/python/postgres.bundle /etc/harmony/bundle
RUN cat /symbols/postgres/harmony-instrumented-events >> /symbols/harmony-instrumented-events \
    && cp /symbols/postgres/python-postgres.sym.tsv /symbols/python-postgres.sym.tsv \
    && chmod +x /opt/harmony/setup.sh /opt/harmony/node.sh /opt/harmony/ready.sh
CMD ["/opt/harmony/node.sh"]
