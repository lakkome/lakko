-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0031 (multi-epoch channel seed-envelope retention) —
-- re-key `channel_envelopes` from one row per (channel,
-- subscriber) to N rows per (channel, subscriber, epoch).
--
-- Migration 0001 kept exactly ONE seed envelope per
-- subscriber (latest-wins, the "N=1 MVP" retention model).
-- The consequence: a subscriber offline across a rotation
-- (epoch N -> N+1) came back, got only the current (N+1)
-- seed, and could not decrypt any post made under epoch N —
-- the seed for N was gone on both server and client. At the
-- 24h rotation cadence ADR-0010 targets, one day offline lost
-- a day of history.
--
-- This migration retains the most-recent N seed envelopes per
-- subscriber (default N = 4, `LAKKO_CHANNELS_ENVELOPE_RETENTION_EPOCHS`),
-- keyed by epoch, so a catching-up subscriber can pull the
-- retained set and decrypt each post with the seed matching
-- its `epoch_id`. The store's write paths file at a concrete
-- epoch and then prune each (channel, subscriber) to its
-- highest N epochs, so growth is bounded.
--
-- Opacity (invariant #2): the new `epoch` column is server
-- bookkeeping alongside opaque `envelope` ciphertext. The
-- server never seals, opens, or parses a seed here — same
-- contract as before the re-key.
--
-- C-42 (rotation atomicity) is preserved and slightly
-- strengthened: `channel_envelopes` only ever holds epochs
-- `<= channels.current_epoch` (the store refuses a
-- future-epoch deliver, and rotate / commit file at the new
-- epoch in the same transaction that bumps `current_epoch`).

-- ------------------------------------------------------------
-- 1. Add the `epoch` column.  Defaults to 0 for the ADD so
--    the statement succeeds against a populated table; step 2
--    backfills every existing row to its channel's current
--    epoch before the primary key is re-keyed.
-- ------------------------------------------------------------
ALTER TABLE channel_envelopes
    ADD COLUMN epoch INT UNSIGNED NOT NULL DEFAULT 0
        COMMENT 'Epoch this seed envelope belongs to (ADR-0031). Always <= channels.current_epoch.';

-- ------------------------------------------------------------
-- 2. Backfill existing single rows to the channel's current
--    epoch.  There is exactly one row per (channel, subscriber)
--    under the old primary key, so this cannot produce a
--    duplicate (channel_id, subscriber_id, epoch) tuple.
-- ------------------------------------------------------------
UPDATE channel_envelopes ce
    JOIN channels c ON ce.channel_id = c.id
    SET ce.epoch = c.current_epoch;

-- ------------------------------------------------------------
-- 3. Drop the two foreign keys before re-keying the primary
--    key.  MariaDB/InnoDB pins the old PRIMARY KEY
--    (channel_id, subscriber_id) as the index backing the
--    composite subscriber FK, so the PK cannot be dropped
--    while that FK still references it.  Both FKs are re-added
--    verbatim in step 5.
-- ------------------------------------------------------------
ALTER TABLE channel_envelopes
    DROP FOREIGN KEY fk_channel_envelopes_subscriber,
    DROP FOREIGN KEY fk_channel_envelopes_channel;

-- ------------------------------------------------------------
-- 4. Re-key the primary key to include the epoch, so up to N
--    epochs of envelopes coexist per subscriber.
-- ------------------------------------------------------------
ALTER TABLE channel_envelopes
    DROP PRIMARY KEY,
    ADD PRIMARY KEY (channel_id, subscriber_id, epoch);

-- ------------------------------------------------------------
-- 5. Re-add both foreign keys.  The new primary key's leftmost
--    prefixes — (channel_id) and (channel_id, subscriber_id) —
--    serve as the indexes InnoDB requires for each FK, so no
--    separate secondary index is needed.
--
--    Semantics are unchanged from migration 0001: channel
--    deletion cascades every retained epoch away, and a
--    subscriber un-following (row removed from
--    channel_subscribers) cascades all of that subscriber's
--    retained envelopes on that channel.
-- ------------------------------------------------------------
ALTER TABLE channel_envelopes
    ADD CONSTRAINT fk_channel_envelopes_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE,
    ADD CONSTRAINT fk_channel_envelopes_subscriber
        FOREIGN KEY (channel_id, subscriber_id)
        REFERENCES channel_subscribers(channel_id, subscriber_id)
        ON DELETE CASCADE;
