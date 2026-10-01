-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0060 section 3: channels get a member list.
--
-- One member card per registered subscriber: the subscriber's profile
-- envelope sealed under the channel's epoch seed
-- (`lakko_channels::member_card`), so every holder of that seed can open it
-- and the server cannot (invariant #2). The server stores it per subscriber
-- key it already holds, so it learns nothing new about who subscribes.
--
-- `epoch` is the card's clear epoch (read with `member_card_epoch`, never
-- authenticated by the server): always <= `channels.current_epoch` when
-- stored, and never lower than the card it replaces.
--
-- The composite FK drops a card with its subscriber, so the stale-subscriber
-- prune removes cards too.

CREATE TABLE IF NOT EXISTS channel_member_cards (
    channel_id      BINARY(16)      NOT NULL,
    subscriber_id   BINARY(32)      NOT NULL,
    -- Opaque sealed card, at most `lakko_channels::MEMBER_CARD_MAX_LEN`
    -- (1057) bytes; the length is checked at handler entry.
    card            VARBINARY(1057) NOT NULL,
    epoch           INT UNSIGNED    NOT NULL,
    updated_at      TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (channel_id, subscriber_id),
    CONSTRAINT fk_channel_member_cards_subscriber
        FOREIGN KEY (channel_id, subscriber_id)
        REFERENCES channel_subscribers(channel_id, subscriber_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
