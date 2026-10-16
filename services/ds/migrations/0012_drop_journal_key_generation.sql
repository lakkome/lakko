-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors
--
-- ADR-0078 section 6 (phase 6): the attested root reset of migration 0009 is
-- gone. A DeviceGroup journal id is derived from the account seed now, so an
-- ADR-0073 key reset founds the DeviceGroup at a fresh journal and claims it
-- plainly; no journal is re-rooted under a key-reset attestation, and the
-- Delivery Service never sees one. The generation a journal recorded for that
-- arm has no reader left.
ALTER TABLE ds_group_owner
    DROP COLUMN key_generation;
