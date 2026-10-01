-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0059 section 1, co-publisher keys (maintainer, 2026-09-29).
--
-- A co-publisher (ADR-0028 OQ3) cannot derive the owner key, so the owner
-- appoints the co-publisher's own per-channel Ed25519 key here with an
-- owner-signed request; a request signed by an appointed key is accepted
-- where the rotation fanout needs it (the subscriber list). The key is
-- derived from the co-publisher's account seed and the channel id and does
-- not name the account. At most 32 keys per channel (enforced by the service
-- under the channel row's lock). Proof nonces share `channel_owner_nonces`.

CREATE TABLE IF NOT EXISTS channel_copublishers (
    channel_id       BINARY(16)      NOT NULL,
    pubkey           BINARY(32)      NOT NULL,
    appointed_at_ms  BIGINT UNSIGNED NOT NULL,
    PRIMARY KEY (channel_id, pubkey),
    CONSTRAINT fk_channel_copublishers_channel
        FOREIGN KEY (channel_id) REFERENCES channels(id)
        ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
