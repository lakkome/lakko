-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 3c3-multi-use — per-joiner consume-dedup side table
-- for call invites. Mirror of `0006_invite_consumers.sql` for
-- the contact-invite ecosystem.
--
-- 3c3-server (0007_call_invites.sql) wired atomic `max_uses`
-- enforcement against a flat `used_count` counter on
-- `call_invites`, but had no way to dedup repeats from the
-- same joiner — clicking Accept twice on the same call invite
-- spent two of the budget. This migration adds the side table
-- the consume handler now uses to dedup: one row per
-- `(blob_id, joiner_tag)` pair, where `joiner_tag` is the
-- 32-byte HMAC tag from `lakko_contacts::invite::compute_joiner_tag`
-- (same helper as contact invites use; salt depends on
-- `capability_bytes` so tags are per-blob unique even
-- though the function is shared across invite kinds).
--
-- ## Why reuse compute_joiner_tag instead of a call-specific
--    helper
--
-- The salt derivation (`SHA-256(JOINER_TAG_SALT_DOMAIN_V1 ||
-- capability_bytes)`) folds the full capability bytes in,
-- so a contact-invite and a call-invite for the same owner +
-- same joiner produce different joiner_tags (because their
-- wire bytes differ structurally). Sharing the function means
-- the joiner-side JS/Swift/Kotlin code path doesn't grow a
-- second helper, and the server stores opaque 32-byte tags
-- either way.
--
-- ## FK CASCADE
--
-- When the inviter revokes (future 3c3 owner-revoke handler)
-- or an expiry pruner deletes from `call_invites`, the
-- side-table rows die with it. Mirror of the contact-invite
-- side-table FK shape.

CREATE TABLE IF NOT EXISTS call_invite_consumers (
    blob_id      BINARY(32) NOT NULL
                   COMMENT 'matches call_invites.blob_id; FK ON DELETE CASCADE',
    joiner_tag   BINARY(32) NOT NULL
                   COMMENT 'lakko_contacts::invite::compute_joiner_tag output (HMAC-SHA-256)',
    consumed_at  TIMESTAMP  NOT NULL DEFAULT CURRENT_TIMESTAMP
                   COMMENT 'informational; not read on the dedup path',

    PRIMARY KEY (blob_id, joiner_tag),
    FOREIGN KEY (blob_id) REFERENCES call_invites(blob_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
