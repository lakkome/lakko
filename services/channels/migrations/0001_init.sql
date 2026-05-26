-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 4a (G4-1-sql) — channels-service persistent schema.
--
-- Four ciphertext-only tables.  None of them touch any
-- plaintext — every payload column carries opaque bytes the
-- server cannot decrypt.  Threat-model boundary documented in
-- docs/security/calls.md C-40..C-42 and ADR-0010 § "What the
-- channels service does NOT do".
--
--   1. `channels`           — registered channel ids + the
--                             server's view of `current_epoch`
--                             (bookkeeping only; the
--                             authoritative epoch lives in the
--                             client-side ChannelPost header).
--   2. `channel_posts`      — opaque ChannelPost bytes keyed by
--                             a per-channel monotone cursor.
--   3. `channel_subscribers`— subscriber X25519 pubkey registry,
--                             used by the publisher to fan out
--                             HPKE-sealed seed envelopes.  The
--                             server never performs ECDH against
--                             these pubkeys — they're identifiers
--                             only.  Audit invariant C-41 #5.
--   4. `channel_envelopes`  — latest HPKE-sealed seed envelope
--                             per (channel, subscriber).
--                             Latest-wins; previous envelopes are
--                             replaced on rotation, matching the
--                             ADR-0010 N=1 retention model.
--
-- Retention:
--   * `channel_posts` is pruned by the background loop in
--     `src/main.rs::run_prune_loop` — defaults to 30 days,
--     overridable via `LAKKO_CHANNELS_RETENTION_SECS`.
--     `next_cursor` is NOT rolled back on prune so a
--     paginating subscriber's resume point stays monotone.
--   * `channel_envelopes` is latest-wins (UPSERT on
--     rotation); no TTL — an envelope is replaced when a
--     new one for the same (channel, subscriber) lands,
--     and dropped when the subscriber un-follows
--     (FK ON DELETE CASCADE).
--   * `channel_subscribers` carries a `last_active_at`
--     column (added in migration 0002) bumped on every
--     successful envelope GET; the background loop GCs
--     subscribers whose last_active is older than
--     `LAKKO_CHANNELS_SUBSCRIBER_RETENTION_SECS`
--     (default 90 days). FK `channel_envelopes` →
--     `channel_subscribers` ON DELETE CASCADE drops the
--     matching envelope automatically.

-- ============================================================
-- channels
-- ============================================================
CREATE TABLE IF NOT EXISTS channels (
    -- 16-byte channel pseudonym (ADR-0010 § "Wire surfaces").
    id              BINARY(16)      NOT NULL,
    -- Server-side bookkeeping epoch.  Bumped by
    -- POST /v1/channels/:id/rotate.  Saturating u32; at one
    -- rotation/second the counter takes ~136 years to wrap,
    -- well past any reasonable identity-key lifetime.
    current_epoch   INT UNSIGNED    NOT NULL DEFAULT 0,
    -- Cursor sequence — last cursor we handed out.  Combined
    -- with WHERE cursor > ? this lets the cursor field stay
    -- monotone across server restarts.  `0` is reserved as
    -- "before any post" so cursors start at 1.
    next_cursor     BIGINT UNSIGNED NOT NULL DEFAULT 1,
    created_at      TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ============================================================
-- channel_posts
-- ============================================================
CREATE TABLE IF NOT EXISTS channel_posts (
    channel_id      BINARY(16)      NOT NULL,
    -- Monotone per-channel cursor — handed out by the
    -- `channels.next_cursor` sequence above on every insert.
    cursor          BIGINT UNSIGNED NOT NULL,
    -- Server wall-clock at insert time, unix milliseconds.
    -- The client never trusts this; it's surfaced for UI
    -- timestamps and metrics.
    server_ts_ms    BIGINT UNSIGNED NOT NULL,
    -- Opaque ChannelPost wire bytes.  LONGBLOB up to 4 GB but
    -- the HTTP layer caps individual posts at the value of
    -- the runtime `max_post_bytes` config (default 1 MB) —
    -- enforced at handler entry before we even touch the DB.
    payload         LONGBLOB        NOT NULL,
    PRIMARY KEY (channel_id, cursor),
    -- Hot path: WHERE channel_id = ? AND cursor > ? ORDER BY cursor LIMIT ?
    INDEX idx_channel_cursor (channel_id, cursor),
    CONSTRAINT fk_channel_posts_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ============================================================
-- channel_subscribers
-- ============================================================
CREATE TABLE IF NOT EXISTS channel_subscribers (
    channel_id      BINARY(16)      NOT NULL,
    -- 32-byte X25519 public key as raw bytes.  The audit
    -- invariant (C-41 #5) requires that the server never
    -- treats this column as an ECDH input — it's an
    -- identifier only.  Every lookup goes through equality
    -- match, never through a `lakko_crypto::agreement::
    -- PublicKey::from_bytes` parse.
    subscriber_id   BINARY(32)      NOT NULL,
    registered_at   TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (channel_id, subscriber_id),
    -- Fanout path: WHERE channel_id = ? ORDER BY registered_at
    INDEX idx_subscribers_registered (channel_id, registered_at),
    CONSTRAINT fk_channel_subscribers_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- ============================================================
-- channel_envelopes
-- ============================================================
CREATE TABLE IF NOT EXISTS channel_envelopes (
    channel_id      BINARY(16)      NOT NULL,
    subscriber_id   BINARY(32)      NOT NULL,
    -- HPKE-sealed seed envelope, exactly
    -- `lakko_channels::SEED_ENVELOPE_LEN = 80` bytes.  Length
    -- is asserted at handler entry — this column is BLOB
    -- only so a future schema bump that changes the wire
    -- length doesn't require a column rebuild.
    envelope        VARBINARY(255)  NOT NULL,
    delivered_at    TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    PRIMARY KEY (channel_id, subscriber_id),
    CONSTRAINT fk_channel_envelopes_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE,
    -- Subscriber must already be registered before we can
    -- deliver an envelope to them.  Composite FK so a
    -- subscriber un-following on one channel only drops their
    -- envelopes on that channel, not globally.
    CONSTRAINT fk_channel_envelopes_subscriber
        FOREIGN KEY (channel_id, subscriber_id)
        REFERENCES channel_subscribers(channel_id, subscriber_id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
