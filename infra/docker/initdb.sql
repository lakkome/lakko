-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- MariaDB bootstrap for the local full-stack Docker run. Mounted into the
-- mariadb container's /docker-entrypoint-initdb.d so it runs once on first
-- boot. Each Lakko service owns its OWN database (sqlx tracks applied
-- migrations per-DB in `_sqlx_migrations`, so two services must NOT share a
-- database). Services apply their own migrations on startup against these
-- empty schemas. `storage` uses a blob directory, not a database, so it has
-- no schema here.

CREATE DATABASE IF NOT EXISTS lakko_auth      CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS lakko_ds        CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS lakko_discovery CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS lakko_channels  CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS lakko_push_gw   CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;

CREATE USER IF NOT EXISTS 'lakko'@'%' IDENTIFIED BY 'devpass';
GRANT ALL PRIVILEGES ON lakko_auth.*      TO 'lakko'@'%';
GRANT ALL PRIVILEGES ON lakko_ds.*        TO 'lakko'@'%';
GRANT ALL PRIVILEGES ON lakko_discovery.* TO 'lakko'@'%';
GRANT ALL PRIVILEGES ON lakko_channels.*  TO 'lakko'@'%';
GRANT ALL PRIVILEGES ON lakko_push_gw.*   TO 'lakko'@'%';
FLUSH PRIVILEGES;
