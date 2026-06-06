-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- G-13 (ADR-0015, sprint 2) — device-removal attestation signing key.
--
-- A DEDICATED Ed25519 key (separate from the call-JWT signing key, for key
-- separation) the auth service uses to sign action attestations on
-- `/auth/action/finish`. The public half is published at
-- `GET /.well-known/device-attest-jwks.json` so it can be pinned /
-- distributed to clients (the peer devices that verify a removal hold it
-- from pinned / bootstrap trust config — fetching it from this endpoint is
-- for discovery, not for the zero-trust verification path).
--
-- Single-row table (`id = 1`), mirroring `call_jwt_signing_key` (0007): the
-- keypair is rebuilt from this 32-byte seed at every process start. Rotation
-- is out of scope here (same posture as the call-JWT key).

CREATE TABLE IF NOT EXISTS action_attest_signing_key (
    id    TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    -- 32-byte Ed25519 seed; the signing keypair is derived from it.
    seed  BINARY(32)       NOT NULL,
    created_at_ms BIGINT   NOT NULL DEFAULT (UNIX_TIMESTAMP() * 1000)
) ENGINE = InnoDB
  DEFAULT CHARSET = utf8mb4
  COLLATE = utf8mb4_unicode_ci;
