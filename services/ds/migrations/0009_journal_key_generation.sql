-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0073 section 5 (S4): a journal records the key generation of the last
-- attested key reset that re-rooted it.
--
-- `PUT /ds/group/{hex}/reset` may carry a key-reset attestation minted by
-- `services/auth`; when it verifies under the pinned attest key
-- (`LAKKO_DS_ATTEST_KEY`) and names this journal, a Reset signed by a root
-- that is NOT the current one is heard, provided the attestation's
-- generation is strictly greater than this value. The default 0 makes the
-- first attested reset always win; only an account's DeviceGroup journal
-- ever holds a value above 0 (the attestation names that journal alone).
-- An integer per journal: nothing here names an account (invariant #5).
ALTER TABLE ds_group_owner
    ADD COLUMN key_generation BIGINT UNSIGNED NOT NULL DEFAULT 0;
