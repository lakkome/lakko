-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Short invite links (the 2026-09-27 amendment of ADR-0028).
--
-- The three invite ledgers (`/i/` contact, `/c/` call, `/g/` group) were
-- addressed by `blob_id = SHA-256(capability_bytes)`, 32 bytes, which is 43
-- base64url characters in a link. They are now addressed by a random 6-byte
-- id the server mints at upload (`lakko_contacts::invite::generate_blob_id`),
-- 8 characters in a link. The id is a lookup handle only: the joiner still
-- verifies the capability's Ed25519 signature. Upload is a plain INSERT
-- retried under a fresh id on a primary-key collision - never an upsert, so a
-- collision can never overwrite another link's row - and the same capability
-- uploaded twice gets two ids.
--
-- Flag day: there is no deployment and no row worth keeping, so the six
-- tables are dropped and recreated at the new width rather than altered (an
-- in-place MODIFY of a primary key that a consumer table references by
-- foreign key would need the constraints dropped and re-added anyway). The
-- recreated shapes are 0004+0005+0006, 0007+0008 and 0014+0015 with
-- `blob_id BINARY(6)`; every other column, index and constraint is unchanged.

DROP TABLE IF EXISTS invite_consumers;
DROP TABLE IF EXISTS invites;
DROP TABLE IF EXISTS call_invite_consumers;
DROP TABLE IF EXISTS call_invites;
DROP TABLE IF EXISTS group_invite_consumers;
DROP TABLE IF EXISTS group_invites;

-- ---------------------------------------------------------------------------
-- `/i/` contact invites (was 0004 + 0005 + 0006)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS invites (
    blob_id           BINARY(6)       NOT NULL PRIMARY KEY
                        COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    capability_bytes  VARBINARY(2048) NOT NULL
                        COMMENT 'lakko_contacts::invite::InviteCapability wire format (1.18a)',
    owner_account_id  BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports owner revocation + the owner list',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means NEVER_EXPIRES; supports periodic pruning',
    max_uses          INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'denormalised from capability_bytes — sentinels: 0 = SINGLE_USE (effective 1), 0xFFFFFFFF = unlimited; consume handler maps via CASE',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'atomic counter — bumped by /consume; capped by max_uses',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_owner (owner_account_id),
    INDEX idx_expiry (expiry_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches invites.blob_id; FK ON DELETE CASCADE so consumer rows die with the invite',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256, 1.18d3a)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ---------------------------------------------------------------------------
-- `/c/` call invites (was 0007 + 0008)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS call_invites (
    blob_id           BINARY(6)       NOT NULL PRIMARY KEY
                        COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    capability_bytes  VARBINARY(8192) NOT NULL
                        COMMENT 'lakko_contacts::call_invite::CallInviteCapability wire format (3c3)',
    owner_account_id  BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports owner revocation',
    call_id           BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports lookup by call',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means CALL_NEVER_EXPIRES; supports periodic pruning',
    max_uses          INT UNSIGNED    NOT NULL
                        COMMENT 'denormalised ceiling — 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'atomic counter — bumped by /consume; capped by max_uses',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_call_owner (owner_account_id),
    INDEX idx_call_expiry (expiry_ms),
    INDEX idx_call_id (call_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS call_invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches call_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES call_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ---------------------------------------------------------------------------
-- `/g/` group invites (was 0014 + 0015)
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS group_invites (
    blob_id           BINARY(6)      NOT NULL PRIMARY KEY
                        COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    capability_bytes  VARBINARY(512) NOT NULL
                        COMMENT 'lakko_contacts::group_invite::GroupInviteCapability wire format (ADR-0028)',
    issuer_pubkey     BINARY(32)     NOT NULL
                        COMMENT 'denormalised Ed25519 issuer key from the verified capability — supports lookup by issuer',
    group_id          VARBINARY(256) NOT NULL
                        COMMENT 'denormalised opaque MLS GroupId from the verified capability — supports lookup by group',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means GROUP_INVITE_NEVER_EXPIRES; supports periodic pruning',
    max_uses          INT UNSIGNED    NOT NULL
                        COMMENT 'SOFT throttle (NOT signed, NOT a security boundary) — 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'atomic counter — bumped by /consume; capped by max_uses',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_group_invite_issuer (issuer_pubkey),
    INDEX idx_group_invite_expiry (expiry_ms),
    INDEX idx_group_invite_gid (group_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS group_invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches group_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES group_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
