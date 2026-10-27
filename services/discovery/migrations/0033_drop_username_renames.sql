-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0076 amendment (2026-10-26): renames are no longer limited to one per
-- 30 days, so the time of an account's last rename is not kept. An account
-- still holds at most one retired name; that rule needs no table.
DROP TABLE IF EXISTS username_renames;
