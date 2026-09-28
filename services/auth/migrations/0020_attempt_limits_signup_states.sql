-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054 S4 — proof of work leaves every auth route except account
-- creation; server-side counters take its place.
--
-- 1. `auth_name_limits` — one fixed-window counter per (scope, what the
--    request names). The key is a BLAKE3 derivation over the named value
--    (an account id, a contact hash), computed BEFORE any lookup, so a
--    counter exists for a name that has no account exactly as for one that
--    does (enumeration safety). Only a hash, a window start and a count are
--    stored; no plaintext name, no address. Per-address limits live in
--    process memory and never reach the database.
CREATE TABLE IF NOT EXISTS auth_name_limits (
    scope        TINYINT UNSIGNED NOT NULL COMMENT 'limits::Scope discriminant',
    key_hash     BINARY(32)       NOT NULL COMMENT 'BLAKE3(scope || named value)',
    window_start TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    hits         INT UNSIGNED     NOT NULL,
    PRIMARY KEY (scope, key_hash),
    INDEX idx_auth_name_limits_window (window_start)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 2. `signup_states` — binds `/signup/finish` to a `/signup/start` whose
--    proof of work was spent. One row per pending account id; finish
--    deletes it in the same transaction that creates the account, so one
--    proof yields at most one account and a finish without a live start is
--    refused. `with_recovery` pins whether the start carried a recovery
--    registration, so finish must carry the matching upload.
CREATE TABLE IF NOT EXISTS signup_states (
    account_id    BINARY(16)   NOT NULL PRIMARY KEY,
    with_recovery BOOLEAN      NOT NULL,
    created_at    TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_signup_states_created (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 3. The per-row lockouts of `/totp/recover` and `/vault/recover/finish`
--    answered 429 only for a row that exists, which told a caller the
--    account existed. The name counter above replaces them. The vault keeps
--    `failed_attempts` for its 10-strike destruction cap (that path answers
--    the same 401 either way).
ALTER TABLE totp_secrets DROP COLUMN failed_attempts, DROP COLUMN last_failed_at;
ALTER TABLE word_vaults DROP COLUMN last_failed_at;
