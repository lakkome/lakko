-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1c — OPAQUE login flow.
--
-- ServerLogin<CipherSuite> state must live between /login/start and
-- /login/finish (registration is server-stateless, login is not).
-- Storing in DB instead of an in-memory cache so the state survives
-- service restarts and works correctly under horizontal scale-out (any
-- instance can answer /login/finish).
--
-- No FK on accounts(id) — for unknown account_ids, /login/start still
-- runs through ServerLogin::start (with password_file=None) and caches
-- the resulting state. This produces a uniform 401 on /login/finish
-- regardless of whether the account exists, defeating
-- username-enumeration via timing.

CREATE TABLE IF NOT EXISTS opaque_login_states (
    session_id   BINARY(16)      NOT NULL PRIMARY KEY,
    account_id   BINARY(16)      NOT NULL,
    -- Serialized ServerLogin<LakkoCipherSuite>; size is in the hundreds of
    -- bytes. VARBINARY(2048) gives plenty of headroom.
    state_bytes  VARBINARY(2048) NOT NULL,
    created_at   TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- Index to make pruning (DELETE WHERE created_at < ...) fast.
    INDEX idx_opaque_login_states_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
