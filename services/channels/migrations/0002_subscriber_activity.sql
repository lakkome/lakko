-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 4a (G4-1-subretention) — subscriber-activity column
-- for stale-subscriber retention pruning.
--
-- The publisher's rotation cost scales linearly with
-- registered-subscriber count (one HPKE seal + one HTTPS
-- POST per subscriber per rotation), so subscribers that
-- went silent N days ago must be GC'd or the rotation
-- cost climbs unbounded over the lifetime of the channel.
-- 0001_init.sql kept subscribers forever; this migration
-- adds the activity timestamp the pruner needs.
--
-- The column tracks "last time the subscriber's client
-- touched this channel" — bumped by services/channels
-- on every successful `GET .../subscribers/:pk/envelope`
-- (the subscriber's own poll loop is the activity signal;
-- we don't need a separate heartbeat). A future sub-
-- sprint can extend the touch to other endpoints if real
-- traffic shows that's necessary.

ALTER TABLE channel_subscribers
    ADD COLUMN last_active_at TIMESTAMP(3) NOT NULL
        DEFAULT CURRENT_TIMESTAMP(3)
        COMMENT 'Bumped on every successful envelope GET; pruner cutoff.';

-- Backfill existing rows — pretend they were active at
-- registration time, which is the most lenient possible
-- starting point. The prune loop will catch them on its
-- next tick if they're truly stale.
UPDATE channel_subscribers
    SET last_active_at = registered_at
    WHERE last_active_at IS NULL OR last_active_at < registered_at;

-- Hot path for the pruner:
--   DELETE FROM channel_subscribers
--   WHERE last_active_at < ?
CREATE INDEX idx_subscribers_last_active
    ON channel_subscribers (last_active_at);
