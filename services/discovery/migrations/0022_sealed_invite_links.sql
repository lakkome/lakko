-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Sealed invite links (the 2026-09-28 security pass of ADR-0028 and ADR-0009).
--
-- Every invite link (`/i/` contact, `/c/` call, `/g/` group) now carries a
-- 12-byte secret in its URL fragment, and the signed capability is ENCRYPTED
-- under a key derived from it (`lakko_contacts::link_seal`). The ledger stores
-- the sealed blob only: the server can no longer read, verify or substitute
-- the capability, and a 48-bit id alone opens nothing.
--
-- What the server still needs travels in clear upload fields beside the blob:
--   * `issuer_pubkey` - the Ed25519 key the issuer-signed revoke verifies
--     against (it used to be read out of the stored capability);
--   * `expiry_ms`, `max_uses` - the courtesy expiry filter, the pruner and the
--     consume ceiling (read out of the capability before, for `/i/` and `/c/`);
--   * `label`, `bootstrap_referral` (`/i/` only) - the owner's own list of live
--     invites, which used to decode them from the stored capability.
-- `owner_account_id` (`/i/`, `/c/`) comes from the session token, as the
-- capability's own field was required to match it before.
--
-- Gone, because nothing read them and the server has no business holding
-- them: `call_invites.call_id` and `group_invites.group_id`.
--
-- Column widths are the longest sealed blob each codec can produce
-- (`lakko_contacts::{invite,call_invite,group_invite}::MAX_SEALED_LEN`: the
-- longest capability wire plus 30 bytes of seal overhead); tests in each
-- ledger module pin them.
--
-- Flag day: no deployment, no row worth keeping, the six tables are dropped
-- and recreated.

DROP TABLE IF EXISTS invite_consumers;
DROP TABLE IF EXISTS invites;
DROP TABLE IF EXISTS call_invite_consumers;
DROP TABLE IF EXISTS call_invites;
DROP TABLE IF EXISTS group_invite_consumers;
DROP TABLE IF EXISTS group_invites;

-- ---------------------------------------------------------------------------
-- `/i/` contact invites
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS invites (
    blob_id            BINARY(6)       NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(254)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed InviteCapability; opaque without the link secret',
    owner_account_id   BINARY(16)      NOT NULL
                         COMMENT 'the uploading session account - supports the owner list and account erasure',
    issuer_pubkey      BINARY(32)      NOT NULL
                         COMMENT 'clear upload field - the Ed25519 key an owner-signed revoke verifies against',
    expiry_ms          BIGINT UNSIGNED NOT NULL
                         COMMENT 'clear upload field - 0 means NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'clear upload field - sentinels: 0 = SINGLE_USE (effective 1), 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    label              VARBINARY(64)   NOT NULL DEFAULT ''
                         COMMENT 'clear upload field - UTF-8 owner label for the owner list (lakko_contacts::invite::MAX_LABEL_LEN)',
    bootstrap_referral BOOLEAN         NOT NULL DEFAULT FALSE
                         COMMENT 'clear upload field - signup deep-link flag for the owner list',
    created_at         TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_owner (owner_account_id),
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

-- ---------------------------------------------------------------------------
-- `/c/` call invites
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS call_invites (
    blob_id            BINARY(6)        NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(4338)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed CallInviteCapability; opaque without the link secret',
    owner_account_id   BINARY(16)       NOT NULL
                         COMMENT 'the uploading session account - supports account erasure',
    issuer_pubkey      BINARY(32)       NOT NULL
                         COMMENT 'clear upload field - the Ed25519 key an owner-signed revoke verifies against',
    expiry_ms          BIGINT UNSIGNED  NOT NULL
                         COMMENT 'clear upload field - 0 means CALL_NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED     NOT NULL
                         COMMENT 'clear upload field - 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED     NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    created_at         TIMESTAMP        NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_call_owner (owner_account_id),
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

-- ---------------------------------------------------------------------------
-- `/g/` group invites
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS group_invites (
    blob_id            BINARY(6)       NOT NULL PRIMARY KEY
                         COMMENT 'random ledger id minted at upload (lakko_contacts::invite::generate_blob_id)',
    sealed_capability  VARBINARY(938)  NOT NULL
                         COMMENT 'lakko_contacts::link_seal sealed GroupInviteCapability; opaque without the link secret',
    issuer_pubkey      BINARY(32)      NOT NULL
                         COMMENT 'clear upload field - the Ed25519 key an issuer-signed revoke verifies against',
    expiry_ms          BIGINT UNSIGNED NOT NULL
                         COMMENT 'clear upload field - 0 means GROUP_INVITE_NEVER_EXPIRES; supports the fetch filter and pruning',
    max_uses           INT UNSIGNED    NOT NULL
                         COMMENT 'SOFT throttle (NOT signed, NOT a security boundary) - 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count         INT UNSIGNED    NOT NULL DEFAULT 0
                         COMMENT 'atomic counter - bumped by /consume; capped by max_uses',
    created_at         TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_group_invite_issuer (issuer_pubkey),
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
