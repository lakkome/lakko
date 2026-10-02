-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- The phone / email one-time-code recovery factor (ADR-0018 factor (d),
-- migration 0013) is removed from the product (maintainer decision,
-- 2026-10-02). Nothing reads or writes these tables any more.
--
-- `otp_challenges` goes first: it is the child side of any reference to the
-- enrolled contacts.

DROP TABLE IF EXISTS otp_challenges;
DROP TABLE IF EXISTS recovery_contacts;
