-- SPDX-License-Identifier: AGPL-3.0-or-later
-- SPDX-FileCopyrightText: 2026 Lakko Contributors

-- ADR-0028 (G-3) — capability-based group join-ticket blob storage.
--
-- Sibling of `0007_call_invites.sql` for `GroupInviteCapability`.
-- An owner/admin POSTs the verified link bytes; a prospective
-- joiner GETs them by content-addressed id (the `/g/<blob_id>`
-- pointer form, the indirection over the inline `/g/<base64url>`
-- link of ADR-0028 Phase 1-3). This row is the server-side
-- anti-abuse ledger: it backs fetch-by-id, the per-joiner
-- consume dedup (0015_group_invite_consumers.sql), expiry
-- pruning, and a SOFT `max_uses` throttle.
--
-- ## Security posture — the ledger is NOT the security boundary
--
-- ADR-0028 puts the security boundary at the admin approval gate
-- (ADR-0027 role-gated, peer-verified): a leaked link lets a
-- stranger *ask* to join, never *in*. So `max_uses` here is a
-- soft anti-spam knob (how many strangers may queue a request),
-- not an access control. It is NOT part of the signed
-- `GroupInviteCapability` wire (which carries only group_id +
-- issuer_pubkey + expiry_ms) — the uploader supplies it in the
-- POST body, and lying about it only changes how many requests
-- reach the admin, who still approves each one individually.
--
-- ## Why a separate table from `call_invites`
--
-- The two capability schemas differ in size and lifecycle: a
-- call-invite embeds a 256-4096-byte KeyPackage and bootstraps a
-- call group; a group-invite is leaner (tops out near 363 bytes:
-- 1 schema + 2 gid_len + 256 gid + 32 issuer_pubkey + 8 expiry +
-- 64 sig) and seeds an external-join proposal. There is no
-- `owner_account_id` to denormalise (the issuer is identified by
-- its Ed25519 pubkey, with no account binding) and no `call_id`.
-- VARBINARY(512) gives ~1.4x headroom over the 363-byte ceiling
-- for a future schema bump.

CREATE TABLE IF NOT EXISTS group_invites (
    blob_id           BINARY(32)     NOT NULL PRIMARY KEY
                        COMMENT 'SHA-256 of capability_bytes — content-addressed',
    capability_bytes  VARBINARY(512) NOT NULL
                        COMMENT 'lakko_contacts::group_invite::GroupInviteCapability wire format (ADR-0028)',
    issuer_pubkey     BINARY(32)     NOT NULL
                        COMMENT 'denormalised Ed25519 issuer key from the verified capability — supports lookup by issuer',
    group_id          VARBINARY(256) NOT NULL
                        COMMENT 'denormalised opaque MLS GroupId from the verified capability — supports lookup by group',
    expiry_ms         BIGINT UNSIGNED NOT NULL
                        COMMENT 'denormalised from the verified capability — 0 means GROUP_INVITE_NEVER_EXPIRES; supports periodic pruning',
    max_uses          INT UNSIGNED    NOT NULL
                        COMMENT 'SOFT throttle (NOT signed, NOT a security boundary) — 0 = single-use sentinel, 0xFFFFFFFF = unlimited',
    used_count        INT UNSIGNED    NOT NULL DEFAULT 0
                        COMMENT 'atomic counter — bumped by /consume; capped by max_uses',
    created_at        TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,

    INDEX idx_group_invite_issuer (issuer_pubkey),
    INDEX idx_group_invite_expiry (expiry_ms),
    INDEX idx_group_invite_gid (group_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
