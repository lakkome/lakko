-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.6 — Push gateway: bind device pseudonyms to platform push
-- tokens (APNs, FCM, Web Push, UnifiedPush). The server stores the
-- token verbatim because the platform provider expects exactly the
-- bytes it issued; we never look inside.
--
-- A single device may have ONE active token per provider — re-register
-- replaces the old one (clients rotate tokens periodically, e.g. APNs
-- after a backup restore or device migration).
--
-- Payload sent to providers contains ONLY:
--   * device_pseudonym               (32 raw bytes, opaque to provider)
--   * group_pseudonym (optional)     (32 raw bytes)
--   * envelope_pointer (optional)    (u64; the cursor into ds journal)
--
-- No plaintext, no preview, no sender identity. The client wakes up,
-- fetches the ciphertext from `services/ds`, decrypts locally and
-- builds the OS-level notification on-device. Apple / Google / Mozilla
-- never see message content.
--
-- Anti-abuse:
--   * PoW-gated /push/notify (battery-drain attack mitigation)
--   * Server-side per-device throttle is Phase 2 hardening
--   * Proof-of-membership ("Alice can wake Bob only if she's in a
--     group with him") is also Phase 2

CREATE TABLE IF NOT EXISTS push_tokens (
    -- The 32-byte device pseudonym. A device may have multiple rows
    -- in this table — one per provider it speaks to.
    device_id   BINARY(32)     NOT NULL,
    -- Provider tag. Stored as a small TINYINT so the column is fast
    -- to index. See `Provider` enum in src/provider.rs for the mapping.
    provider    TINYINT UNSIGNED NOT NULL
                    COMMENT '1 = apns, 2 = fcm, 3 = webpush, 4 = unifiedpush',
    -- The provider-specific token. APNs device tokens are 32 hex bytes
    -- but FCM tokens can be ~200 chars and Web Push subscription
    -- endpoints can be ~500. VARBINARY(2048) leaves room for all of
    -- them plus future providers.
    token       VARBINARY(2048) NOT NULL,
    created_at  TIMESTAMP      NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMP      NOT NULL DEFAULT CURRENT_TIMESTAMP
                                ON UPDATE CURRENT_TIMESTAMP,
    PRIMARY KEY (device_id, provider),
    -- Reverse index isn't needed in Phase 1 (clients only look up by
    -- device_id), but cheap to keep for ops queries (e.g. "how many
    -- distinct devices does provider X cover").
    INDEX idx_push_tokens_provider (provider)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- PoW challenges — same shape as services/auth and services/discovery.
-- Phase 2 should DRY this into a `lakko-pow` workspace crate.
CREATE TABLE IF NOT EXISTS pow_challenges (
    seed         BINARY(16)   NOT NULL PRIMARY KEY,
    effort_bits  TINYINT UNSIGNED NOT NULL,
    expires_at   TIMESTAMP(3) NOT NULL,
    INDEX idx_pow_challenges_expires_at (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
