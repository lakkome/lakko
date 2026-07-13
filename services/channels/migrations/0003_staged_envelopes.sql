-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0030 (batched channel-rotation fanout) — the staging
-- table for the two-phase prepare + atomic commit rotation.
--
-- The single-shot POST /v1/channels/:id/rotate (migration
-- 0001 + the `rotate` store method) takes the whole
-- subscriber envelope set in one request + commits it under
-- one write lock (audit invariant C-42). That does not scale
-- to the 50k-1M subscribers ADR-0010 targets: the request
-- body is multi-MB..GB and a dropped connection wastes the
-- whole attempt.
--
-- ADR-0030 splits rotation into:
--   * STAGE (chunked, resumable, idempotent) — the publisher
--     uploads the new epoch's envelopes in bounded chunks
--     into THIS table. The epoch is NOT bumped and these rows
--     are NEVER served to subscribers (the read path
--     `fetch_envelope` only ever consults `channel_envelopes`).
--   * COMMIT (atomic, single) — one transaction re-verifies
--     `target_epoch == current_epoch + 1`, promotes the
--     staged rows into `channel_envelopes`, bumps
--     `current_epoch`, and clears the staged rows for the
--     channel. This is the single atomic point; C-42 is
--     preserved, now as "the commit is atomic" rather than
--     "the one giant request is atomic".
--
-- Opacity (invariant #2): `envelope` is opaque ciphertext,
-- exactly like `channel_envelopes.envelope`. The server never
-- seals, opens, or parses a seed here.

CREATE TABLE IF NOT EXISTS channel_staged_envelopes (
    channel_id      BINARY(16)      NOT NULL,
    -- The epoch this staged set becomes live at on commit.
    -- Always `current_epoch + 1` at stage time; commit
    -- re-verifies before promoting, so a stale / replayed
    -- target cannot roll the epoch back or skip.
    target_epoch    INT UNSIGNED    NOT NULL,
    -- 32-byte X25519 subscriber pubkey — an identifier only,
    -- never an ECDH input (same contract as
    -- `channel_subscribers.subscriber_id`, C-41 #5). There is
    -- deliberately NO foreign key to `channel_subscribers`
    -- here: staged rows are transient scratch space and a
    -- subscriber pruned mid-prepare must not fail an INSERT.
    -- `commit_rotation` instead promotes only staged rows
    -- whose subscriber is still registered (partial-coverage
    -- policy), so an unregistered staged row is silently
    -- skipped rather than blocking the commit.
    subscriber_id   BINARY(32)      NOT NULL,
    -- HPKE-sealed seed envelope, opaque. Same wire length as
    -- `channel_envelopes.envelope` (validated at handler
    -- entry). Promoted into `channel_envelopes` only by
    -- `commit_rotation`; never served to subscribers.
    envelope        VARBINARY(255)  NOT NULL,
    staged_at       TIMESTAMP(3)    NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    -- Keyed on (channel, target_epoch, subscriber) so a
    -- re-sent chunk is an idempotent upsert (resumability):
    -- re-staging the same subscriber for the same target
    -- epoch overwrites the prior envelope.
    PRIMARY KEY (channel_id, target_epoch, subscriber_id),
    -- Channel deletion cascades the staged rows away. A
    -- staging TTL / GC sweep for an abandoned prepare (staged
    -- but never committed) is deferred (ADR-0030 open
    -- question 2) — commit clears the rows for the channel.
    CONSTRAINT fk_channel_staged_envelopes_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
