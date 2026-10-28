-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0078 section 3 - push without a bipartite graph.
--
-- `push_tokens (device_id, provider, token)` and
-- `push_group_subscriptions (device_id, group_id)` held, together, every
-- device's tokens in the clear and one device pseudonym subscribed to all of
-- its groups: the devices x groups graph. Both go. Flag day: there is no
-- deployment and no row worth keeping.
DROP TABLE IF EXISTS push_group_subscriptions;
DROP TABLE IF EXISTS push_tokens;

-- One row per (device, group) subscription. Nothing in a row names a device,
-- and nothing is equal across two groups:
--
--   * `sub_id`   16 random bytes the device keeps so it can replace or remove
--                its own row; a device mints one per subscription.
--   * `push_gid` the group's ADR-0042 routing id.
--   * `wake_pk`  the public half of the group's wake key; a wake is delivered
--                only to rows registered under the key that signed it.
--   * `sealed`   the device's provider tokens AEAD-sealed under the group's
--                unlock key, which push-gw sees only inside a sealed wake and
--                never stores.
--
-- No timestamp column: a device re-registers every subscription when its
-- provider token changes, and equal timestamps across groups would tie the
-- rows to one device. The random primary key keeps insertion order out of
-- the clustered index for the same reason.
CREATE TABLE IF NOT EXISTS push_subscriptions (
    sub_id   BINARY(16)       NOT NULL PRIMARY KEY,
    push_gid BINARY(32)       NOT NULL,
    wake_pk  BINARY(32)       NOT NULL,
    -- `lakko_contacts::push_wake::MAX_PUSH_SUBSCRIPTION_LEN` = 10525.
    sealed   VARBINARY(10525) NOT NULL,
    KEY idx_push_subscriptions_target (push_gid, wake_pk)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- push-gw's X25519 wake key: clients seal every wake to its public half.
-- Single row (`id = 1`), generated at first boot (`src/wake_key.rs`), the
-- pattern of auth's `action_attest_signing_key`.
CREATE TABLE IF NOT EXISTS push_wake_key (
    id     TINYINT UNSIGNED NOT NULL PRIMARY KEY CHECK (id = 1),
    secret BINARY(32)       NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
