-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0078 section 1: the public record of a person in Everyone carries the
-- person's mailbox binding (the mailbox key signed by the account identity
-- key) instead of a mailbox address derived from the identity key, since a
-- mailbox is no longer named after the identity key.
--
-- The introduction payload grew by the 97-byte binding (and the optional
-- `fof` grant by 97 bytes too), so the longest record is now
-- `lakko_contacts::introduction::INTRODUCTION_RECORD_MAX_LEN` (6744) bytes;
-- `MAX_PUBLIC_RECORD_LEN` in `src/visibility.rs` derives it from the codec
-- and a test pins it against this file.
--
-- A flag day: nothing is deployed, so the old records (payload v2, which no
-- client opens any more) are deleted rather than migrated.

DELETE FROM public_records;

ALTER TABLE public_records MODIFY record VARBINARY(6744) NOT NULL;
