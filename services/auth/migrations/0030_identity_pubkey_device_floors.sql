-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0066 sprint T1: the recorded identity key and per-device session
-- floors.
--
-- 1. `accounts.identity_pubkey` (ADR-0066 section 0.1). The account's
--    identity signature public key (Ed25519), the same key discovery
--    publishes for the account. NULL until the client's first genesis mint
--    (`POST /auth/genesis/attest`), which sets it once; from then on that
--    route attests only this key. The key reset
--    (`POST /account/keys/reset/finish`) is the one writer that moves it, to
--    the key the reset installs. A statement signed by this key
--    (`lakko-attest` account statement) is what `POST /account/devices/revoke`
--    requires beside the fresh-auth grant. A public key: no private material
--    (invariant #2), no PII (invariant #1).
--
-- 2. `device_session_floors` (ADR-0066 section 0.4). One row per (account,
--    device) that logged out or was removed: a session credential naming
--    that device and issued at or before `not_before_ms` is refused. `reason`
--    1 = the device logged out (plain 401 / refresh-invalid), 2 = the device
--    was removed from the account (401 device-revoked). Not a registry of
--    devices: a row exists only for a device that logged out or was removed,
--    and the pruner drops it once the floor is older than the refresh
--    lifetime (every credential it could refuse has expired). `device_id` is
--    the public id of discovery's device directory (the conversation leaf
--    signature key).
--
-- The recall anchors (ADR-0066 section 0.2) are sprint T3's and get their
-- own migration.

ALTER TABLE accounts
    ADD COLUMN identity_pubkey BINARY(32) NULL
        COMMENT 'Ed25519 identity signature pubkey; set once at the genesis mint, moved by a key reset (ADR-0066)';

CREATE TABLE IF NOT EXISTS device_session_floors (
    account_id    BINARY(16)       NOT NULL COMMENT 'FK -> accounts.id',
    device_id     BINARY(32)       NOT NULL COMMENT 'conversation leaf signature key (public device id)',
    not_before_ms BIGINT           NOT NULL COMMENT 'unix ms; credentials issued at or before it are refused',
    reason        TINYINT UNSIGNED NOT NULL COMMENT '1 = logout, 2 = removed',
    PRIMARY KEY (account_id, device_id),
    CONSTRAINT fk_device_session_floors_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The hourly pruner deletes by floor age.
CREATE INDEX idx_device_session_floors_not_before
    ON device_session_floors (not_before_ms);
