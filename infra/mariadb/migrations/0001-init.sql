-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.0 placeholder. Per-service schemas (auth, ds, discovery) land
-- in their respective sprints (1.1 / 1.3 / 1.5).
--
-- Apply manually on the VPS:
--   sudo mariadb < infra/mariadb/migrations/0001-init.sql
--
-- Phase 2: switch to a proper migration tool (refinery or sqlx-cli).

CREATE DATABASE IF NOT EXISTS lakko
    CHARACTER SET utf8mb4
    COLLATE utf8mb4_unicode_ci;

-- Service user. The password is set via GRANT at first setup time (see
-- the "MariaDB user" step in infra/README.md). It is **not** declared
-- here — passwords must not enter the git history.
