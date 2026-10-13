-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0074 sprint T5 — a recall whose answer was lost can be asked again.
--
-- `POST /account/recall/finish` changes everything in one transaction and
-- answers with the only credentials the recalling device will ever get: the
-- key-reset attestation, a session pair and a fresh-auth grant. If that
-- answer is lost on the way back (a dropped connection, an app killed in the
-- background), the device holds a new phrase the server has accepted and no
-- way in: the anchor it recalled through is gone, the attempt's state is
-- consumed, and its old identity key authorises nothing any more.
--
-- One row per SUCCESSFUL finish, kept for what is left of the attempt's own
-- lifetime (ten minutes from its `recall/start`). A finish that names the
-- same attempt and carries exactly the same request (`request_digest` is
-- BLAKE3 over every field of it, the signed account statement included) is
-- answered again: the same attestation, and a pair and a grant at the
-- generation the recall set. Anything else about that attempt is the uniform
-- refusal. A replay is honoured only while the account is still at
-- `key_generation`, so it can never undo a later change.
--
-- What is stored: a hash, a public device id, a generation and an
-- attestation the device already holds. No credential, no key material
-- (invariant #2), nothing that names a person (invariant #1).

CREATE TABLE IF NOT EXISTS recall_outcomes (
    session_id     BINARY(16)      NOT NULL PRIMARY KEY COMMENT 'the recall_states id the finish consumed',
    account_id     BINARY(16)      NOT NULL COMMENT 'FK -> accounts.id',
    request_digest BINARY(32)      NOT NULL COMMENT 'BLAKE3 over the whole finish request',
    key_generation BIGINT UNSIGNED NOT NULL COMMENT 'the generation the recall moved the account to',
    device_id      BINARY(32)      NOT NULL COMMENT 'the device the issued pair names',
    attestation    VARBINARY(256)  NOT NULL COMMENT 'the key-reset attestation the finish answered',
    expires_at_ms  BIGINT          NOT NULL COMMENT 'unix ms; the attempt state''s own expiry',
    INDEX idx_recall_outcomes_account (account_id),
    INDEX idx_recall_outcomes_expires (expires_at_ms),
    CONSTRAINT fk_recall_outcomes_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
