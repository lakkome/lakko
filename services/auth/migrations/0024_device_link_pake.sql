-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0049 section 2, amended 2026-09-28 (security pass): the camera-less
-- add-device confirmation becomes a SPAKE2 exchange (`lakko-device-link`).
--
-- The comparison code was SHA-256 of the pairing payload, and the payload
-- carries a free 32-byte rendezvous pseudonym, so a relay could grind its own
-- payload until the codes matched. Now the signed-in device chooses the code,
-- the person types it on the new device, and the devices agree on a key from
-- it; the pairing payload travels sealed under that key. The server relays
-- four opaque SPAKE2 / MAC / AEAD values it cannot open (invariant #2) and
-- enforces one exchange per request: `pake_b` once, the sealed payload once,
-- the outcome once.
--
-- Flag day (no deployment): the table is rebuilt; open requests are dropped.
-- `account_id` is a real account or the stand-in of a handle nobody holds
-- (ADR-0056), so there is still no foreign key.
DROP TABLE IF EXISTS device_requests;

CREATE TABLE device_requests (
    account_id      BINARY(16)       NOT NULL PRIMARY KEY
        COMMENT 'real account or the stand-in of an unbound handle; one open request per account',
    request_id      BINARY(16)       NOT NULL,
    -- The signed-in device's SPAKE2 message; set once.
    pake_b          VARBINARY(64)    NULL,
    -- The new device's SPAKE2 message, its sealed pairing payload and its key
    -- confirmation; set once, together.
    pake_a          VARBINARY(64)    NULL,
    sealed_payload  VARBINARY(4096)  NULL,
    confirm_a       VARBINARY(32)    NULL,
    -- 1 = accepted (confirm_b set), 2 = wrong code; set once.
    outcome         TINYINT UNSIGNED NULL,
    confirm_b       VARBINARY(32)    NULL,
    created_at      TIMESTAMP(3)     NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    expires_at      TIMESTAMP(3)     NOT NULL,
    UNIQUE INDEX uq_device_requests_request (request_id),
    INDEX idx_device_requests_expires (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
