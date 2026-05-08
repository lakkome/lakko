-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.18d2a — denormalised `max_uses` for server-side
-- enforcement.
--
-- 1.18d1's `0004_invites.sql` shipped `used_count INT UNSIGNED`
-- (zeroed counter, reserved for this sprint) but no
-- corresponding ceiling — server had no way to enforce
-- `max_uses` because the value was buried inside
-- `capability_bytes`. This migration denormalises it onto a
-- typed column the consume handler can compare against in a
-- single atomic UPDATE, mirroring the `expiry_ms`
-- denormalisation in 0004.
--
-- ## Sentinel mapping (matches `lakko_contacts::invite`)
--
--   * `0`           — `SINGLE_USE`. Effective ceiling is 1.
--   * `0xFFFFFFFF`  — unlimited. Effective ceiling is u32::MAX,
--                     i.e. enforcement is a no-op.
--   * any other N   — N consumes allowed.
--
-- The sentinel mapping lives at the SQL layer (CASE expression
-- in the consume handler's UPDATE) rather than at row-write
-- time so the table stays a faithful reflection of the wire
-- format and a future `lakko_contacts` change to the sentinel
-- semantics doesn't require a data migration.
--
-- ## Backfill
--
-- Existing rows from 1.18d1a (if any) get `max_uses = 0`
-- (single-use). Anyone who actually uses such a row before
-- re-uploading from a 1.18d2-aware client will hit the
-- enforced ceiling at 1, which matches the most-conservative
-- read of the original 1.18d1 `max_uses` UI default
-- ("Single-use (default)" in the InvitePanel form).
--
-- Production has no rows yet — 1.18d1 hasn't been deployed —
-- so the backfill is academic. The default exists for
-- developer machines that have run d1 locally.

ALTER TABLE invites
    ADD COLUMN max_uses INT UNSIGNED NOT NULL DEFAULT 0
        COMMENT 'denormalised from capability_bytes — sentinels: 0 = SINGLE_USE (effective 1), 0xFFFFFFFF = unlimited; consume handler maps via CASE'
        AFTER expiry_ms;
