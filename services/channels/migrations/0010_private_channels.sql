-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0087 sections 6, 7 and 15: public and private channels.
--
-- - `channels.private`: the flag. A private channel is never listed (section
--   16) and holds no public seed; subscribing to it needs an invite proof.
-- - `channels.subscriber_count`: a maintained counter (`GET .../subscribers/
--   count`), moved by every insert and removal of a subscriber row, so a
--   publisher's count is not a COUNT(*) over a million rows.
-- - `channel_subscribers.seq`: a row number in insertion order, the cursor of
--   the paged subscriber list (a publisher's admission loop reads rows after
--   the last one it saw); `sealed_ticket`: the opaque admission ticket a
--   private channel's subscriber files (a sealed box to the channel's
--   admission key, `lakko_channels::invite::ADMISSION_TICKET_LEN` bytes). The
--   service stores it and never opens it.
-- - `channel_public_seeds`: one sealed seed per epoch of a public channel
--   (`lakko_channels::public_seed`, 64 bytes), served openly. Kept until the
--   posts it opens are gone: `superseded_at_ms` is set when a later epoch's
--   seed is stored, and the pruner drops a seed superseded before the post
--   retention cutoff.
-- - `channel_invite_keys`: a private channel's per-link Ed25519 invite keys
--   with expiry and use count. A key names no account.
-- - `channel_removed_subscribers`: a subscriber key a publisher removed from
--   a private channel, so its envelope read answers `410 subscriber-removed`
--   rather than "never registered". Pruned with the stale-subscriber
--   retention; a later subscribe of the key clears it.
--
-- Flag day: nothing is deployed. The backfill of the counter is for test
-- stacks only.

ALTER TABLE channels
    ADD COLUMN private TINYINT(1) NOT NULL DEFAULT 0
        COMMENT 'ADR-0087: 1 = private (invite proof to subscribe, never listed)',
    ADD COLUMN subscriber_count BIGINT UNSIGNED NOT NULL DEFAULT 0
        COMMENT 'maintained count of channel_subscribers rows';

UPDATE channels c
   SET subscriber_count = (SELECT COUNT(*) FROM channel_subscribers s
                            WHERE s.channel_id = c.id);

ALTER TABLE channel_subscribers
    ADD COLUMN seq BIGINT UNSIGNED NOT NULL AUTO_INCREMENT
        COMMENT 'insertion order; the cursor of the paged subscriber list',
    ADD COLUMN sealed_ticket BINARY(147) NULL
        COMMENT 'opaque admission ticket to the channel admission key; never opened here',
    ADD UNIQUE KEY uq_channel_subscribers_seq (seq),
    ADD INDEX idx_channel_subscribers_page (channel_id, seq);

CREATE TABLE IF NOT EXISTS channel_public_seeds (
    channel_id        BINARY(16)      NOT NULL,
    epoch             INT UNSIGNED    NOT NULL,
    envelope          BINARY(64)      NOT NULL
                        COMMENT 'lakko_channels::public_seed envelope; opaque here',
    stored_at_ms      BIGINT UNSIGNED NOT NULL,
    superseded_at_ms  BIGINT UNSIGNED NULL
                        COMMENT 'when a later epoch seed was stored',
    PRIMARY KEY (channel_id, epoch),
    KEY idx_channel_public_seeds_superseded (superseded_at_ms),
    CONSTRAINT fk_channel_public_seeds_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_invite_keys (
    channel_id        BINARY(16)      NOT NULL,
    invite_pk         BINARY(32)      NOT NULL,
    expiry_ms         BIGINT UNSIGNED NOT NULL COMMENT '0 = never expires',
    max_uses          INT UNSIGNED    NOT NULL
                        COMMENT '0 = one use, 4294967295 = unlimited (the discovery ledger rule)',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0,
    registered_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (channel_id, invite_pk),
    KEY idx_channel_invite_keys_expiry (expiry_ms),
    CONSTRAINT fk_channel_invite_keys_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS channel_removed_subscribers (
    channel_id     BINARY(16)      NOT NULL,
    subscriber_id  BINARY(32)      NOT NULL,
    removed_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (channel_id, subscriber_id),
    KEY idx_channel_removed_subscribers_at (removed_at_ms),
    CONSTRAINT fk_channel_removed_subscribers_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
