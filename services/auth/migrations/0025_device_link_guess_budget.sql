-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0049 section 2 (the device link), 2026-09-28 security pass finding 6:
-- the guess budget moves to the server.
--
-- Each device request gets one online guess at the six-digit code, but the
-- budget across requests was the clients' to keep. Now every `wrong_code`
-- outcome an account's signed-in device records is logged here, and enough of
-- them in a window suspend the account's device requests: while suspended, a
-- `POST /device/request` naming the account is stored against the handle's
-- stand-in (nobody polls it) and `GET /device/requests` shows the account
-- nothing. Thresholds and durations are env knobs (`limits.rs`).
--
-- Stored: the real account id (only a signed-in device of the account posts
-- an outcome), the time of each wrong code, and the end of a suspension. No
-- handle, no address, no request content. Both tables go with the account.

CREATE TABLE IF NOT EXISTS device_link_wrong_codes (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    account_id  BINARY(16)      NOT NULL,
    at          TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    INDEX idx_device_link_wrong_codes_account (account_id, at),
    INDEX idx_device_link_wrong_codes_at (at),
    CONSTRAINT fk_device_link_wrong_codes_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS device_link_suspensions (
    account_id       BINARY(16)   NOT NULL PRIMARY KEY,
    suspended_until  TIMESTAMP(3) NOT NULL,
    INDEX idx_device_link_suspensions_until (suspended_until),
    CONSTRAINT fk_device_link_suspensions_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
