-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0018 (Recovery factor cascade, factor (a)) — single-use
-- device-authorization grants.
--
-- An already-authenticated device authorizes a FRESH device to obtain a
-- session token (ACCESS). `/device/authorize/start` (bearer-auth + PoW)
-- mints a random 32-byte grant_token bound to the session's account_id;
-- `/device/authorize/redeem` (anonymous + PoW) submits the token, the
-- server atomically consumes it (a single DELETE whose rows_affected == 1)
-- and issues the session-token triple. Single-use, time-bound,
-- account-bound — modelled on `webauthn_action_grants` (ADR-0014,
-- migration 0009). Short TTL; FK-cascade; swept by the existing state
-- pruner.
--
-- SECURITY: we store the HASH at rest, `grant_hash = SHA-256(grant_token)`,
-- NOT the raw token — mirroring how 0009 stores an opaque `action_hash`.
-- The raw token leaves the server exactly once (the `/start` response) and
-- is presented again only by the fresh device on `/redeem`, where the
-- server recomputes the SHA-256 and matches it. A transient DB read during
-- the grant's short life therefore cannot be replayed: the attacker sees
-- only the hash, and SHA-256 is preimage-resistant, so the raw token (which
-- `/redeem` requires) cannot be recovered from it.

CREATE TABLE IF NOT EXISTS device_grants (
    account_id   BINARY(16)  NOT NULL,
    -- SHA-256 over the random 32-byte grant token. Opaque at rest; the
    -- redeem endpoint recomputes it from the presented raw token and
    -- matches it (preimage resistance keeps a leaked hash unredeemable).
    grant_hash   BINARY(32)  NOT NULL,
    expires_at   TIMESTAMP   NOT NULL,
    PRIMARY KEY (account_id, grant_hash),
    INDEX idx_device_grants_expires (expires_at),
    CONSTRAINT fk_device_grants_account
        FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
