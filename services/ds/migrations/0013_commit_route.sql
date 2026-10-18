-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0081: an epoch is registered together with the commit row that reaches
-- it. `POST /ds/group/<hex>/commit` registers the epoch's access key and
-- appends the commit row in one transaction, and records here the row's
-- BLAKE3 digest and the id the row was given, so an identical re-send (the
-- same key at that epoch, the same row) appends nothing and answers the
-- original id. A registration made through `PUT .../access` (a claim, a
-- reset, a replace) carries no row: both columns stay NULL, and a later
-- replace of an epoch clears them.
--
-- A digest of an opaque ciphertext and a DS-assigned row id: nothing the DS
-- did not already hold (invariant #2).

ALTER TABLE ds_group_access
    ADD COLUMN commit_row_hash   BINARY(32)      NULL,
    ADD COLUMN commit_message_id BIGINT UNSIGNED NULL;
