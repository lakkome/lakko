-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0058 section 5: who can find me.
--
-- A username binding answers `POST /discovery/lookup` only for a person who
-- chose Everyone; for everyone else a lookup answers exactly as for an
-- unregistered name. Only Everyone publishes name search terms here, each one
-- an OPRF output (the name is never on the server in plaintext, invariant #1).
--
-- Numbered 0023: main carries a 0022 this branch does not have yet.

-- `is_public` rather than `public`: PUBLIC is a keyword in MariaDB's role
-- grammar.
ALTER TABLE usernames
    ADD COLUMN is_public BOOLEAN NOT NULL DEFAULT FALSE;

-- One row per name search term of a person in Everyone
-- (`lakko_discovery_client::name_search_terms`, at most 64 per account). The
-- server learns how many terms a public person has (roughly the name's
-- length) and which public accounts share a term; that is what Everyone
-- means (ADR-0058 Residuals).
CREATE TABLE IF NOT EXISTS display_names (
    oprf_hash   BINARY(64)  NOT NULL,
    account_id  BINARY(16)  NOT NULL,
    created_at  TIMESTAMP   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (oprf_hash, account_id),
    INDEX idx_display_names_account (account_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- The dedicated Ed25519 key discovery signs username certificates with
-- (`lakko_attest::username_cert`). Single row, generated on first boot like
-- the OPRF seed. Its public half is published at
-- `GET /.well-known/lakko-username-cert-key` and pinned in the clients.
CREATE TABLE IF NOT EXISTS username_cert_signing_key (
    id          TINYINT UNSIGNED NOT NULL PRIMARY KEY DEFAULT 1,
    seed        BINARY(32)       NOT NULL,
    created_at  TIMESTAMP        NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT chk_username_cert_signing_key_singleton CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
