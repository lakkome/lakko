-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0060 section 5: a person in Everyone is found with a name.
--
-- Entering Everyone publishes one public record: the section 4 introduction
-- record (`lakko_contacts::introduction::compose_public_record`), keyed per
-- search term over the username and the name terms, carrying no `fof` grant.
-- Discovery keeps it opaque, checks only its structure on upload, and serves
-- it beside each account id from `POST /discovery/lookup` and
-- `POST /discovery/name/lookup`. It is written in the same transaction as the
-- name terms, deleted on leaving Everyone and on `DELETE /discovery/account`.
--
-- `record` is at most `lakko_contacts::introduction::INTRODUCTION_RECORD_MAX_LEN`
-- (6550) bytes; `MAX_PUBLIC_RECORD_LEN` in `src/visibility.rs` derives it from
-- the codec and a test pins it against this file.

CREATE TABLE IF NOT EXISTS public_records (
    account_id  BINARY(16)      NOT NULL PRIMARY KEY,
    record      VARBINARY(6550) NOT NULL,
    updated_at  TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP
                                ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
