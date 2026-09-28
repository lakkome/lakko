-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054 S5 — sign in and recover by name without a discovery lookup.
--
-- The client derives a login handle from its canonical username
-- (`BLAKE3-derive-key("lakko/auth/login-handle/v1", username)`) and binds it
-- to its account once signed in (`PUT /auth/login-handle`). The pre-session
-- routes take the handle and resolve it here.
--
-- Only `keyed-BLAKE3(LAKKO_AUTH_HANDLE_PEPPER, handle)` is stored: without the
-- pepper a dump of this table is not a dictionary over usernames. One handle
-- per account (UNIQUE account_id), one account per handle (the primary key);
-- deleting the account deletes its handle.
CREATE TABLE IF NOT EXISTS login_handles (
    handle_key BINARY(32)   NOT NULL PRIMARY KEY COMMENT 'keyed-BLAKE3(pepper, login handle)',
    account_id BINARY(16)   NOT NULL,
    created_at TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    UNIQUE KEY uq_login_handles_account (account_id),
    CONSTRAINT fk_login_handles_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
