-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- Calls — a fifth `push_tokens.provider` value for the Apple call token.
--
-- An iOS device holds TWO APNs tokens, not one:
--
--   * the alert token (provider 1) from
--     `registerForRemoteNotifications`, which carries the mutable-alert
--     message wakes the notification-service extension decrypts;
--   * the call token (provider 5) from `PKPushRegistry`, which carries
--     `apns-push-type: voip` pushes — the only push that may launch a
--     TERMINATED app and let it report a call to CallKit before the
--     handler returns (`docs/ux/calls.md` section 2.1).
--
-- They are issued by different system facilities, addressed under
-- different topics, and signed by different Apple credentials; neither
-- can deliver the other's pushes. So they are two rows of one device
-- under the existing `(device_id, provider)` primary key, never one row
-- overwritten — a fresh call token must not evict the alert token.
--
-- ## Why this migration exists at all
--
-- `provider` is an unconstrained TINYINT UNSIGNED, so value 5 was
-- already storable and no data changes. What was wrong was the column
-- COMMENT, which enumerated four values and would have told the next
-- operator reading the schema that a 5 is corruption. The comment is
-- the only documentation of this mapping inside the database, so it is
-- kept accurate rather than left to drift.
--
-- ## The rule that must not be renumbered
--
-- Values 1..4 are live in rows written before calls existed.
-- Renumbering any of them re-points every stored token at the wrong
-- platform; a new kind takes the next free integer and nothing else
-- moves. `ProviderKind::as_db` in `src/provider.rs` pins the same five
-- values, with a test asserting the four originals literally.

ALTER TABLE push_tokens
    MODIFY COLUMN provider TINYINT UNSIGNED NOT NULL
        COMMENT '1 = apns (alert), 2 = fcm, 3 = webpush, 4 = unifiedpush, 5 = apns_voip (call)';
