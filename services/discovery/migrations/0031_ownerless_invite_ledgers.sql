-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0078 section 10: the invite ledgers name no owner and no issuer.
--
-- Until now `invites` and `call_invites` stored the uploading session's
-- account id (`owner_account_id`) and all three ledgers stored the Ed25519
-- key a revocation verifies against (`issuer_pubkey`, the account identity
-- key on every client). Together with the knock journal that tied an account
-- to every link it handed out and to every group it invited into. Each row
-- now stores `revocation_pk`, the public half of a key minted for the one
-- link (`lakko_contacts::invite::revocation_public_key`); the issuing device
-- keeps the seed beside the link secret and signs a revocation with it, so
-- two links of one person carry unrelated keys.
--
-- Also gone: the `label` and `bootstrap_referral` columns, which existed only
-- for `POST /discovery/invite/by-owner` (deleted: a client lists its own
-- links from the local record it keeps for each link it minted), and the
-- `idx_owner` / `idx_call_owner` / `idx_group_invite_issuer` indexes. Account
-- deletion no longer deletes rows by owner; a row lives until its expiry, its
-- last use, or a revocation, and a deleting client revokes its live links
-- first.
--
-- A flag day: nothing is deployed for users, so the six tables are dropped
-- and created again rather than altered.

DROP TABLE IF EXISTS invite_consumers;
DROP TABLE IF EXISTS invites;
DROP TABLE IF EXISTS call_invite_consumers;
DROP TABLE IF EXISTS call_invites;
DROP TABLE IF EXISTS group_invite_consumers;
DROP TABLE IF EXISTS group_invites;

CREATE TABLE IF NOT EXISTS invites (
    blob_id            BINARY(6)       NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(404)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed InviteCapability; opaque without the link secret',
    revocation_pk      BINARY(32)      NOT NULL
                         COMMENT 'per-link Ed25519 key a revocation verifies against (ADR-0078 section 10); names nobody',
    expiry_ms          BIGINT UNSIGNED NOT NULL
                         COMMENT 'clear upload field - 0 means NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'clear upload field - sentinels: 0 = SINGLE_USE (effective 1), 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    created_at         TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_expiry (expiry_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches invites.blob_id; FK ON DELETE CASCADE so consumer rows die with the invite',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag over the PLAINTEXT capability',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS call_invites (
    blob_id            BINARY(6)        NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(4488)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed CallInviteCapability; opaque without the link secret',
    revocation_pk      BINARY(32)       NOT NULL
                         COMMENT 'per-link Ed25519 key a revocation verifies against (ADR-0078 section 10); names nobody',
    expiry_ms          BIGINT UNSIGNED  NOT NULL
                         COMMENT 'clear upload field - 0 means CALL_NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED     NOT NULL
                         COMMENT 'clear upload field - 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED     NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    created_at         TIMESTAMP        NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_call_expiry (expiry_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS call_invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches call_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag over the PLAINTEXT capability',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES call_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS group_invites (
    blob_id            BINARY(6)       NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(938)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed GroupInviteCapability; opaque without the link secret',
    revocation_pk      BINARY(32)      NOT NULL
                         COMMENT 'per-link Ed25519 key a revocation verifies against (ADR-0078 section 10); names nobody',
    expiry_ms          BIGINT UNSIGNED NOT NULL
                         COMMENT 'clear upload field - 0 means GROUP_INVITE_NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED    NOT NULL
                         COMMENT 'SOFT throttle (NOT signed, NOT a security boundary) - 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    created_at         TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_group_invite_expiry (expiry_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS group_invite_consumers (
    blob_id      BINARY(6)  NOT NULL
                   COMMENT 'matches group_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag over the PLAINTEXT capability',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES group_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
