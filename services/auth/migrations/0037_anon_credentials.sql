-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0092 section 4.2: the anonymous credential for channel polls.
--
-- `anon_credential_commitments` holds the one commitment `C = s*G` an account
-- registered with a proof of knowledge of `s` (`lakko-anon-cred`). `s` is
-- derived on the client from the account's lineage seed and never reaches
-- this service; under DDH, `C` does not let auth recognise the tags `s`
-- makes. Replaced only by a key reset or a recall (both delete the row inside
-- their transaction); deleted with the account.
--
-- `anon_credential_issued` records that an account obtained the credential
-- of a period (one row per pair; a repeat in the period re-issues for the
-- same `C`). Rows older than the previous period are pruned hourly.
--
-- Neither table holds key material or PII (invariants #1 and #2): a public
-- group element and two integers.

CREATE TABLE IF NOT EXISTS anon_credential_commitments (
    account_id     BINARY(16)  NOT NULL COMMENT 'FK -> accounts.id',
    commitment     BINARY(32)  NOT NULL COMMENT 'Ristretto255 point s*G, canonical encoding',
    created_at_ms  BIGINT      NOT NULL COMMENT 'unix ms of the registration',
    PRIMARY KEY (account_id),
    CONSTRAINT fk_anon_credential_commitments_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS anon_credential_issued (
    account_id     BINARY(16)      NOT NULL COMMENT 'FK -> accounts.id',
    period         BIGINT UNSIGNED NOT NULL COMMENT 'floor(unix_secs / 2592000)',
    issued_at_ms   BIGINT          NOT NULL COMMENT 'unix ms of the first issuance in the period',
    PRIMARY KEY (account_id, period),
    KEY idx_anon_credential_issued_period (period),
    CONSTRAINT fk_anon_credential_issued_account
        FOREIGN KEY (account_id) REFERENCES accounts (id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
