-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Phase 1.20e — Group fan-out for the push gateway.
--
-- Bridges `(device_pseudonym, group_pseudonym)` so a single
-- `POST /push/notify-group` from the publisher's client fans out
-- wakes to every device subscribed to that group. Without this
-- table the existing `/push/notify` is per-device-only — the
-- publisher would need to know every group member's device
-- pseudonym before posting, which means a separate in-MLS
-- announcement protocol the client doesn't have yet.
--
-- ## Privacy trade-off
--
-- The push-gw now learns "device X reads group G" via the
-- subscription row. This is a real metadata leak vs the §4
-- "social graph not materialised on server" invariant.
--
-- We accept it for the MVP because:
--
--   1. The DS already sees "device X pulls from group G" via
--      cursor-fetch pattern; the leak surface here is the same
--      data point exposed to a sibling service.
--   2. The alternative (every device announces its push
--      pseudonym in an MLS application message at join time;
--      publishers iterate group members + call /push/notify
--      per-device) is a larger wire-format change deferred to
--      Phase 2 hardening.
--
-- Recovery path for Phase 2: replace this table with a hash-
-- based `group_id` derivation (HMAC(group_secret, "push") on
-- the client side) so the push-gw sees opaque values that
-- cross-correlation with the DS journal can't link without the
-- group secret.

CREATE TABLE IF NOT EXISTS push_group_subscriptions (
    -- 32-byte device pseudonym. Same opaque handle the
    -- push_tokens table keys against; the FK below enforces
    -- "you must have at least one token registered before you
    -- can subscribe to groups."
    device_id BINARY(32) NOT NULL,

    -- 32-byte group pseudonym. Matches the same hex form
    -- `services/ds`'s `/ds/group/{group_id_hex}/messages`
    -- path uses; clients hex-encode the same 32-byte buffer.
    -- No FK to a `groups` table because the push-gw has no
    -- such table; groups are an MLS-side concept the push-gw
    -- only sees as a routing pseudonym.
    group_id BINARY(32) NOT NULL,

    -- Insertion timestamp. Surfaces in operator-side debug
    -- queries ("when did this device subscribe?"); not on the
    -- wire.
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,

    -- Composite PK: `(device, group)` pairs are unique. A
    -- re-subscribe by the same device for the same group is
    -- a no-op via INSERT IGNORE or ON DUPLICATE KEY UPDATE
    -- (whichever the handler picks).
    PRIMARY KEY (device_id, group_id)

    -- No FK on device_id → push_tokens(device_id) because
    -- push_tokens's PK is (device_id, provider) — a single
    -- device with two tokens (web + mobile) would have two
    -- rows there, and the FK would require a specific
    -- provider on the subscription. Leaving this enforcement
    -- to the handler keeps the schema flexible.
);

-- Index for the fan-out direction: `WHERE group_id = ?`
-- returns every device subscribed to a group. This is the hot
-- path of `/push/notify-group`.
CREATE INDEX idx_push_group_subscriptions_by_group
    ON push_group_subscriptions (group_id);
