-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.1e — Equix proof-of-work for anti-spam.
--
-- The expensive endpoints (/signup/{start,finish}, /login/start,
-- /recovery/login/start) are gated by a PoW: the client first calls
-- POST /pow/challenge to obtain a fresh seed, then runs Equix (Tor
-- Project's memory-hard PoW; ~50-500ms client effort at default
-- difficulty) and submits the solution alongside the request.
--
-- Equix verify is fast (microseconds). The seed is one-shot: the row
-- is deleted on the first successful or failed verification attempt,
-- so a stolen / replayed proof cannot be reused.
--
-- This is layer one of three planned (see ~/.claude/plans/...nested-wreath
-- §4):
--   1. Equix PoW                           — Phase 1.1e (this sprint)
--   2. Privacy Pass tokens (RFC 9576)      — deferred (needs App Attest /
--                                            Play Integrity issuer)
--   3. IP rate limit with rotating salts   — Phase 2 hardening

CREATE TABLE IF NOT EXISTS pow_challenges (
    -- 16 random bytes from OsRng, also serves as a primary key.
    seed         BINARY(16)   NOT NULL PRIMARY KEY,
    -- Number of leading zero bits required on
    -- SHA-256(seed || client_nonce || equix_solution).
    -- Stored per row so the difficulty knob can be raised dynamically
    -- without invalidating in-flight challenges.
    effort_bits  TINYINT UNSIGNED NOT NULL,
    -- Absolute expiry — simpler than TTL-from-created_at because the
    -- verifier can use `WHERE expires_at > NOW(3)` directly.
    expires_at   TIMESTAMP(3) NOT NULL,
    -- Index the pruner deletes by; also useful for ops queries.
    INDEX idx_pow_challenges_expires_at (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
