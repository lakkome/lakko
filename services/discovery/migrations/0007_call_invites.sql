-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 3c3-server — capability-based call-invite blob storage.
--
-- Sibling of `0004_invites.sql` for `CallInviteCapability` per
-- ROADMAP 3c3. Owners POST verified bytes; joiners GET by
-- content-addressed id. Closes G-21 / G-22 by giving the
-- multi-joiner + max_uses surface a real server-side row to
-- back it (V1 in `lakko_contacts::call_invite` embedded the
-- bytes inline in the URL; this migration adds the
-- indirection).
--
-- ## Why a separate table from `invites`
--
-- Despite the close mirror, the two capability schemas have
-- different sizes (call_invite includes a 256–4096-byte
-- embedded `KeyPackage`; contact-invite tops out at 224 bytes)
-- and different lifecycles (contact-invite consumes seed a
-- ContactRequest; call-invite consumes bootstrap an MLS group).
-- Same row schema would force one column to over-allocate or
-- accept a polymorphic shape; separating is cleaner.
--
-- The 3c3 wire format with a 4 KiB KP ceiling tops out at:
--   1 (schema) + 32 (Ed25519) + 16 (account_id) + 16 (call_id)
--   + 8 (expiry) + 4 (max_uses) + 1 (media_flags) + 4 (kp_len)
--   + 4096 (kp_bytes) + 2 (label_len) + 64 (label)
--   + 64 (signature) = 4308 bytes.
-- VARBINARY(8192) gives ~2× headroom for future schema bumps
-- (e.g. a transparency_proof extension parallel to ADR-0005
-- §13.3 for the contact-invite side).

CREATE TABLE IF NOT EXISTS call_invites (
    blob_id           BINARY(32)      NOT NULL PRIMARY KEY
                        COMMENT 'SHA-256 of capability_bytes — content-addressed',
    capability_bytes  VARBINARY(8192) NOT NULL
                        COMMENT 'lakko_contacts::call_invite::CallInviteCapability wire format (3c3)',
    owner_account_id  BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports owner revocation',
    call_id           BINARY(16)      NOT NULL
                        COMMENT 'denormalised from the verified capability — supports lookup by call',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means CALL_NEVER_EXPIRES; supports periodic pruning',
    max_uses          INT UNSIGNED    NOT NULL
                        COMMENT 'denormalised ceiling — 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'atomic counter — bumped by /consume; capped by max_uses',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_call_owner (owner_account_id),
    INDEX idx_call_expiry (expiry_ms),
    INDEX idx_call_id (call_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
