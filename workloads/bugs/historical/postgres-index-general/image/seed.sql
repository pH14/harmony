-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Build-time seed for the general-discovery PostgreSQL workload: one ordinary
-- table at the default fillfactor with 1000 rows split across four owners and
-- no secondary index. The workload creates and drops its own indexes.

CREATE DATABASE faultlab;
\c faultlab
CREATE EXTENSION amcheck;

CREATE TABLE items(
    id bigint PRIMARY KEY,
    owner int NOT NULL,
    a int NOT NULL,
    b int NOT NULL,
    c text NOT NULL
);
INSERT INTO items
    SELECT g, (g - 1) % 4, g, g % 100, md5(g::text) FROM generate_series(1, 1000) g;

VACUUM ANALYZE items;
CHECKPOINT;
