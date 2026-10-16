-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0078 section 3 - no push binding.
--
-- `ds_push_bindings` (migration 0006, ADR-0057) stored, per journal,
-- keyed-BLAKE3(LAKKO_DS_BINDING_PEPPER, push_gid). The pepper sat in the one
-- environment file every service reads, so the table was the join between
-- push-gw's subscriptions and the DS journals at rest. A wake is now sealed to
-- push-gw and signed by a key only members of the group hold; the DS forwards
-- it unread and binds it to nothing, and the `push-binding` statement route is
-- gone. Flag day: no row is worth keeping.
DROP TABLE IF EXISTS ds_push_bindings;
