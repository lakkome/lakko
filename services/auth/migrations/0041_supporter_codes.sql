-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0097 section 1 - supporter codes.
--
-- One row per code the operator minted (`lakko-svc-auth supporter-codes
-- mint`). The code itself is never stored: `code_hash` is
-- BLAKE3-keyed(LAKKO_AUTH_SUPPORTER_CODE_PEPPER, 10 code bytes).
--
-- The first redeem stores the one evaluation the code buys: the 30-day
-- `beta-place` epoch it was made in, the blinded element it was made over,
-- the evaluated element and its DLEQ proof. A later redeem in the same
-- epoch with the same blinded element gets the stored answer back; any
-- other blinded element is refused, and a redeem in a later epoch finds the
-- code spent.
--
-- No account column, and nothing here is written by the bind that spends
-- the token: the spend goes to `anon_token_spent` under an identifier the
-- blinding keeps apart from these rows.
CREATE TABLE IF NOT EXISTS supporter_codes (
    code_hash       BINARY(32)      NOT NULL COMMENT 'keyed BLAKE3 of the code',
    created_at      TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT 'when the code was minted',
    epoch           BIGINT UNSIGNED NULL COMMENT 'beta-place epoch of the evaluation; NULL until redeemed',
    blinded_element BINARY(32)      NULL COMMENT 'the blinded element evaluated',
    evaluation      BINARY(32)      NULL COMMENT 'the evaluated element',
    proof           BINARY(64)      NULL COMMENT 'the DLEQ proof',
    PRIMARY KEY (code_hash)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
