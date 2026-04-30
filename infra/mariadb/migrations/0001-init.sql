-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.0 placeholder. Per-service schemas (auth, ds, discovery) добавятся
-- в их sprint-ах (1.1 / 1.3 / 1.5).
--
-- Применять вручную на VPS:
--   sudo mariadb < infra/mariadb/migrations/0001-init.sql
--
-- Phase 2: переходим на нормальную migration tool (refinery / sqlx-cli).

CREATE DATABASE IF NOT EXISTS lakko
    CHARACTER SET utf8mb4
    COLLATE utf8mb4_unicode_ci;

-- Сервисный пользователь. Пароль задаётся через GRANT при первом setup-е
-- (см. infra/README.md шаг "MariaDB user").
-- Здесь не создаём — пароль не должен попадать в git.
