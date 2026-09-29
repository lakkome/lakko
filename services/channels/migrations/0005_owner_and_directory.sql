-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0059 S2 - the channel owner key and the directory.
--
-- No production data exists, so the owner key is added NOT NULL with no
-- back-fill (flag day): every channel is created with the Ed25519 key its
-- owner derives from the account seed and the channel id
-- (`lakko_channels::owner`). The key does not name the account.

ALTER TABLE channels
    ADD COLUMN owner_pubkey BINARY(32) NOT NULL
        COMMENT 'Ed25519 owner key, registered at creation (ADR-0059 section 1)',
    ADD COLUMN first_listed_at_ms BIGINT UNSIGNED NULL
        COMMENT 'When the channel was first listed; survives withdrawal, feeds the fresh-listing bonus';

-- A subscription's first registration time is kept across re-subscribes
-- (the 7-day growth signal counts new registrations, not re-registrations).
-- `channel_subscribers.registered_at` already exists; the SQL store stops
-- bumping it on a duplicate subscribe.

-- Owner request-proof nonces: a proof whose nonce was seen is refused, and a
-- proof older than the 120 s window is refused by the clock, so rows older
-- than twice the window are pruned.
CREATE TABLE IF NOT EXISTS channel_owner_nonces (
    nonce       BINARY(16)      NOT NULL,
    seen_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (nonce),
    KEY idx_channel_owner_nonces_seen (seen_at_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The public listing an owner chose to publish. `listing` is the canonical
-- `lakko/channel-listing/v1` bytes served back verbatim; the text columns are
-- the same fields decoded, kept for the bucket index and search. `topic` is
-- the bucket topic: an id outside the known list is filed under Other (0).
CREATE TABLE IF NOT EXISTS channel_listings (
    channel_id     BINARY(16)       NOT NULL,
    topic          TINYINT UNSIGNED NOT NULL,
    language       VARCHAR(3)       NOT NULL,
    handle         VARCHAR(32)      NOT NULL,
    name           VARCHAR(255)     NOT NULL,
    description    TEXT             NOT NULL,
    listing        BLOB             NOT NULL,
    updated_at_ms  BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (channel_id),
    KEY idx_channel_listings_bucket (topic, language),
    CONSTRAINT fk_channel_listings_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Operator moderation (internal listener only). One row per channel that was
-- ever moderated: the last action's reason code and whether the channel is
-- blocked from being listed again. No foreign key: a block outlives nothing
-- else and must hold even for a channel id not created yet.
CREATE TABLE IF NOT EXISTS channel_moderation (
    channel_id    BINARY(16)       NOT NULL,
    blocked       TINYINT(1)       NOT NULL DEFAULT 0,
    reason        VARCHAR(64)      NOT NULL,
    action_at_ms  BIGINT UNSIGNED  NOT NULL,
    PRIMARY KEY (channel_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
