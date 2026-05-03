-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1c — OPAQUE login flow.
--
-- ServerLogin<CipherSuite> state должен жить между /login/start и
-- /login/finish (в отличие от registration где сервер stateless).
-- Хранение в DB вместо in-memory cache — выживает рестарты, корректно
-- работает при horizontal scale-апе (любой instance может ответить на
-- /login/finish).
--
-- БЕЗ FK на accounts(id) — для unknown account-ов /login/start всё
-- равно проходит через ServerLogin::start с password_file=None и
-- кэширует state (защита от username-enumeration по timing).

CREATE TABLE IF NOT EXISTS opaque_login_states (
    session_id   BINARY(16)      NOT NULL PRIMARY KEY,
    account_id   BINARY(16)      NOT NULL,
    -- Сериализованный ServerLogin<LakkoCipherSuite>. Размер ~hundreds bytes.
    -- VARBINARY(2048) с запасом.
    state_bytes  VARBINARY(2048) NOT NULL,
    created_at   TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- Индекс для эффективного pruner-а (DELETE WHERE created_at < ...).
    INDEX idx_opaque_login_states_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
