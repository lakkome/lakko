-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054, what the security pass found (2026-09-28).
--
-- 1. `auth_name_limits.key_hash` is now keyed: keyed-BLAKE3 under a subkey of
--    LAKKO_AUTH_HANDLE_PEPPER over (scope || named value), so a dump of this
--    table plus a list of usernames no longer shows whose name was the target
--    of which attempts. Rows written under the old unkeyed derivation can
--    never match again; drop them (flag day, no deployment).
DELETE FROM auth_name_limits;
ALTER TABLE auth_name_limits
    MODIFY key_hash BINARY(32) NOT NULL
        COMMENT 'keyed-BLAKE3(limiter subkey of the handle pepper, scope || named value)';

-- 2. `login_handle_changes` — when an account last changed its login handle
--    and which handle it held just before, kept apart from `login_handles`
--    so a DELETE of the handle does not reset it. `PUT /auth/login-handle`
--    refuses a change within LAKKO_AUTH_HANDLE_CHANGE_MIN_INTERVAL_SECS of
--    the last one (restoring the previous handle excepted), which is what
--    keeps one account from sweeping the handle registry by
--    bind-and-release. `previous_handle_key` is a handle key, as in
--    `login_handles`, never a handle.
CREATE TABLE IF NOT EXISTS login_handle_changes (
    account_id          BINARY(16)   NOT NULL PRIMARY KEY,
    last_changed_at     TIMESTAMP(3) NULL,
    previous_handle_key BINARY(32)   NULL COMMENT 'keyed-BLAKE3(pepper, login handle)',
    CONSTRAINT fk_login_handle_changes_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 3. The word vault's 10-strike destruction leaves a tombstone: the row keeps
--    `positions`, `salt` and the Argon2 cost (so `/vault/recover/start`
--    answers exactly as before the destruction) and loses `body_enc` (so
--    nothing is left to open; every guess misses). Before, the row was
--    deleted and start switched to decoys, which told a caller that a vault
--    had existed.
ALTER TABLE word_vaults MODIFY body_enc VARBINARY(255) NULL
    COMMENT 'seal_with_key(vault_key, verifier || sealed_entropy); NULL = destroyed (tombstone)';
