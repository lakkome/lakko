-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.18d1 — capability-based invite-link blob storage.
--
-- ADR-0005 §13.3 calls for `InviteCapability` URL pointers with
-- the bytes living server-side. 1.18a/b/c shipped the wire codec
-- and a V1 web flow that puts the capability bytes inline in the
-- URL itself. 1.18d1 adds the indirection: owner POSTs the
-- capability to `/discovery/invite` and gets a short content-
-- addressed blob_id back; the URL becomes
-- `https://lakko.me/i/<blob_id>`, and joiners GET the bytes by id.
--
-- ## Why content-addressed by SHA-256
--
-- - Same capability bytes -> same blob_id. A retried upload is a
--   no-op (`INSERT ... ON DUPLICATE KEY UPDATE`) — no
--   accumulation of identical rows.
-- - The 32-byte hash double-roles as a tamper detector: the
--   server cannot return a substituted capability for a given
--   blob_id without changing the id (and the joiner re-hashes
--   the bytes locally before consuming).
-- - Decouples the URL surface from any internal sequence /
--   primary-key choice.
--
-- ## What this migration ships, and what it leaves for d2/d3
--
--   * `used_count` is here but **not enforced** in 1.18d1 —
--     the column exists so 1.18d2 can wire `max_uses` rejection
--     without another schema bump.
--   * Per-joiner de-dup (`HmacBloom` per ADR-0005) lands as a
--     side-table in 1.18d3. We do NOT bake the bloom into this
--     row because Bloom filters grow with N — a separate
--     append-only table per blob is cleaner.
--
-- ## Sizes
--
-- The 1.18a wire format is fixed-size at 224 bytes max:
--   1 (schema) + 32 (Ed25519) + 16 (account_id) + 32 (X25519)
--   + 8 (expiry) + 4 (max_uses) + 1 (referral) + 2 (label_len)
--   + 64 (label) + 64 (signature) = 224.
-- VARBINARY(2048) gives ~9× headroom for future schema bumps —
-- adding `transparency_proof` per ADR-0005 §13.3 (KT-log
-- inclusion) would push closer to 600 bytes; we have the room.

CREATE TABLE IF NOT EXISTS invites (
    blob_id           BINARY(32)      NOT NULL PRIMARY KEY
                        COMMENT 'SHA-256 of capability_bytes — content-addressed',
    capability_bytes  VARBINARY(2048) NOT NULL
                        COMMENT 'lakko_contacts::invite::InviteCapability wire format (1.18a)',
    owner_account_id  BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports owner revocation in 1.18d2',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means NEVER_EXPIRES; supports periodic pruning',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'reserved for 1.18d2 max_uses enforcement; not consulted in 1.18d1',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_owner (owner_account_id),
    INDEX idx_expiry (expiry_ms)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
