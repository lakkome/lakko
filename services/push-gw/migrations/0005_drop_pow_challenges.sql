-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0054 S3 - proof of work is gone from this service: the anonymous routes
-- that paid it now spend an anonymous token (`anon_token_spent`, the
-- migration before this one) and the bearer routes are counted per account.
-- Nothing reads or writes the single-use challenge table any more, so it is
-- dropped. Flag day: there is no deployment and no challenge worth keeping.
DROP TABLE IF EXISTS pow_challenges;
