-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0028 (G-3) — per-joiner consume-dedup side table for group
-- invites. Mirror of `0008_call_invite_consumers.sql`.
--
-- `0014_group_invites.sql` wired the atomic `max_uses` ceiling
-- against a flat `used_count` counter, but cannot tell a repeat
-- from the same joiner (tapping "request to join" twice) from a
-- fresh one. This side table dedups: one row per `(blob_id,
-- joiner_tag)`, where `joiner_tag` is the 32-byte HMAC tag from
-- `lakko_contacts::invite::compute_joiner_tag` (the same helper
-- the contact- and call-invite ecosystems use; the salt folds in
-- `capability_bytes` so tags are per-blob unique even though the
-- function is shared across invite kinds).
--
-- ## Server-blindness
--
-- The server stores only the opaque 32-byte tag — never the raw
-- joiner account id, which the HMAC does not reveal (one-way).
-- A repeat from the same joiner conflicts on the PK, so the
-- consume handler idempotently returns "already consumed"
-- without bumping the counter (invariant #5 — no social graph
-- materialised server-side).
--
-- ## FK CASCADE
--
-- When an expiry pruner (or a future owner-revoke handler)
-- deletes from `group_invites`, the side-table rows die with it.

CREATE TABLE IF NOT EXISTS group_invite_consumers (
    blob_id      BINARY(32) NOT NULL
                   COMMENT 'matches group_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES group_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
