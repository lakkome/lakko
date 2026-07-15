-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0033 — call-admission anti-spam throttle for guest-account link joins.
--
-- ADR-0032 lets an anonymous `/c/<slug>` visitor auto-create a full account to
-- join a call, which opens an account-creation-spam surface: a burst of guest
-- joins on one link mints many accounts. This migration adds the two bookkeeping
-- tables the `POST /call/jwt` admission gate uses to detect + throttle "many
-- brand-new accounts on ONE call link, fast".
--
-- Only admission bookkeeping is stored here — `call_id`, `account_id`, and
-- timestamps. No key material, no message/media content (security invariant #2:
-- the server sees only ciphertext + public artefacts). The throttle is an
-- availability gate on admission and fails OPEN on a store error (see
-- `src/throttle.rs`); it never touches the E2EE path (invariant #4).

-- One row per (call_id, account) admission attempt. The composite PRIMARY KEY
-- makes the windowed distinct-account count natural: re-admission of the same
-- account to the same call upserts `at` rather than adding a row, so a plain
-- `COUNT(*)` over the window counts DISTINCT accounts. `call_id` is stored as
-- the 16 raw bytes (the handler decodes the validated 32-hex string).
CREATE TABLE IF NOT EXISTS call_admission_events (
    call_id    BINARY(16)   NOT NULL COMMENT 'raw 16-byte call_id',
    account_id BINARY(16)   NOT NULL COMMENT 'UUIDv7 account id (raw bytes)',
    at         TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3)
        COMMENT 'last admission attempt for this (call_id, account)',
    PRIMARY KEY (call_id, account_id),
    -- Supports the windowed COUNT(*) WHERE call_id = ? AND at >= ? and the
    -- background prune (DELETE WHERE at < ?).
    INDEX idx_cae_call_at (call_id, at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- A throttled call_id refuses ALL further admissions (429) until `until`
-- expires — the "end the call" response (no new joins; the already-minted
-- short-TTL participant JWTs age out, so the call dies rather than growing).
CREATE TABLE IF NOT EXISTS throttled_calls (
    call_id BINARY(16)   NOT NULL PRIMARY KEY COMMENT 'raw 16-byte call_id',
    until   TIMESTAMP(3) NOT NULL COMMENT 'throttle expiry — refuse all joins until this instant',
    -- Supports the background prune (DELETE WHERE until < NOW(3)).
    INDEX idx_tc_until (until)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
