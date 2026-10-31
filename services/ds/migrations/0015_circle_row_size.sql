-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0086: a row now holds a card sealed under its search terms, 2,992 bytes
-- at one length. Rows stay opaque here; the column only has to hold them.
ALTER TABLE ds_directory_rows MODIFY row_bytes VARBINARY(4096) NOT NULL;
