-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0065 section 4.1 (S3): the identity bundle records the account's key
-- generation and the key-reset attestation that moved it there.
--
-- `key_generation` is the ADR-0065 generation the published key belongs to:
-- 1 is genesis (every existing row reads 1 - a flag day, no production
-- data), and every attested reset stores the attestation's strictly greater
-- value. It is the floor discovery applies to the two identity writes
-- (`POST /discovery/identity`, `POST /discovery/keypackage`): a session token
-- issued under a lower generation is refused `401 key-generation-superseded`.
--
-- `key_reset_attest` is the 177-byte `KeyResetAttestation` (`lakko-attest`)
-- minted by `services/auth` on `POST /account/keys/reset/finish`. Discovery
-- VERIFIES it on upload (signature under the pinned auth attest key, the
-- account_ref of the session account, the uploaded key, a strictly greater
-- generation) and then stores it opaque, replaced on the next attested reset
-- and never served in phase 1 (ADR-0065 open question 7). It is a public,
-- key-signed artefact, so storing it leaks no secret (invariant #2).
-- `VARBINARY(256)` matches the sibling `genesis_attest` column's headroom.
ALTER TABLE identity_bundles
    ADD COLUMN key_generation BIGINT UNSIGNED NOT NULL DEFAULT 1
        COMMENT 'ADR-0065: the key generation of the published key (1 = genesis); the floor for the two identity writes',
    ADD COLUMN key_reset_attest VARBINARY(256) NULL
        COMMENT 'ADR-0065: the 177B KeyResetAttestation of the last attested reset, verified on upload, stored opaque, never served';
