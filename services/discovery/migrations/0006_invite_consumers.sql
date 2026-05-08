-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.18d3b — per-joiner consume-dedup side table.
--
-- 1.18d2a wired atomic `max_uses` enforcement against a flat
-- `used_count` counter, but had no way to tell whether two
-- consume calls came from the same joiner — clicking Accept
-- twice on the same invite spent two of the budget. This
-- migration adds the side table the consume handler uses to
-- dedup repeats: one row per `(blob_id, joiner_tag)` pair, where
-- `joiner_tag` is the 32-byte HMAC tag from
-- `lakko_contacts::invite::compute_joiner_tag` (1.18d3a).
--
-- ## Why a side table, not a bloom filter
--
-- ADR-0005 §13.3 calls the dedup primitive `consumed_pubkey_log:
-- HmacBloom`. For Phase-1 invites with `max_uses ≤ 10` typical,
-- a small side table is simpler than a bloom filter and gives
-- exact dedup (no false-positive rejection of innocent
-- joiners). Switching to a bloom-on-disk later is a non-breaking
-- swap of the storage layer; the wire protocol stays unchanged
-- because the joiner-side helper already produces a 32-byte
-- opaque tag.
--
-- ## FK CASCADE on invites.blob_id
--
-- When the inviter revokes (1.18d2b) or the expiry pruner
-- (1.18d2c) deletes a row from `invites`, the side-table rows
-- die with it. There's no value in keeping consume records for
-- a blob nobody can use any more, and the FK gives us the
-- cleanup for free without growing `invite::spawn_pruner`.
--
-- ## PK shape
--
-- `(blob_id, joiner_tag)` is the dedup primary key — INSERT
-- IGNORE on this PK is what surfaces "already consumed" to the
-- handler without a separate SELECT. We do NOT add a separate
-- index on `joiner_tag` alone — the dedup query is always
-- scoped to a single `blob_id`, and a global index over a
-- random 32-byte HMAC would buy nothing.
--
-- ## Sizes
--
-- 32 + 32 + 4 (TIMESTAMP) ≈ 70 bytes/row + InnoDB overhead.
-- A single invite with `max_uses = u32::MAX` would fit into a
-- few hundred MB before the rate limits / expiry catch up.
-- `BIGINT consumed_at` (TIMESTAMP) is informational only —
-- nothing reads it today. 1.18d3 doesn't add an index on it
-- because the dedup path doesn't need one; if a future
-- "consume audit log" feature wants to scan it, an index can
-- land in its own migration.

CREATE TABLE IF NOT EXISTS invite_consumers (
    blob_id      BINARY(32) NOT NULL
                   COMMENT 'matches invites.blob_id; FK ON DELETE CASCADE so consumer rows die with the invite',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256, 1.18d3a)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
